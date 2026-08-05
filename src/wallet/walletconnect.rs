//! WalletConnect v2, rol dApp, sobre el relay oficial por WebSocket.
//!
//! Port a la app del spike validado en `spike/wc_sign` (2026-07-13, ✅ con
//! pairing y personal_sign contra MetaMask móvil). El relay ya solo habla
//! WebSocket (su endpoint HTTP /rpc devuelve 404 desde 2026) y no existe crate
//! nativo con rol dApp, así que transporte y protocolo van a mano; la
//! criptografía está en `wc_crypto`.
//!
//! Flujo: QR de pairing → wc_sessionPropose (tag 1100) → aprobación en
//! MetaMask → clave de sesión x25519+HKDF → wc_sessionSettle del wallet
//! (ack nuestro, tag 1103) → sesión activa (pings/eventos atendidos).
//!
//! Esta sesión es la cuenta MAESTRA de Fase 2. Además de conexión + estado,
//! aquí viven las firmas de cuenta maestra:
//! - Depósito real (Pieza 2): wc_sessionRequest con eth_sendTransaction — un
//!   transfer de USDC al bridge, acreditado a la dirección REMITENTE (ver
//!   data::deposit_route para la verificación del mecanismo).
//! - Retiro (paso 5): wc_sessionRequest con eth_signTypedData_v4 — firma
//!   EIP-712 GASLESS del action `withdraw3` (formato verificado contra el SDK
//!   oficial pineado y la doc exchange-endpoint), que luego se POSTea a
//!   `{api}/exchange`. Hyperliquid envía el USDC a `destination` en Arbitrum
//!   en ~5 min, con $1 de comisión.
//! - Autorización de agent wallet (paso 6): misma mecánica gasless
//!   (eth_signTypedData_v4 del action `approveAgent`, verificado contra el
//!   SDK) — la maestra autoriza una clave local nueva con permiso de trading
//!   y SIN retiro (limitación del propio protocolo para agents). La clave se
//!   persiste vía `wallet::agent` ANTES del POST para no perderla jamás.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy_primitives::Address;
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;
use serde_json::{json, Value};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message as WsFrame;

use super::wc_crypto as crypto;
use crate::data::types::DataMsg;

const RELAY_WS: &str = "wss://relay.walletconnect.org";
/// Project id público (el de los ejemplos de walletconnect-sdk-rs). Si el
/// relay rate-limita: crear uno gratis en https://cloud.reown.com y exportar
/// WC_PROJECT_ID antes de lanzar la app.
const FALLBACK_PROJECT_ID: &str = "35d44d49c2dee217a3eb24bb4410acc7";
/// Vida del QR de pairing (la propuesta caduca a los 5 min, como en el spike).
const PAIRING_TTL: Duration = Duration::from_secs(300);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(120);
const RPC_TIMEOUT: Duration = Duration::from_secs(15);

// Tags IRN del protocolo Sign (specs.walletconnect.com, rpc-methods).
const TAG_SESSION_PROPOSE: u16 = 1100;
const TAG_SESSION_SETTLE_RESPONSE: u16 = 1103;
const TAG_SESSION_REQUEST: u16 = 1108;
const TAG_SESSION_UPDATE_RESPONSE: u16 = 1105;
const TAG_SESSION_EVENT_RESPONSE: u16 = 1111;
const TAG_SESSION_DELETE: u16 = 1112;
const TAG_SESSION_DELETE_RESPONSE: u16 = 1113;
const TAG_SESSION_PING_RESPONSE: u16 = 1115;

/// Tiempo que se le da a MetaMask para aprobar una firma (depósito o retiro).
const SIGN_TTL: Duration = Duration::from_secs(300);
/// Espera máxima del receipt on-chain antes de rendirse (la tx puede seguir
/// viva: el hash queda visible para comprobarla a mano).
const RECEIPT_TIMEOUT: Duration = Duration::from_secs(600);
/// Espera máxima de la llegada del retiro a la wallet (la doc dice ~5 min;
/// margen amplio antes de dejar de vigilar — la solicitud sigue viva).
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(1200);
/// Cadencia del balanceOf mientras se espera la llegada del retiro.
const ARRIVAL_POLL: Duration = Duration::from_secs(10);

/// Comandos de la vista Fondos hacia el gestor de sesión.
#[derive(Debug, Clone)]
pub enum WcCmd {
    Connect,
    Disconnect,
    /// Pieza 2: depósito real — transfer de USDC al bridge, firmado en MetaMask.
    Deposit(DepositReq),
    /// Paso 5: retiro real — firma EIP-712 (gasless) + POST a /exchange.
    Withdraw(WithdrawReq),
    /// Paso 6: autorización de agent wallet — firma EIP-712 (gasless) de la
    /// maestra + POST a /exchange; la clave ya generada viaja en la petición.
    ApproveAgent(AgentReq),
    /// Transferencia interna spot⇄perps (usdClassTransfer) — firma EIP-712
    /// (gasless) de la maestra + POST a /exchange. Sin comisión, instantánea.
    ClassTransfer(TransferReq),
}

/// Petición de transferencia interna spot⇄perps, ya validada por la UI
/// (cantidad > 0 y ≤ disponible del lado origen).
#[derive(Debug, Clone)]
pub struct TransferReq {
    /// Cantidad en USDC solo para mostrar en los estados.
    pub usdc: f64,
    /// La misma cantidad exacta en unidades base (6 decimales).
    pub units: u128,
    /// Cantidad canónica como string decimal — va IDÉNTICA en el typed data
    /// firmado y en el action del POST (el servidor verifica sobre el string).
    pub amount: String,
    /// true = spot → perps; false = perps → spot.
    pub to_perp: bool,
    /// Cuenta maestra (checksummed) — solo para vigilar la llegada por saldos.
    pub master: String,
    /// Base del Exchange API según la red de la sesión (data::withdraw_route).
    pub api: String,
    /// "Mainnet" | "Testnet" — campo hyperliquidChain del action y la firma.
    pub hl_chain: &'static str,
    /// Chain id del dominio EIP-712 (el de la sesión WC: 42161 o 421614).
    pub chain_id: u64,
}

/// Fases de la transferencia interna que pinta la Vista 8 (`DataMsg::Transfer`).
#[derive(Debug, Clone)]
pub enum TransferStatus {
    /// wc_sessionRequest publicado; falta aprobar la firma EIP-712 en MetaMask.
    AwaitingWallet { usdc: f64, to_perp: bool },
    /// Hyperliquid aceptó la transferencia (debería reflejarse en segundos).
    Accepted { usdc: f64, to_perp: bool },
    /// El saldo del lado destino ya subió lo transferido.
    Arrived { usdc: f64, to_perp: bool },
    Failed { error: String },
}

/// Petición de depósito ya validada por la UI (≥ mínimo del bridge, ≤ saldo).
#[derive(Debug, Clone)]
pub struct DepositReq {
    /// Cantidad en USDC solo para mostrar en los estados.
    pub usdc: f64,
    /// La misma cantidad exacta en unidades base del token (6 decimales).
    pub units: u128,
    /// Contrato del USDC nativo: el `to` de la transacción (es un transfer).
    pub token: String,
    /// Bridge2 de Hyperliquid: destinatario del transfer. La cuenta acreditada
    /// es la REMITENTE (la maestra de esta sesión).
    pub bridge: String,
    /// RPC para vigilar el receipt tras la firma.
    pub rpc: String,
}

/// Fases del depósito real que pinta la Vista 8 (`DataMsg::Deposit`).
#[derive(Debug, Clone)]
pub enum DepositStatus {
    /// wc_sessionRequest publicado; falta aprobar la firma en MetaMask.
    AwaitingWallet { usdc: f64 },
    /// Firmada y difundida; esperando el receipt on-chain.
    Submitted { usdc: f64, tx: String },
    /// Receipt OK: Hyperliquid acredita al remitente en <1 min.
    Confirmed { usdc: f64, tx: String },
    Failed { error: String },
}

/// Petición de retiro ya validada por la UI (> comisión, ≤ withdrawable).
#[derive(Debug, Clone)]
pub struct WithdrawReq {
    /// Cantidad en USDC solo para mostrar en los estados.
    pub usdc: f64,
    /// La misma cantidad exacta en unidades base (6 decimales).
    pub units: u128,
    /// Cantidad canónica como string decimal — va IDÉNTICA en el typed data
    /// firmado y en el action del POST (el servidor verifica sobre el string).
    pub amount: String,
    /// Dónde envía Hyperliquid el USDC: la propia cuenta maestra (checksummed).
    pub destination: String,
    /// Base del Exchange API según la red de la sesión (data::withdraw_route).
    pub api: String,
    /// "Mainnet" | "Testnet" — campo hyperliquidChain del action y la firma.
    pub hl_chain: &'static str,
    /// Chain id del dominio EIP-712 (el de la sesión WC: 42161 o 421614).
    pub chain_id: u64,
    /// RPC + contrato USDC para vigilar la llegada a la wallet.
    pub rpc: String,
    pub token: String,
}

/// Petición de autorización de agent wallet (paso 6), ya confirmada en la UI.
/// La clave privada generada viaja aquí SOLO hacia el gestor (para
/// persistirla en disco); nunca se loguea ni se muestra.
#[derive(Debug, Clone)]
pub struct AgentReq {
    /// Dirección pública del agent nuevo (checksummed) — lo que se autoriza.
    pub agent_address: String,
    /// Clave privada del agent en hex. NO imprimir jamás (el derive de Debug
    /// del enum WcCmd no se usa en ningún log de esta app — mantenerlo así).
    pub agent_priv: String,
    /// Cuenta maestra de la sesión (checksummed): quién firma y autoriza.
    pub master: String,
    /// Base del Exchange API según la red de la sesión (data::withdraw_route).
    pub api: String,
    /// "Mainnet" | "Testnet" — campo hyperliquidChain del action y la firma.
    pub hl_chain: &'static str,
    /// Chain id del dominio EIP-712 (el de la sesión WC: 42161 o 421614).
    pub chain_id: u64,
}

/// Fases de la autorización del agent que pinta la Vista 8 (`DataMsg::Agent`).
#[derive(Debug, Clone)]
pub enum AgentStatus {
    /// wc_sessionRequest publicado; falta aprobar la firma EIP-712 en MetaMask.
    AwaitingWallet { agent: String },
    /// /exchange respondió ok y la clave quedó guardada en `path`.
    Accepted { agent: String, path: String },
    /// Además, extraAgents del Info API ya lista el agent — verificación
    /// independiente de que el servidor lo registró.
    Verified { agent: String, path: String },
    /// ok de /exchange y clave guardada, pero extraAgents no lo listó dentro
    /// del plazo — no es un fallo confirmado: la validación definitiva es la
    /// primera orden firmada con la agent key (paso 7).
    Unlisted { agent: String, path: String },
    Failed { error: String },
}

/// Fases del retiro real que pinta la Vista 8 (`DataMsg::Withdraw`).
#[derive(Debug, Clone)]
pub enum WithdrawStatus {
    /// wc_sessionRequest publicado; falta aprobar la firma EIP-712 en MetaMask.
    AwaitingWallet { usdc: f64 },
    /// Hyperliquid aceptó la solicitud; procesa y envía en ~5 min.
    Accepted { usdc: f64 },
    /// El USDC llegó a la wallet on-chain (balanceOf subió lo esperado).
    Arrived { usdc: f64 },
    Failed { error: String },
}

/// Typed data EIP-712 del retiro, byte a byte lo que verifica el servidor:
/// dominio HyperliquidSignTransaction v1 (verifyingContract 0x0, chainId el
/// declarado en signatureChainId) + HyperliquidTransaction:Withdraw. Formato
/// contrastado con `Withdraw3` del SDK pineado (test `typed_data_hash_sdk`).
/// Se envía como STRING JSON (params[1] de eth_signTypedData_v4).
fn withdraw_typed_data(req: &WithdrawReq, time: u64) -> String {
    json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"},
            ],
            "HyperliquidTransaction:Withdraw": [
                {"name": "hyperliquidChain", "type": "string"},
                {"name": "destination", "type": "string"},
                {"name": "amount", "type": "string"},
                {"name": "time", "type": "uint64"},
            ],
        },
        "primaryType": "HyperliquidTransaction:Withdraw",
        "domain": {
            "name": "HyperliquidSignTransaction",
            "version": "1",
            "chainId": req.chain_id,
            "verifyingContract": "0x0000000000000000000000000000000000000000",
        },
        "message": {
            "hyperliquidChain": req.hl_chain,
            "destination": req.destination,
            "amount": req.amount,
            "time": time,
        },
    })
    .to_string()
}

/// Action `withdraw3` del POST a /exchange. Mismos strings EXACTOS que el
/// typed data firmado; signatureChainId en hex como lo serializa el SDK.
fn withdraw_action(req: &WithdrawReq, time: u64) -> Value {
    json!({
        "type": "withdraw3",
        "signatureChainId": format!("0x{:x}", req.chain_id),
        "hyperliquidChain": req.hl_chain,
        "destination": req.destination,
        "amount": req.amount,
        "time": time,
    })
}

/// Typed data EIP-712 de la transferencia interna spot⇄perps. El SDK de Rust
/// pineado NO trae este action firmado por usuario (su `class_transfer` usa
/// el action L1 `spotUser` con firma de connectionId opaco, pensada para una
/// clave en proceso, ilegible en MetaMask) — el formato de `usdClassTransfer`
/// está verificado contra el SDK oficial de Python (signing.py
/// USD_CLASS_TRANSFER_SIGN_TYPES + exchange.py usd_class_transfer,
/// 2026-07-20) y contra la sonda real de testnet. Se envía como STRING JSON
/// (params[1] de eth_signTypedData_v4).
fn transfer_typed_data(req: &TransferReq, nonce: u64) -> String {
    json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"},
            ],
            "HyperliquidTransaction:UsdClassTransfer": [
                {"name": "hyperliquidChain", "type": "string"},
                {"name": "amount", "type": "string"},
                {"name": "toPerp", "type": "bool"},
                {"name": "nonce", "type": "uint64"},
            ],
        },
        "primaryType": "HyperliquidTransaction:UsdClassTransfer",
        "domain": {
            "name": "HyperliquidSignTransaction",
            "version": "1",
            "chainId": req.chain_id,
            "verifyingContract": "0x0000000000000000000000000000000000000000",
        },
        "message": {
            "hyperliquidChain": req.hl_chain,
            "amount": req.amount,
            "toPerp": req.to_perp,
            "nonce": nonce,
        },
    })
    .to_string()
}

/// Action `usdClassTransfer` del POST a /exchange. Mismos valores EXACTOS que
/// el typed data firmado; signatureChainId en hex, como los demás actions
/// firmados por usuario de esta app.
fn transfer_action(req: &TransferReq, nonce: u64) -> Value {
    json!({
        "type": "usdClassTransfer",
        "signatureChainId": format!("0x{:x}", req.chain_id),
        "hyperliquidChain": req.hl_chain,
        "amount": req.amount,
        "toPerp": req.to_perp,
        "nonce": nonce,
    })
}

/// Typed data EIP-712 de la autorización del agent (paso 6). Formato
/// contrastado contra `ApproveAgent` del SDK pineado (test
/// `agent_typed_data_hash_sdk`). `agentName` va como string vacío: es lo que
/// el SDK hashea para su `None` (agent sin nombre — el que se reemplaza al
/// rotar). Se envía como STRING JSON (params[1] de eth_signTypedData_v4).
fn agent_typed_data(req: &AgentReq, nonce: u64) -> String {
    json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"},
            ],
            "HyperliquidTransaction:ApproveAgent": [
                {"name": "hyperliquidChain", "type": "string"},
                {"name": "agentAddress", "type": "address"},
                {"name": "agentName", "type": "string"},
                {"name": "nonce", "type": "uint64"},
            ],
        },
        "primaryType": "HyperliquidTransaction:ApproveAgent",
        "domain": {
            "name": "HyperliquidSignTransaction",
            "version": "1",
            "chainId": req.chain_id,
            "verifyingContract": "0x0000000000000000000000000000000000000000",
        },
        "message": {
            "hyperliquidChain": req.hl_chain,
            "agentAddress": req.agent_address,
            "agentName": "",
            "nonce": nonce,
        },
    })
    .to_string()
}

/// Action `approveAgent` del POST a /exchange, idéntico al que serializa el
/// SDK (test `agent_action_identico_al_sdk`): dirección en hex minúsculas,
/// `agentName` null para el agent sin nombre, signatureChainId en hex.
fn agent_action(req: &AgentReq, nonce: u64) -> Value {
    json!({
        "type": "approveAgent",
        "signatureChainId": format!("0x{:x}", req.chain_id),
        "hyperliquidChain": req.hl_chain,
        "agentAddress": req.agent_address.to_lowercase(),
        "agentName": Value::Null,
        "nonce": nonce,
    })
}

/// Firma hex de 65 bytes de MetaMask → (r, s, v) para el POST, con la `v`
/// normalizada a 27/28 (algunos wallets devuelven 0/1).
fn split_signature(sig: &str) -> Option<(String, String, u64)> {
    let h = sig.strip_prefix("0x")?;
    if h.len() != 130 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u64::from_str_radix(&h[128..], 16).ok()?;
    let v = if v < 27 { v + 27 } else { v };
    if v != 27 && v != 28 {
        return None;
    }
    Some((format!("0x{}", &h[..64]), format!("0x{}", &h[64..128]), v))
}

/// Calldata de `transfer(address,uint256)` (selector 0xa9059cbb).
fn transfer_calldata(to: &str, units: u128) -> String {
    let addr = to.trim_start_matches("0x").to_lowercase();
    format!("0xa9059cbb{addr:0>64}{units:064x}")
}

/// "7.5" → unidades base del USDC (6 decimales), exacto y sin floats.
/// None si no es un decimal válido o trae más de 6 decimales.
pub fn usdc_units(s: &str) -> Option<u128> {
    let s = s.trim();
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if (int.is_empty() && frac.is_empty()) || frac.len() > 6 {
        return None;
    }
    if !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let int: u128 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac: u128 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<6}").parse().ok()?
    };
    int.checked_mul(1_000_000)?.checked_add(frac)
}

/// Unidades base → texto exacto ("7500000" → "7.5"), sin ceros de cola.
pub fn fmt_usdc(units: u128) -> String {
    let (int, frac) = (units / 1_000_000, units % 1_000_000);
    if frac == 0 {
        format!("{int}")
    } else {
        format!("{int}.{}", format!("{frac:06}").trim_end_matches('0'))
    }
}

/// Estado de la conexión WalletConnect que pinta la Vista 8.
#[derive(Debug, Clone)]
pub enum WcStatus {
    Idle,
    Connecting,
    /// QR en pantalla, esperando que MetaMask lo escanee y apruebe.
    WaitingScan {
        uri: String,
        /// QR ya renderizado a media-altura Unicode (una línea por fila).
        qr: String,
        expires_at: Instant,
    },
    /// Aprobado; esperando el wc_sessionSettle del wallet.
    WaitingSettle,
    Connected(WcSession),
    /// Terminal: error, QR caducado o sesión cerrada. `c` reintenta.
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct WcSession {
    /// Dirección de la cuenta maestra (checksummed).
    pub address: String,
    /// CAIP-2, p. ej. "eip155:42161".
    pub chain: String,
    /// Nombre del wallet según su metadata (p. ej. "MetaMask Wallet").
    pub peer: Option<String>,
    pub since: Instant,
    pub session_topic: String,
}

/// Lanza el gestor: duerme hasta recibir `Connect`, mantiene la sesión viva
/// (pings/eventos) y publica cada cambio de estado como `DataMsg::Wc`.
pub fn spawn(tx: UnboundedSender<DataMsg>, cmd_rx: UnboundedReceiver<WcCmd>, chain_id: u64) {
    tokio::spawn(manager(tx, cmd_rx, format!("eip155:{chain_id}")));
}

fn payload_id() -> u64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ms * 1000 + (rand::random::<u64>() % 1000)
}

async fn manager(
    tx: UnboundedSender<DataMsg>,
    mut cmd_rx: UnboundedReceiver<WcCmd>,
    chain: String,
) {
    let mut pending: Option<WcCmd> = None;
    loop {
        let cmd = match pending.take() {
            Some(c) => c,
            None => match cmd_rx.recv().await {
                Some(c) => c,
                None => return,
            },
        };
        match cmd {
            WcCmd::Disconnect => {
                let _ = tx.send(DataMsg::Wc(WcStatus::Idle));
            }
            // sin sesión no hay a quién pedirle la firma
            WcCmd::Deposit(_) => {
                let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
                    error: "sin sesión WalletConnect activa".to_string(),
                }));
            }
            WcCmd::Withdraw(_) => {
                let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed {
                    error: "sin sesión WalletConnect activa".to_string(),
                }));
            }
            WcCmd::ApproveAgent(_) => {
                let _ = tx.send(DataMsg::Agent(AgentStatus::Failed {
                    error: "sin sesión WalletConnect activa".to_string(),
                }));
            }
            WcCmd::ClassTransfer(_) => {
                let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed {
                    error: "sin sesión WalletConnect activa".to_string(),
                }));
            }
            WcCmd::Connect => match run_session(&tx, &mut cmd_rx, &chain).await {
                // un comando interrumpió la sesión: procesarlo en la siguiente vuelta
                Ok(next) => pending = next,
                Err(e) => {
                    let _ = tx.send(DataMsg::Wc(WcStatus::Failed {
                        error: format!("{e:#}"),
                    }));
                }
            },
        }
    }
}

/// Una vida completa de sesión. Devuelve el comando que la interrumpió (si lo
/// hubo); los finales propios (error, delete remoto, WS caído) emiten su
/// estado antes de retornar.
async fn run_session(
    tx: &UnboundedSender<DataMsg>,
    cmd_rx: &mut UnboundedReceiver<WcCmd>,
    chain: &str,
) -> Result<Option<WcCmd>> {
    // establecimiento interrumpible: un Connect nuevo regenera el QR al vuelo
    let mut establish = std::pin::pin!(establish(tx, chain));
    let (mut relay, session) = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                // una firma no puede interrumpir el pairing: sin sesión no hay firma
                Some(WcCmd::Deposit(_)) => {
                    let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
                        error: "la sesión WalletConnect aún no está establecida".to_string(),
                    }));
                }
                Some(WcCmd::Withdraw(_)) => {
                    let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed {
                        error: "la sesión WalletConnect aún no está establecida".to_string(),
                    }));
                }
                Some(WcCmd::ApproveAgent(_)) => {
                    let _ = tx.send(DataMsg::Agent(AgentStatus::Failed {
                        error: "la sesión WalletConnect aún no está establecida".to_string(),
                    }));
                }
                Some(WcCmd::ClassTransfer(_)) => {
                    let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed {
                        error: "la sesión WalletConnect aún no está establecida".to_string(),
                    }));
                }
                cmd => return Ok(cmd),
            },
            r = &mut establish => break r?,
        }
    };

    // firma en vuelo (una a la vez): (id del wc_sessionRequest, qué se firma,
    // tope de espera). Depósito y retiro comparten el mecanismo.
    let mut pending_sig: Option<(u64, Flight, tokio::time::Instant)> = None;

    // sesión viva: atender protocolo y comandos hasta que algo la termine
    loop {
        let sig_deadline = pending_sig.as_ref().map(|(_, _, d)| *d);
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(WcCmd::Deposit(req)) => {
                    if pending_sig.is_some() {
                        let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
                            error: "ya hay una firma pendiente en MetaMask".to_string(),
                        }));
                    } else {
                        pending_sig = request_deposit(&relay, &session, chain, req, tx).await;
                    }
                }
                Some(WcCmd::Withdraw(req)) => {
                    if pending_sig.is_some() {
                        let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed {
                            error: "ya hay una firma pendiente en MetaMask".to_string(),
                        }));
                    } else {
                        pending_sig = request_withdraw(&relay, &session, chain, req, tx).await;
                    }
                }
                Some(WcCmd::ApproveAgent(req)) => {
                    if pending_sig.is_some() {
                        let _ = tx.send(DataMsg::Agent(AgentStatus::Failed {
                            error: "ya hay una firma pendiente en MetaMask".to_string(),
                        }));
                    } else {
                        pending_sig = request_agent(&relay, &session, chain, req, tx).await;
                    }
                }
                Some(WcCmd::ClassTransfer(req)) => {
                    if pending_sig.is_some() {
                        let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed {
                            error: "ya hay una firma pendiente en MetaMask".to_string(),
                        }));
                    } else {
                        pending_sig = request_transfer(&relay, &session, chain, req, tx).await;
                    }
                }
                Some(WcCmd::Disconnect) => {
                    abort_pending_sig(tx, &mut pending_sig);
                    let bye = json!({
                        "id": payload_id(), "jsonrpc": "2.0",
                        "method": "wc_sessionDelete",
                        "params": {"code": 6000, "message": "User disconnected"},
                    });
                    // aviso al wallet para que no muestre la sesión como viva
                    let _ = relay
                        .publish_payload(&session.topic, session.sym_key, &bye, TAG_SESSION_DELETE, 86_400, false)
                        .await;
                    let _ = tx.send(DataMsg::Wc(WcStatus::Idle));
                    return Ok(None);
                }
                cmd => {
                    abort_pending_sig(tx, &mut pending_sig);
                    return Ok(cmd);
                }
            },
            // MetaMask no respondió a la firma dentro del TTL de la petición
            _ = tokio::time::sleep_until(sig_deadline.unwrap_or_else(far_future)),
                if sig_deadline.is_some() =>
            {
                let (_, flight, _) = pending_sig.take().expect("rama activa solo con firma");
                let msg = "MetaMask no respondió a la firma en 5 min — reintenta".to_string();
                flight_failed(tx, &flight, msg);
            }
            item = relay.inbox.recv() => {
                let Some((topic, sealed)) = item else {
                    abort_pending_sig(tx, &mut pending_sig);
                    let _ = tx.send(DataMsg::Wc(WcStatus::Failed {
                        error: "conexión con el relay perdida".to_string(),
                    }));
                    return Ok(None);
                };
                if topic != session.topic {
                    continue; // restos del pairing topic
                }
                let Ok(text) = crypto::decrypt(&sealed, session.sym_key) else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                // respuesta de MetaMask a la firma en vuelo (depósito o retiro)
                if let Some((id, _, _)) = &pending_sig {
                    if v["id"].as_u64() == Some(*id)
                        && (!v["result"].is_null() || !v["error"].is_null())
                    {
                        let (_, flight, _) = pending_sig.take().expect("recién comprobado");
                        match flight {
                            Flight::Dep(req) => finish_deposit(tx, req, &v),
                            Flight::Wd(req, time) => finish_withdraw(tx, req, time, &v),
                            Flight::Agent(req, nonce) => finish_agent(tx, req, nonce, &v),
                            Flight::Xfer(req, nonce) => finish_transfer(tx, req, nonce, &v),
                        }
                        continue;
                    }
                }
                match v["method"].as_str() {
                    Some("wc_sessionPing") => {
                        relay.ack(&session, &v, TAG_SESSION_PING_RESPONSE, 30).await;
                    }
                    Some("wc_sessionEvent") => {
                        relay.ack(&session, &v, TAG_SESSION_EVENT_RESPONSE, 300).await;
                    }
                    Some("wc_sessionUpdate") => {
                        relay.ack(&session, &v, TAG_SESSION_UPDATE_RESPONSE, 300).await;
                    }
                    Some("wc_sessionDelete") => {
                        relay.ack(&session, &v, TAG_SESSION_DELETE_RESPONSE, 300).await;
                        abort_pending_sig(tx, &mut pending_sig);
                        let _ = tx.send(DataMsg::Wc(WcStatus::Failed {
                            error: "sesión cerrada desde el wallet".to_string(),
                        }));
                        return Ok(None);
                    }
                    _ => log::debug!("wc: mensaje de sesión ignorado: {text}"),
                }
            }
        }
    }
}

/// Deadline inerte para la rama deshabilitada del select (nunca se pollea).
fn far_future() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(3600)
}

/// Qué firma hay en vuelo hacia MetaMask. Retiro y agent arrastran su
/// `time`/`nonce`: el action del POST debe llevar EXACTAMENTE el firmado.
enum Flight {
    Dep(DepositReq),
    Wd(WithdrawReq, u64),
    Agent(AgentReq, u64),
    Xfer(TransferReq, u64),
}

/// Emite el fallo por el canal de estado que corresponde a la firma en vuelo.
fn flight_failed(tx: &UnboundedSender<DataMsg>, flight: &Flight, error: String) {
    match flight {
        Flight::Dep(_) => {
            let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed { error }));
        }
        Flight::Wd(..) => {
            let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed { error }));
        }
        Flight::Agent(..) => {
            let _ = tx.send(DataMsg::Agent(AgentStatus::Failed { error }));
        }
        Flight::Xfer(..) => {
            let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed { error }));
        }
    }
}

/// La sesión muere con una firma en vuelo: que la UI no se quede en "esperando".
fn abort_pending_sig(
    tx: &UnboundedSender<DataMsg>,
    pending: &mut Option<(u64, Flight, tokio::time::Instant)>,
) {
    if let Some((_, flight, _)) = pending.take() {
        flight_failed(
            tx,
            &flight,
            "la sesión terminó con la firma pendiente — no se envió nada".to_string(),
        );
    }
}

/// Publica un wc_sessionRequest de firma y devuelve la entrada en vuelo, o
/// None si ni se pudo publicar (el fallo ya queda emitido).
async fn request_signature(
    relay: &RelayWs,
    session: &Session,
    chain: &str,
    method: &str,
    params: Value,
    flight: Flight,
    tx: &UnboundedSender<DataMsg>,
) -> Option<(u64, Flight, tokio::time::Instant)> {
    let id = payload_id();
    let payload = json!({
        "id": id, "jsonrpc": "2.0",
        "method": "wc_sessionRequest",
        "params": {
            "chainId": chain,
            "request": {
                "method": method,
                "params": params,
                "expiryTimestamp": crypto::unix_ts() + SIGN_TTL.as_secs(),
            },
        },
    });
    match relay
        .publish_payload(
            &session.topic,
            session.sym_key,
            &payload,
            TAG_SESSION_REQUEST,
            SIGN_TTL.as_secs(),
            true,
        )
        .await
    {
        Ok(()) => Some((id, flight, tokio::time::Instant::now() + SIGN_TTL)),
        Err(e) => {
            flight_failed(
                tx,
                &flight,
                format!("no se pudo publicar la petición de firma: {e:#}"),
            );
            None
        }
    }
}

/// Publica el eth_sendTransaction del depósito como wc_sessionRequest.
async fn request_deposit(
    relay: &RelayWs,
    session: &Session,
    chain: &str,
    req: DepositReq,
    tx: &UnboundedSender<DataMsg>,
) -> Option<(u64, Flight, tokio::time::Instant)> {
    let params = json!([{
        "from": session.address,
        "to": req.token,
        "value": "0x0",
        "data": transfer_calldata(&req.bridge, req.units),
    }]);
    let usdc = req.usdc;
    let out = request_signature(
        relay,
        session,
        chain,
        "eth_sendTransaction",
        params,
        Flight::Dep(req),
        tx,
    )
    .await;
    if out.is_some() {
        let _ = tx.send(DataMsg::Deposit(DepositStatus::AwaitingWallet { usdc }));
    }
    out
}

/// Publica la firma EIP-712 del retiro (eth_signTypedData_v4, GASLESS — en
/// MetaMask aparece como "Signature request", no como transacción).
async fn request_withdraw(
    relay: &RelayWs,
    session: &Session,
    chain: &str,
    req: WithdrawReq,
    tx: &UnboundedSender<DataMsg>,
) -> Option<(u64, Flight, tokio::time::Instant)> {
    // el nonce nace aquí: lo que se firma es lo que luego se POSTea
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let params = json!([session.address, withdraw_typed_data(&req, time)]);
    let usdc = req.usdc;
    let out = request_signature(
        relay,
        session,
        chain,
        "eth_signTypedData_v4",
        params,
        Flight::Wd(req, time),
        tx,
    )
    .await;
    if out.is_some() {
        let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::AwaitingWallet { usdc }));
    }
    out
}

/// Publica la firma EIP-712 de la autorización del agent (gasless, igual que
/// el retiro: MetaMask muestra "Signature request", no una transacción).
async fn request_agent(
    relay: &RelayWs,
    session: &Session,
    chain: &str,
    req: AgentReq,
    tx: &UnboundedSender<DataMsg>,
) -> Option<(u64, Flight, tokio::time::Instant)> {
    // el nonce nace aquí: lo que se firma es lo que luego se POSTea
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let params = json!([session.address, agent_typed_data(&req, nonce)]);
    let agent = req.agent_address.clone();
    let out = request_signature(
        relay,
        session,
        chain,
        "eth_signTypedData_v4",
        params,
        Flight::Agent(req, nonce),
        tx,
    )
    .await;
    if out.is_some() {
        let _ = tx.send(DataMsg::Agent(AgentStatus::AwaitingWallet { agent }));
    }
    out
}

/// Publica la firma EIP-712 de la transferencia interna spot⇄perps (gasless).
async fn request_transfer(
    relay: &RelayWs,
    session: &Session,
    chain: &str,
    req: TransferReq,
    tx: &UnboundedSender<DataMsg>,
) -> Option<(u64, Flight, tokio::time::Instant)> {
    // el nonce nace aquí: lo que se firma es lo que luego se POSTea
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let params = json!([session.address, transfer_typed_data(&req, nonce)]);
    let (usdc, to_perp) = (req.usdc, req.to_perp);
    let out = request_signature(
        relay,
        session,
        chain,
        "eth_signTypedData_v4",
        params,
        Flight::Xfer(req, nonce),
        tx,
    )
    .await;
    if out.is_some() {
        let _ = tx.send(DataMsg::Transfer(TransferStatus::AwaitingWallet {
            usdc,
            to_perp,
        }));
    }
    out
}

/// Respuesta a la firma EIP-712 de la transferencia: firma → POST a
/// /exchange; error → rechazo del usuario en MetaMask.
fn finish_transfer(tx: &UnboundedSender<DataMsg>, req: TransferReq, nonce: u64, v: &Value) {
    let sig = v["result"].as_str().map(str::to_string);
    match sig.as_deref().and_then(split_signature) {
        Some((r, s, sig_v)) => {
            tokio::spawn(submit_transfer(tx.clone(), req, nonce, r, s, sig_v));
        }
        None => {
            let msg = match sig {
                Some(raw) => format!("firma ilegible de MetaMask: {raw}"),
                None => {
                    let e = v["error"]["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v["error"].to_string());
                    format!("MetaMask no firmó: {e}")
                }
            };
            let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed { error: msg }));
        }
    }
}

/// La transferencia interna es instantánea: margen amplio de sobra para
/// verla reflejada en el saldo destino.
const TRANSFER_ARRIVAL_TIMEOUT: Duration = Duration::from_secs(90);
const TRANSFER_ARRIVAL_POLL: Duration = Duration::from_secs(5);

/// Saldo del lado DESTINO de la transferencia, vía /info (solo lectura):
/// perps → `withdrawable` del clearinghouseState; spot → USDC total del
/// spotClearinghouseState.
async fn fetch_class_balance(
    client: &reqwest::Client,
    api: &str,
    user: &str,
    perp_side: bool,
) -> Option<f64> {
    let body = if perp_side {
        json!({"type": "clearinghouseState", "user": user})
    } else {
        json!({"type": "spotClearinghouseState", "user": user})
    };
    let j: Value = client
        .post(format!("{api}/info"))
        .json(&body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let s = if perp_side {
        j["withdrawable"].as_str()?
    } else {
        j["balances"]
            .as_array()?
            .iter()
            .find(|b| b["coin"].as_str() == Some("USDC"))?["total"]
            .as_str()?
    };
    s.parse().ok()
}

/// POST del action firmado a `{api}/exchange` y, si Hyperliquid lo acepta,
/// vigilancia de la llegada al saldo destino (solo lectura).
async fn submit_transfer(
    tx: UnboundedSender<DataMsg>,
    req: TransferReq,
    nonce: u64,
    r: String,
    s: String,
    v: u64,
) {
    let client = reqwest::Client::new();
    // saldo destino ANTES de enviar, para detectar la llegada por diferencia
    let mut baseline = None;
    for _ in 0..3 {
        match fetch_class_balance(&client, &req.api, &req.master, req.to_perp).await {
            Some(b) => {
                baseline = Some(b);
                break;
            }
            None => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }

    let body = json!({
        "action": transfer_action(&req, nonce),
        "nonce": nonce,
        "signature": {"r": r, "s": s, "v": v},
    });
    let resp = client
        .post(format!("{}/exchange", req.api))
        .json(&body)
        .send()
        .await;
    let outcome = match resp {
        Ok(rp) => match rp.json::<Value>().await {
            Ok(j) if j["status"].as_str() == Some("ok") => Ok(()),
            Ok(j) => Err(format!(
                "Hyperliquid rechazó la transferencia: {}",
                j["response"].as_str().unwrap_or(&j.to_string())
            )),
            Err(e) => Err(format!("respuesta ilegible de /exchange: {e}")),
        },
        Err(e) => Err(format!("no se pudo enviar a /exchange: {e}")),
    };
    if let Err(error) = outcome {
        let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed { error }));
        return;
    }
    let (usdc, to_perp) = (req.usdc, req.to_perp);
    let _ = tx.send(DataMsg::Transfer(TransferStatus::Accepted { usdc, to_perp }));

    let Some(base) = baseline else {
        return; // sin baseline no hay vigilancia honesta — Accepted es terminal
    };
    let expected = req.units as f64 / 1e6 - 1e-6;
    let deadline = Instant::now() + TRANSFER_ARRIVAL_TIMEOUT;
    loop {
        tokio::time::sleep(TRANSFER_ARRIVAL_POLL).await;
        if let Some(b) = fetch_class_balance(&client, &req.api, &req.master, req.to_perp).await {
            if b >= base + expected {
                let _ = tx.send(DataMsg::Transfer(TransferStatus::Arrived { usdc, to_perp }));
                return;
            }
        }
        if Instant::now() > deadline {
            let _ = tx.send(DataMsg::Transfer(TransferStatus::Failed {
                error: "aceptada por Hyperliquid pero sin reflejo en el saldo destino en 90s \
                        — revisa los saldos de arriba a mano"
                    .to_string(),
            }));
            return;
        }
    }
}

/// Respuesta a la firma EIP-712 del agent: firma → persistir y POSTear;
/// error → rechazo del usuario en MetaMask (no se guarda nada).
fn finish_agent(tx: &UnboundedSender<DataMsg>, req: AgentReq, nonce: u64, v: &Value) {
    let sig = v["result"].as_str().map(str::to_string);
    match sig.as_deref().and_then(split_signature) {
        Some((r, s, sig_v)) => {
            tokio::spawn(submit_agent(tx.clone(), req, nonce, r, s, sig_v));
        }
        None => {
            let msg = match sig {
                Some(raw) => format!("firma ilegible de MetaMask: {raw}"),
                None => {
                    let e = v["error"]["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v["error"].to_string());
                    format!("MetaMask no firmó: {e}")
                }
            };
            let _ = tx.send(DataMsg::Agent(AgentStatus::Failed { error: msg }));
        }
    }
}

/// Cuánto se espera a que extraAgents liste el agent recién autorizado.
const AGENT_VERIFY_TIMEOUT: Duration = Duration::from_secs(90);
const AGENT_VERIFY_POLL: Duration = Duration::from_secs(5);

/// Persistencia + POST del action firmado + verificación independiente.
/// Orden pensado para no perder jamás una clave ya autorizada: la clave se
/// escribe en disco (`.pending`) ANTES de enviar la aprobación al servidor,
/// y se promueve a definitiva solo con el ok de /exchange.
async fn submit_agent(
    tx: UnboundedSender<DataMsg>,
    req: AgentReq,
    nonce: u64,
    r: String,
    s: String,
    v: u64,
) {
    use crate::wallet::agent;

    if let Err(e) = agent::save_pending(
        req.hl_chain,
        &req.master,
        &req.agent_address,
        &req.agent_priv,
        nonce,
    ) {
        // sin persistencia garantizada NO se autoriza: una clave solo en
        // memoria se perdería con la app y dejaría un agent inutilizable
        let _ = tx.send(DataMsg::Agent(AgentStatus::Failed {
            error: format!("no se pudo guardar la clave — aprobación NO enviada: {e:#}"),
        }));
        return;
    }

    let client = reqwest::Client::new();
    let body = json!({
        "action": agent_action(&req, nonce),
        "nonce": nonce,
        "signature": {"r": r, "s": s, "v": v},
    });
    let resp = client
        .post(format!("{}/exchange", req.api))
        .json(&body)
        .send()
        .await;
    let outcome = match resp {
        Ok(rp) => match rp.json::<Value>().await {
            Ok(j) if j["status"].as_str() == Some("ok") => Ok(()),
            Ok(j) => Err(format!(
                "Hyperliquid rechazó la autorización: {}",
                j["response"].as_str().unwrap_or(&j.to_string())
            )),
            Err(e) => Err(format!("respuesta ilegible de /exchange: {e}")),
        },
        Err(e) => Err(format!("no se pudo enviar a /exchange: {e}")),
    };
    if let Err(error) = outcome {
        agent::discard_pending(req.hl_chain);
        let _ = tx.send(DataMsg::Agent(AgentStatus::Failed { error }));
        return;
    }
    let path = match agent::promote(req.hl_chain) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            // autorizado en el servidor pero el rename falló: avisar con la
            // ruta pending para que la clave se rescate a mano, nunca se borra
            let _ = tx.send(DataMsg::Agent(AgentStatus::Failed {
                error: format!(
                    "AUTORIZADO en Hyperliquid pero no se pudo renombrar la clave — \
                     está en secrets/*.pending: {e:#}"
                ),
            }));
            return;
        }
    };
    let agent_addr = req.agent_address.clone();
    let _ = tx.send(DataMsg::Agent(AgentStatus::Accepted {
        agent: agent_addr.clone(),
        path: path.clone(),
    }));

    // verificación independiente: el Info API debe listar el agent nuevo
    let deadline = Instant::now() + AGENT_VERIFY_TIMEOUT;
    let want = req.agent_address.to_lowercase();
    loop {
        tokio::time::sleep(AGENT_VERIFY_POLL).await;
        let body = json!({"type": "extraAgents", "user": req.master});
        let listed = match client
            .post(format!("{}/info", req.api))
            .json(&body)
            .send()
            .await
        {
            Ok(rp) => rp.json::<Value>().await.ok().is_some_and(|j| {
                j.as_array().is_some_and(|agents| {
                    agents.iter().any(|a| {
                        a["address"]
                            .as_str()
                            .is_some_and(|x| x.to_lowercase() == want)
                    })
                })
            }),
            Err(_) => false,
        };
        if listed {
            let _ = tx.send(DataMsg::Agent(AgentStatus::Verified {
                agent: agent_addr,
                path,
            }));
            return;
        }
        if Instant::now() > deadline {
            let _ = tx.send(DataMsg::Agent(AgentStatus::Unlisted {
                agent: agent_addr,
                path,
            }));
            return;
        }
    }
}

/// Respuesta al wc_sessionRequest: hash → vigilar el receipt; error → rechazo.
fn finish_deposit(tx: &UnboundedSender<DataMsg>, req: DepositReq, v: &Value) {
    if let Some(txhash) = v["result"].as_str() {
        let _ = tx.send(DataMsg::Deposit(DepositStatus::Submitted {
            usdc: req.usdc,
            tx: txhash.to_string(),
        }));
        tokio::spawn(watch_receipt(
            tx.clone(),
            req.rpc,
            txhash.to_string(),
            req.usdc,
        ));
    } else {
        let msg = v["error"]["message"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| v["error"].to_string());
        let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
            error: format!("MetaMask no firmó: {msg}"),
        }));
    }
}

/// Vigila el receipt del transfer vía eth_getTransactionReceipt (solo lectura)
/// hasta verlo confirmado, revertido, o agotar el tope de espera.
async fn watch_receipt(tx: UnboundedSender<DataMsg>, rpc: String, txhash: String, usdc: f64) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + RECEIPT_TIMEOUT;
    loop {
        tokio::time::sleep(Duration::from_secs(4)).await;
        let body = json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "eth_getTransactionReceipt", "params": [txhash],
        });
        let status = match client.post(&rpc).json(&body).send().await {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["result"]["status"].as_str().map(str::to_string)),
            Err(_) => None,
        };
        match status.as_deref() {
            Some("0x1") => {
                let _ = tx.send(DataMsg::Deposit(DepositStatus::Confirmed { usdc, tx: txhash }));
                return;
            }
            Some("0x0") => {
                let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
                    error: format!("la transacción revirtió on-chain — tx {txhash}"),
                }));
                return;
            }
            _ => {} // aún sin receipt
        }
        if Instant::now() > deadline {
            let _ = tx.send(DataMsg::Deposit(DepositStatus::Failed {
                error: format!("sin receipt tras 10 min — comprueba {txhash} en arbiscan.io"),
            }));
            return;
        }
    }
}

/// Respuesta a la firma EIP-712 del retiro: firma → POST a /exchange y
/// vigilancia de llegada; error → rechazo del usuario en MetaMask.
fn finish_withdraw(tx: &UnboundedSender<DataMsg>, req: WithdrawReq, time: u64, v: &Value) {
    let sig = v["result"].as_str().map(str::to_string);
    match sig.as_deref().and_then(split_signature) {
        Some((r, s, sig_v)) => {
            tokio::spawn(submit_withdraw(tx.clone(), req, time, r, s, sig_v));
        }
        None => {
            let msg = match sig {
                // hubo result pero no parsea como firma de 65 bytes
                Some(raw) => format!("firma ilegible de MetaMask: {raw}"),
                None => {
                    let e = v["error"]["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v["error"].to_string());
                    format!("MetaMask no firmó: {e}")
                }
            };
            let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed { error: msg }));
        }
    }
}

/// POST del action firmado a `{api}/exchange` y, si Hyperliquid lo acepta,
/// vigilancia de la llegada del USDC a la wallet (balanceOf, solo lectura).
async fn submit_withdraw(
    tx: UnboundedSender<DataMsg>,
    req: WithdrawReq,
    time: u64,
    r: String,
    s: String,
    v: u64,
) {
    let client = reqwest::Client::new();
    // saldo base ANTES de enviar, para detectar la llegada por diferencia;
    // si el RPC no responde se sigue sin vigilancia (el retiro no depende de él)
    let dest = req.destination.parse::<Address>().ok();
    let mut baseline = None;
    if let Some(d) = dest {
        for _ in 0..3 {
            match crate::data::fetch_usdc_balance(&client, &req.rpc, &req.token, d).await {
                Ok(b) => {
                    baseline = Some(b);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
    }

    let body = json!({
        "action": withdraw_action(&req, time),
        "nonce": time,
        "signature": {"r": r, "s": s, "v": v},
    });
    let resp = client
        .post(format!("{}/exchange", req.api))
        .json(&body)
        .send()
        .await;
    let outcome = match resp {
        Ok(rp) => match rp.json::<Value>().await {
            Ok(j) if j["status"].as_str() == Some("ok") => Ok(()),
            Ok(j) => Err(format!(
                "Hyperliquid rechazó el retiro: {}",
                j["response"].as_str().unwrap_or(&j.to_string())
            )),
            Err(e) => Err(format!("respuesta ilegible de /exchange: {e}")),
        },
        Err(e) => Err(format!("no se pudo enviar a /exchange: {e}")),
    };
    if let Err(error) = outcome {
        let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed { error }));
        return;
    }
    let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Accepted { usdc: req.usdc }));

    // llegada esperada: cantidad pedida menos la comisión de $1
    let expected =
        req.units.saturating_sub(crate::data::WITHDRAW_FEE_UNITS) as f64 / 1e6 - 1e-6;
    let (Some(d), Some(base)) = (dest, baseline) else {
        return; // sin baseline no hay vigilancia honesta — Accepted es terminal
    };
    let deadline = Instant::now() + ARRIVAL_TIMEOUT;
    loop {
        tokio::time::sleep(ARRIVAL_POLL).await;
        if let Ok(b) = crate::data::fetch_usdc_balance(&client, &req.rpc, &req.token, d).await {
            if b >= base + expected {
                let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Arrived { usdc: req.usdc }));
                return;
            }
        }
        if Instant::now() > deadline {
            let _ = tx.send(DataMsg::Withdraw(WithdrawStatus::Failed {
                error: "aceptado por Hyperliquid pero sin llegada visible en 20 min — \
                        comprueba el saldo on-chain a mano"
                    .to_string(),
            }));
            return;
        }
    }
}

/// Sesión interna (la clave no sale de aquí; `WcSession` es lo publicable).
struct Session {
    sym_key: [u8; 32],
    topic: String,
    /// Cuenta maestra checksummed: el `from` de las transacciones a firmar.
    address: String,
}

async fn establish(tx: &UnboundedSender<DataMsg>, chain: &str) -> Result<(RelayWs, Session)> {
    let _ = tx.send(DataMsg::Wc(WcStatus::Connecting));

    let project_id =
        std::env::var("WC_PROJECT_ID").unwrap_or_else(|_| FALLBACK_PROJECT_ID.to_string());
    let jwt = crypto::sign_relay_jwt(crypto::random_bytes32(), RELAY_WS);
    let url =
        format!("{RELAY_WS}/?auth={jwt}&projectId={project_id}&ua=wc-2%2Frust-hypert%2F0.1.0");
    let mut relay = RelayWs::connect(&url).await?;

    // pairing topic + URI + QR
    let pairing_key = crypto::random_bytes32();
    let pairing_topic = alloy_primitives::hex::encode(crypto::sha256_32(pairing_key));
    let dapp_secret = crypto::random_bytes32();
    let dapp_pub = alloy_primitives::hex::encode(crypto::x25519_public(dapp_secret));
    let expiry_ts = crypto::unix_ts() + PAIRING_TTL.as_secs();
    let uri = format!(
        "wc:{pairing_topic}@2?relay-protocol=irn&symKey={}&expiryTimestamp={expiry_ts}",
        alloy_primitives::hex::encode(pairing_key)
    );
    // mismo render que el spike validado: módulos oscuros = espacios (fondo),
    // claros = bloques; la vista lo pinta blanco-sobre-negro explícito
    let qr = QrCode::new(uri.as_bytes())
        .context("generando el QR de pairing")?
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .build();

    relay.subscribe(&pairing_topic).await?;

    let propose_id = payload_id();
    let propose = json!({
        "id": propose_id, "jsonrpc": "2.0",
        "method": "wc_sessionPropose",
        "params": {
            "requiredNamespaces": {
                "eip155": {
                    "chains": [chain],
                    "methods": ["personal_sign", "eth_sendTransaction", "eth_signTypedData_v4"],
                    "events": ["chainChanged", "accountsChanged"],
                }
            },
            "optionalNamespaces": {},
            "relays": [{"protocol": "irn"}],
            "pairingTopic": pairing_topic,
            "proposer": {
                "publicKey": dapp_pub,
                "metadata": {
                    "name": "hyperT",
                    "description": "Hyperliquid TUI — conexión de cuenta maestra (Fase 2)",
                    "url": "https://hypert.local",
                    "icons": [],
                }
            },
            "expiryTimestamp": expiry_ts,
        }
    });
    relay
        .publish_payload(
            &pairing_topic,
            pairing_key,
            &propose,
            TAG_SESSION_PROPOSE,
            300,
            true,
        )
        .await?;

    let _ = tx.send(DataMsg::Wc(WcStatus::WaitingScan {
        uri,
        qr,
        expires_at: Instant::now() + PAIRING_TTL,
    }));

    // aprobación del pairing (result con responderPublicKey sobre el propose id)
    let approval = relay
        .wait_for(&pairing_topic, pairing_key, PAIRING_TTL, |v| {
            v["id"].as_u64() == Some(propose_id)
                && (!v["result"].is_null() || !v["error"].is_null())
        })
        .await
        .map_err(|_| anyhow!("QR caducado sin aprobar — pulsa c para regenerar"))?;
    if !approval["error"].is_null() {
        bail!("MetaMask rechazó la conexión: {}", approval["error"]);
    }
    let responder_pub_hex = approval["result"]["responderPublicKey"]
        .as_str()
        .context("aprobación sin responderPublicKey")?;
    let _ = tx.send(DataMsg::Wc(WcStatus::WaitingSettle));

    // clave y topic de sesión; el settle llega del wallet por el topic derivado
    let responder_pub: [u8; 32] = alloy_primitives::hex::decode_to_array(responder_pub_hex)
        .map_err(|e| anyhow!("responderPublicKey inválida: {e}"))?;
    let sym_key = crypto::derive_sym_key(dapp_secret, responder_pub);
    let topic = alloy_primitives::hex::encode(crypto::sha256_32(sym_key));
    relay.subscribe(&topic).await?;

    let settle = relay
        .wait_for(&topic, sym_key, SETTLE_TIMEOUT, |v| {
            v["method"].as_str() == Some("wc_sessionSettle")
        })
        .await
        .map_err(|_| anyhow!("el wallet aprobó pero no llegó el sessionSettle"))?;

    let account_str = settle["params"]["namespaces"]["eip155"]["accounts"][0]
        .as_str()
        .context("sessionSettle sin cuentas eip155")?;
    let address: Address = account_str
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .parse()
        .map_err(|_| anyhow!("cuenta no parseable: {account_str}"))?;
    let peer = settle["params"]["controller"]["metadata"]["name"]
        .as_str()
        .map(str::to_string);

    // confirmar el settle para que el wallet dé la sesión por activa
    let ack = json!({"id": settle["id"], "jsonrpc": "2.0", "result": true});
    relay
        .publish_payload(
            &topic,
            sym_key,
            &ack,
            TAG_SESSION_SETTLE_RESPONSE,
            300,
            false,
        )
        .await?;

    let session = Session {
        sym_key,
        topic: topic.clone(),
        address: format!("{address}"),
    };
    let _ = tx.send(DataMsg::Wc(WcStatus::Connected(WcSession {
        address: format!("{address}"),
        chain: chain.to_string(),
        peer,
        since: Instant::now(),
        session_topic: topic,
    })));
    Ok((relay, session))
}

/// Respuestas JSON-RPC en vuelo: id → canal con `result` (Ok) o `error` (Err).
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, Value>>>>>;

/// Cliente mínimo del relay por WebSocket: requests JSON-RPC correlacionados
/// por id, y push entrante de `irn_subscription` con ack automático.
struct RelayWs {
    out: mpsc::UnboundedSender<WsFrame>,
    pending: Pending,
    /// (topic, envelope sellado en base64) de cada mensaje empujado por el relay.
    inbox: mpsc::UnboundedReceiver<(String, String)>,
}

impl RelayWs {
    async fn connect(url: &str) -> Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .context("conectando al relay de WalletConnect")?;
        let (mut sink, mut stream) = ws.split();

        let (out, mut out_rx) = mpsc::unbounded_channel::<WsFrame>();
        let (inbox_tx, inbox) = mpsc::unbounded_channel::<(String, String)>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

        tokio::spawn(async move {
            while let Some(frame) = out_rx.recv().await {
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        });

        let out_reader = out.clone();
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            while let Some(frame) = stream.next().await {
                let frame = match frame {
                    Ok(f) => f,
                    Err(e) => {
                        log::error!("wc: WS roto: {e}");
                        break;
                    }
                };
                match frame {
                    WsFrame::Text(text) => {
                        let v: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                log::warn!("wc: frame no-JSON ignorado: {e}");
                                continue;
                            }
                        };
                        if v["method"] == "irn_subscription" {
                            let data = &v["params"]["data"];
                            let topic = data["topic"].as_str().unwrap_or_default().to_string();
                            let message = data["message"].as_str().unwrap_or_default().to_string();
                            let ack = json!({"id": v["id"], "jsonrpc": "2.0", "result": true});
                            let _ = out_reader.send(WsFrame::text(ack.to_string()));
                            let _ = inbox_tx.send((topic, message));
                        } else if let Some(id) = v["id"].as_u64() {
                            if let Some(tx) = pending_reader.lock().unwrap().remove(&id) {
                                let outcome = if v["error"].is_null() {
                                    Ok(v["result"].clone())
                                } else {
                                    Err(v["error"].clone())
                                };
                                let _ = tx.send(outcome);
                            }
                        }
                    }
                    WsFrame::Ping(data) => {
                        let _ = out_reader.send(WsFrame::Pong(data));
                    }
                    WsFrame::Close(c) => {
                        log::warn!("wc: el relay cerró la conexión: {c:?}");
                        break;
                    }
                    _ => {}
                }
            }
            // al terminar el lector se suelta inbox_tx → la sesión ve el cierre
        });

        Ok(Self {
            out,
            pending,
            inbox,
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = payload_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = json!({"id": id, "jsonrpc": "2.0", "method": method, "params": params});
        self.out
            .send(WsFrame::text(req.to_string()))
            .map_err(|_| anyhow!("WS del relay cerrado"))?;
        let outcome = tokio::time::timeout(RPC_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow!("timeout esperando respuesta de {method}"))?
            .map_err(|_| anyhow!("respuesta de {method} perdida (WS caído)"))?;
        outcome.map_err(|e| anyhow!("el relay devolvió error a {method}: {e}"))
    }

    async fn subscribe(&self, topic: &str) -> Result<()> {
        self.request("irn_subscribe", json!({"topic": topic}))
            .await?;
        Ok(())
    }

    /// Cifra `payload` (tipo 0) con la clave del topic y lo publica.
    async fn publish_payload(
        &self,
        topic: &str,
        key: [u8; 32],
        payload: &Value,
        tag: u16,
        ttl: u64,
        prompt: bool,
    ) -> Result<()> {
        let sealed = crypto::encrypt_type0(key, &payload.to_string())?;
        self.request(
            "irn_publish",
            json!({
                "topic": topic,
                "message": sealed,
                "ttl": ttl,
                "tag": tag,
                "prompt": prompt,
            }),
        )
        .await?;
        Ok(())
    }

    /// Respuesta `result: true` al id de un request entrante del wallet.
    async fn ack(&self, session: &Session, incoming: &Value, tag: u16, ttl: u64) {
        let ack = json!({"id": incoming["id"], "jsonrpc": "2.0", "result": true});
        if let Err(e) = self
            .publish_payload(&session.topic, session.sym_key, &ack, tag, ttl, false)
            .await
        {
            log::warn!("wc: ack fallido (tag {tag}): {e:#}");
        }
    }

    /// Primer mensaje en `topic` que descifre con `key` y cumpla `pred`.
    async fn wait_for<F>(
        &mut self,
        topic: &str,
        key: [u8; 32],
        timeout: Duration,
        mut pred: F,
    ) -> Result<Value>
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let (msg_topic, sealed) = tokio::time::timeout_at(deadline, self.inbox.recv())
                .await
                .map_err(|_| anyhow!("timeout"))?
                .ok_or_else(|| anyhow!("conexión con el relay perdida"))?;
            if msg_topic != topic {
                continue;
            }
            match crypto::decrypt(&sealed, key) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(v) if pred(&v) => return Ok(v),
                    Ok(v) => log::debug!("wc: mensaje no relevante: {v}"),
                    Err(e) => log::debug!("wc: payload no-JSON: {e}"),
                },
                Err(e) => log::debug!("wc: mensaje no descifrable (ignorado): {e:#}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El calldata del transfer es lo ÚNICO que decide a dónde va el dinero:
    /// selector + bridge + cantidad, byte a byte.
    #[test]
    fn calldata_del_transfer_al_bridge() {
        let d = transfer_calldata("0x2Df1c51E09aECF9cacB7bc98cB1742757f163dF7", 7_500_000);
        assert_eq!(
            d,
            "0xa9059cbb\
             0000000000000000000000002df1c51e09aecf9cacb7bc98cb1742757f163df7\
             00000000000000000000000000000000000000000000000000000000007270e0"
                .replace(char::is_whitespace, "")
        );
        assert_eq!(d.len(), 2 + 8 + 64 + 64);
    }

    #[test]
    fn unidades_usdc_exactas() {
        assert_eq!(usdc_units("5"), Some(5_000_000));
        assert_eq!(usdc_units("7.5"), Some(7_500_000));
        assert_eq!(usdc_units("0.000001"), Some(1));
        assert_eq!(usdc_units(".5"), Some(500_000));
        assert_eq!(usdc_units(" 12.34 "), Some(12_340_000));
        assert_eq!(usdc_units(""), None);
        assert_eq!(usdc_units("."), None);
        assert_eq!(usdc_units("1.2345678"), None); // más de 6 decimales
        assert_eq!(usdc_units("1,5"), None);
        assert_eq!(usdc_units("-5"), None);
        assert_eq!(usdc_units("1e6"), None);
    }

    #[test]
    fn formato_de_unidades() {
        assert_eq!(fmt_usdc(5_000_000), "5");
        assert_eq!(fmt_usdc(7_500_000), "7.5");
        assert_eq!(fmt_usdc(1), "0.000001");
        assert_eq!(fmt_usdc(0), "0");
    }

    fn req_de_prueba(chain_id: u64, hl_chain: &'static str) -> WithdrawReq {
        WithdrawReq {
            usdc: 10.0,
            units: 10_000_000,
            amount: "10".into(),
            destination: "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7".into(),
            api: "https://api.hyperliquid-testnet.xyz".into(),
            hl_chain,
            chain_id,
            rpc: "https://sepolia-rollup.arbitrum.io/rpc".into(),
            token: "0x75faf114eafb1bdbe2f0316df893fd58ce46aa4d".into(),
        }
    }

    /// Hashea EL JSON QUE PRODUCE `withdraw_typed_data` según la spec EIP-712
    /// (sin atajos: dominio, type string y campos salen del propio JSON), para
    /// contrastarlo contra el hash del SDK. Si el builder emitiera cualquier
    /// campo/valor distinto, el hash divergiría.
    fn hash_del_typed_data(td: &Value) -> [u8; 32] {
        use alloy_primitives::keccak256;
        let d = &td["domain"];
        let mut enc = Vec::new();
        const DOMAIN_TYPE: &str =
            "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
        enc.extend_from_slice(keccak256(DOMAIN_TYPE).as_slice());
        enc.extend_from_slice(keccak256(d["name"].as_str().unwrap()).as_slice());
        enc.extend_from_slice(keccak256(d["version"].as_str().unwrap()).as_slice());
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&d["chainId"].as_u64().unwrap().to_be_bytes());
        enc.extend_from_slice(&w);
        let vc: Address = d["verifyingContract"].as_str().unwrap().parse().unwrap();
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(vc.as_slice());
        enc.extend_from_slice(&w);
        let domain_sep = keccak256(&enc);

        let pt = td["primaryType"].as_str().unwrap();
        let fields = td["types"][pt].as_array().unwrap();
        let type_str = format!(
            "{pt}({})",
            fields
                .iter()
                .map(|f| format!(
                    "{} {}",
                    f["type"].as_str().unwrap(),
                    f["name"].as_str().unwrap()
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let msg = &td["message"];
        let mut enc = Vec::new();
        enc.extend_from_slice(keccak256(type_str.as_bytes()).as_slice());
        for f in fields {
            let name = f["name"].as_str().unwrap();
            match f["type"].as_str().unwrap() {
                "string" => enc.extend_from_slice(keccak256(msg[name].as_str().unwrap()).as_slice()),
                "uint64" => {
                    let mut w = [0u8; 32];
                    w[24..].copy_from_slice(&msg[name].as_u64().unwrap().to_be_bytes());
                    enc.extend_from_slice(&w);
                }
                "address" => {
                    let a: Address = msg[name].as_str().unwrap().parse().unwrap();
                    let mut w = [0u8; 32];
                    w[12..].copy_from_slice(a.as_slice());
                    enc.extend_from_slice(&w);
                }
                "bool" => {
                    let mut w = [0u8; 32];
                    w[31] = msg[name].as_bool().unwrap() as u8;
                    enc.extend_from_slice(&w);
                }
                t => panic!("tipo sin soporte en el hasher del test: {t}"),
            }
        }
        let struct_hash = keccak256(&enc);

        let mut fin = [0u8; 66];
        fin[0] = 0x19;
        fin[1] = 0x01;
        fin[2..34].copy_from_slice(domain_sep.as_slice());
        fin[34..66].copy_from_slice(struct_hash.as_slice());
        keccak256(fin).0
    }

    /// LA garantía del retiro: lo que MetaMask firmará (nuestro typed data)
    /// produce EXACTAMENTE el mismo hash EIP-712 que el `Withdraw3` del SDK
    /// oficial pineado — es decir, lo mismo que el servidor verificará.
    #[test]
    fn typed_data_hash_sdk() {
        use hyperliquid_rust_sdk::{Eip712, Withdraw3};
        for (chain_id, hl_chain) in [(421_614u64, "Testnet"), (42_161u64, "Mainnet")] {
            let req = req_de_prueba(chain_id, hl_chain);
            let time = 1_716_531_066_415u64;
            let td: Value =
                serde_json::from_str(&withdraw_typed_data(&req, time)).expect("JSON válido");
            let sdk = Withdraw3 {
                signature_chain_id: chain_id,
                hyperliquid_chain: hl_chain.to_string(),
                destination: req.destination.clone(),
                amount: req.amount.clone(),
                time,
            };
            assert_eq!(
                hash_del_typed_data(&td),
                sdk.eip712_signing_hash().0,
                "hash EIP-712 distinto del SDK para {hl_chain}"
            );
        }
    }

    /// El action del POST es EXACTAMENTE el que serializa el SDK (mismo tag,
    /// mismos campos, signatureChainId en hex) — así el servidor reconstruye
    /// el mismo typed data que se firmó.
    #[test]
    fn action_identico_al_sdk() {
        use hyperliquid_rust_sdk::{Actions, Withdraw3};
        let req = req_de_prueba(421_614, "Testnet");
        let time = 1_716_531_066_415u64;
        let sdk = serde_json::to_value(Actions::Withdraw3(Withdraw3 {
            signature_chain_id: 421_614,
            hyperliquid_chain: "Testnet".to_string(),
            destination: req.destination.clone(),
            amount: req.amount.clone(),
            time,
        }))
        .unwrap();
        assert_eq!(withdraw_action(&req, time), sdk);
        assert_eq!(sdk["signatureChainId"].as_str(), Some("0x66eee"));
        assert_eq!(sdk["type"].as_str(), Some("withdraw3"));
    }

    fn agent_req_de_prueba(chain_id: u64, hl_chain: &'static str) -> AgentReq {
        AgentReq {
            agent_address: "0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf".into(),
            agent_priv: "0xnunca-se-firma-en-estos-tests".into(),
            master: "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7".into(),
            api: "https://api.hyperliquid-testnet.xyz".into(),
            hl_chain,
            chain_id,
        }
    }

    /// LA garantía del paso 6: lo que MetaMask firmará (nuestro typed data de
    /// approveAgent) produce EXACTAMENTE el mismo hash EIP-712 que el
    /// `ApproveAgent` del SDK oficial pineado, en ambas redes — incluida la
    /// equivalencia agentName "" (nuestro JSON) ↔ None (el SDK).
    #[test]
    fn agent_typed_data_hash_sdk() {
        use hyperliquid_rust_sdk::{ApproveAgent, Eip712};
        for (chain_id, hl_chain) in [(421_614u64, "Testnet"), (42_161u64, "Mainnet")] {
            let req = agent_req_de_prueba(chain_id, hl_chain);
            let nonce = 1_716_531_066_415u64;
            let td: Value =
                serde_json::from_str(&agent_typed_data(&req, nonce)).expect("JSON válido");
            let sdk = ApproveAgent {
                signature_chain_id: chain_id,
                hyperliquid_chain: hl_chain.to_string(),
                agent_address: req.agent_address.parse().unwrap(),
                agent_name: None,
                nonce,
            };
            assert_eq!(
                hash_del_typed_data(&td),
                sdk.eip712_signing_hash().0,
                "hash EIP-712 distinto del SDK para {hl_chain}"
            );
        }
    }

    /// El action del POST es EXACTAMENTE el que serializa el SDK (tag
    /// `approveAgent`, agentName null, signatureChainId en hex, dirección
    /// en minúsculas como la serde de alloy).
    #[test]
    fn agent_action_identico_al_sdk() {
        use hyperliquid_rust_sdk::{Actions, ApproveAgent};
        let req = agent_req_de_prueba(421_614, "Testnet");
        let nonce = 1_716_531_066_415u64;
        let sdk = serde_json::to_value(Actions::ApproveAgent(ApproveAgent {
            signature_chain_id: 421_614,
            hyperliquid_chain: "Testnet".to_string(),
            agent_address: req.agent_address.parse().unwrap(),
            agent_name: None,
            nonce,
        }))
        .unwrap();
        assert_eq!(agent_action(&req, nonce), sdk);
        assert_eq!(sdk["type"].as_str(), Some("approveAgent"));
        assert!(sdk["agentName"].is_null());
    }

    fn transfer_req_de_prueba(chain_id: u64, hl_chain: &'static str, to_perp: bool) -> TransferReq {
        TransferReq {
            usdc: 12.5,
            units: 12_500_000,
            amount: "12.5".into(),
            to_perp,
            master: "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7".into(),
            api: "https://api.hyperliquid-testnet.xyz".into(),
            hl_chain,
            chain_id,
        }
    }

    /// Hash EIP-712 de referencia de usdClassTransfer, calculado A MANO con
    /// el type string y el orden de campos EXACTOS del SDK oficial de Python
    /// (signing.py USD_CLASS_TRANSFER_SIGN_TYPES, verificado 2026-07-20) —
    /// el SDK de Rust pineado no trae este action firmado por usuario, así
    /// que esta es la contraparte independiente del builder JSON.
    fn hash_usd_class_transfer_ref(
        chain_id: u64,
        hl_chain: &str,
        amount: &str,
        to_perp: bool,
        nonce: u64,
    ) -> [u8; 32] {
        use alloy_primitives::keccak256;
        let mut enc = Vec::new();
        enc.extend_from_slice(
            keccak256(
                "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
            )
            .as_slice(),
        );
        enc.extend_from_slice(keccak256("HyperliquidSignTransaction").as_slice());
        enc.extend_from_slice(keccak256("1").as_slice());
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&chain_id.to_be_bytes());
        enc.extend_from_slice(&w);
        enc.extend_from_slice(&[0u8; 32]); // verifyingContract 0x0
        let domain_sep = keccak256(&enc);

        let mut enc = Vec::new();
        enc.extend_from_slice(
            keccak256(
                "HyperliquidTransaction:UsdClassTransfer(string hyperliquidChain,string amount,bool toPerp,uint64 nonce)",
            )
            .as_slice(),
        );
        enc.extend_from_slice(keccak256(hl_chain).as_slice());
        enc.extend_from_slice(keccak256(amount).as_slice());
        let mut w = [0u8; 32];
        w[31] = to_perp as u8;
        enc.extend_from_slice(&w);
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&nonce.to_be_bytes());
        enc.extend_from_slice(&w);
        let struct_hash = keccak256(&enc);

        let mut fin = [0u8; 66];
        fin[0] = 0x19;
        fin[1] = 0x01;
        fin[2..34].copy_from_slice(domain_sep.as_slice());
        fin[34..66].copy_from_slice(struct_hash.as_slice());
        keccak256(fin).0
    }

    /// LA garantía de la transferencia: el typed data que firmará MetaMask
    /// produce el mismo hash que la referencia byte a byte del formato del
    /// SDK de Python, en ambas redes y en ambas direcciones (el bool cambia
    /// el hash — si toPerp no se codificara, este test lo cazaría).
    #[test]
    fn transfer_typed_data_hash_referencia() {
        let nonce = 1_716_531_066_415u64;
        for (chain_id, hl_chain) in [(421_614u64, "Testnet"), (42_161u64, "Mainnet")] {
            for to_perp in [true, false] {
                let req = transfer_req_de_prueba(chain_id, hl_chain, to_perp);
                let td: Value =
                    serde_json::from_str(&transfer_typed_data(&req, nonce)).expect("JSON válido");
                assert_eq!(
                    hash_del_typed_data(&td),
                    hash_usd_class_transfer_ref(chain_id, hl_chain, "12.5", to_perp, nonce),
                    "hash distinto de la referencia para {hl_chain} toPerp={to_perp}"
                );
            }
        }
        // sanity: las dos direcciones NO comparten hash
        let req = transfer_req_de_prueba(421_614, "Testnet", true);
        let td: Value = serde_json::from_str(&transfer_typed_data(&req, nonce)).unwrap();
        assert_ne!(
            hash_del_typed_data(&td),
            hash_usd_class_transfer_ref(421_614, "Testnet", "12.5", false, nonce),
        );
    }

    /// El action del POST lleva exactamente los campos documentados (SDK de
    /// Python exchange.py): type usdClassTransfer, amount como string, toPerp
    /// como bool JSON, nonce numérico y signatureChainId en hex.
    #[test]
    fn transfer_action_formato_documentado() {
        let req = transfer_req_de_prueba(421_614, "Testnet", true);
        let nonce = 1_716_531_066_415u64;
        assert_eq!(
            transfer_action(&req, nonce),
            json!({
                "type": "usdClassTransfer",
                "signatureChainId": "0x66eee",
                "hyperliquidChain": "Testnet",
                "amount": "12.5",
                "toPerp": true,
                "nonce": 1_716_531_066_415u64,
            })
        );
    }

    /// Sonda REAL contra /exchange de TESTNET del action usdClassTransfer con
    /// firma inválida a propósito — mismo criterio que la sonda del agent:
    /// "Unable to recover signer" = wire correcto hasta la verificación de
    /// firma, sin efectos en el servidor.
    /// `cargo test probe_transfer_wire_testnet -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn probe_transfer_wire_testnet() {
        let req = transfer_req_de_prueba(421_614, "Testnet", true);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let body = json!({
            "action": transfer_action(&req, nonce),
            "nonce": nonce,
            "signature": {
                "r": format!("0x{}", "ff".repeat(32)),
                "s": format!("0x{}", "22".repeat(32)),
                "v": 27,
            },
        });
        let j: Value = reqwest::Client::new()
            .post(format!("{}/exchange", req.api))
            .json(&body)
            .send()
            .await
            .expect("POST a testnet")
            .json()
            .await
            .expect("respuesta JSON");
        println!("respuesta de testnet: {j}");
        assert_eq!(j["status"].as_str(), Some("err"), "esperaba err, no ok: {j}");
        let resp = j["response"].as_str().unwrap_or_default();
        assert!(
            resp.contains("Unable to recover signer") || resp.contains("does not exist"),
            "respuesta inesperada (¿formato del action mal?): {resp}"
        );
    }

    /// Sonda REAL contra /exchange de TESTNET con firma inválida a propósito
    /// (r fuera del orden de la curva → ecrecover no puede recuperar nada):
    /// "Unable to recover signer" prueba que el wire (action approveAgent +
    /// nonce + signature) llega bien hasta la verificación de firma. Sin
    /// efectos: ninguna firma válida, ningún estado cambia en el servidor.
    /// `cargo test probe_agent_wire_testnet -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn probe_agent_wire_testnet() {
        let req = agent_req_de_prueba(421_614, "Testnet");
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let body = json!({
            "action": agent_action(&req, nonce),
            "nonce": nonce,
            "signature": {
                "r": format!("0x{}", "ff".repeat(32)),
                "s": format!("0x{}", "22".repeat(32)),
                "v": 27,
            },
        });
        let j: Value = reqwest::Client::new()
            .post(format!("{}/exchange", req.api))
            .json(&body)
            .send()
            .await
            .expect("POST a testnet")
            .json()
            .await
            .expect("respuesta JSON");
        println!("respuesta de testnet: {j}");
        assert_eq!(j["status"].as_str(), Some("err"), "esperaba err, no ok: {j}");
        let resp = j["response"].as_str().unwrap_or_default();
        // recuperación imposible (lo esperado) o, si algún día recuperara una
        // dirección basura, cuenta inexistente — ambas prueban el formato OK
        assert!(
            resp.contains("Unable to recover signer") || resp.contains("does not exist"),
            "respuesta inesperada (¿formato del action mal?): {resp}"
        );
    }

    /// Troceo de la firma de MetaMask, con `v` 0/1 normalizada a 27/28.
    #[test]
    fn troceo_de_firma() {
        let r_hex = "11".repeat(32);
        let s_hex = "22".repeat(32);
        for (v_in, v_out) in [("00", 27u64), ("01", 28), ("1b", 27), ("1c", 28)] {
            let sig = format!("0x{r_hex}{s_hex}{v_in}");
            let (r, s, v) = split_signature(&sig).expect("firma válida");
            assert_eq!(r, format!("0x{r_hex}"));
            assert_eq!(s, format!("0x{s_hex}"));
            assert_eq!(v, v_out);
        }
        assert!(split_signature("0x1234").is_none()); // corta
        assert!(split_signature(&format!("0x{}xx", "11".repeat(32))).is_none());
        // v fuera de rango (2..26 o >28) no es una firma Ethereum válida
        assert!(split_signature(&format!("0x{r_hex}{s_hex}05")).is_none());
    }
}
