//! Spike Fase 2: ¿podemos emparejar con MetaMask vía WalletConnect v2 y recibir
//! una firma `personal_sign` desde Rust nativo?
//!
//! El crate `walletconnect-sdk` solo implementa el rol wallet, pero expone
//! públicamente el JWT del relay, el cifrado de envelopes y los tipos del
//! protocolo Sign. Su transporte (HTTP POST a /rpc) ya no existe en el relay
//! oficial (404 en jul-2026), así que aquí el relay se habla por WebSocket —
//! la interfaz documentada que usan todos los wallets. Encima va el rol dApp:
//!
//!   1. symKey aleatoria → topic = sha256(symKey) → URI `wc:` → QR en terminal
//!   2. publicar wc_sessionPropose (tag 1100) en el pairing topic
//!   3. MetaMask escanea, aprueba → responde con su responderPublicKey
//!   4. clave de sesión = HKDF(x25519(nuestra_priv, su_pub)); topic derivado
//!   5. MetaMask envía wc_sessionSettle → lo confirmamos (result: true)
//!   6. publicar wc_sessionRequest personal_sign (tag 1108) → esperar firma
//!   7. verificar la firma recuperando la dirección (EIP-191)
//!
//! No envía ninguna transacción ni toca fondos. `WC_SMOKE=1` corta tras
//! publicar la propuesta (valida relay/auth sin necesitar el móvil).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::hex;
use alloy::primitives::{Address, Signature};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;
use serde_json::{json, Number, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsFrame;
use walletconnect_sdk::message::Message;
use walletconnect_sdk::relay_auth::RelayAuth;
use walletconnect_sdk::types::{
    Id, IrnTag, Metadata, Namespace, Participant, Relay,
    SessionProposeParams, SessionRequestData, SessionRequestMethod,
    SessionRequestObject, SessionRequestParams,
};
use walletconnect_sdk::utils::{
    derive_sym_key, random_bytes32, sha256, unix_timestamp,
};

const RELAY_WS: &str = "wss://relay.walletconnect.org";
/// Project id público que usa el propio SDK en sus ejemplos. Si el relay
/// rate-limita, crear uno propio (gratis) en https://cloud.reown.com y
/// exportar WC_PROJECT_ID.
const FALLBACK_PROJECT_ID: &str = "35d44d49c2dee217a3eb24bb4410acc7";

const CHAIN: &str = "eip155:42161"; // Arbitrum One, la chain de los depósitos a Hyperliquid

fn payload_id() -> u64 {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    ms * 1000 + (rand::random::<u64>() % 1000)
}

fn id_matches(id: &Id, expected: u64) -> bool {
    match id {
        Id::Number(n) => n.as_u64() == Some(expected),
        Id::U128(n) => *n == expected as u128,
        Id::String(s) => s == &expected.to_string(),
    }
}

/// Cliente mínimo del relay por WebSocket: requests JSON-RPC salientes con
/// respuesta correlacionada por id, y push entrante de `irn_subscription`
/// (mensajes cifrados por topic), con ack automático.
struct RelayWs {
    out: mpsc::UnboundedSender<WsFrame>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, Value>>>>>,
    inbox: mpsc::UnboundedReceiver<(String, String)>,
}

impl RelayWs {
    async fn connect(url: &str) -> Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
            .await
            .context("conectando al relay por WebSocket")?;
        let (mut sink, mut stream) = ws.split();

        let (out, mut out_rx) = mpsc::unbounded_channel::<WsFrame>();
        let (inbox_tx, inbox) = mpsc::unbounded_channel::<(String, String)>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, Value>>>>> =
            Arc::new(Mutex::new(HashMap::new()));

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
                        log::error!("WS roto: {e}");
                        break;
                    }
                };
                match frame {
                    WsFrame::Text(text) => {
                        let v: Value = match serde_json::from_str(&text) {
                            Ok(v) => v,
                            Err(e) => {
                                log::warn!("frame no-JSON ignorado: {e}");
                                continue;
                            }
                        };
                        if v["method"] == "irn_subscription" {
                            let topic = v["params"]["data"]["topic"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            let message = v["params"]["data"]["message"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            // ack para que el relay no reintente
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
                        log::error!("relay cerró la conexión: {c:?}");
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self { out, pending, inbox })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = payload_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = json!({"id": id, "jsonrpc": "2.0", "method": method, "params": params});
        self.out
            .send(WsFrame::text(req.to_string()))
            .map_err(|_| anyhow!("WS cerrado"))?;
        let outcome = tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .with_context(|| format!("timeout esperando respuesta de {method}"))?
            .map_err(|_| anyhow!("respuesta de {method} perdida (WS caído)"))?;
        outcome.map_err(|e| anyhow!("el relay devolvió error a {method}: {e}"))
    }

    async fn subscribe(&self, topic: &str) -> Result<()> {
        self.request("irn_subscribe", json!({"topic": topic})).await?;
        Ok(())
    }

    async fn publish<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        topic: &str,
        key: [u8; 32],
        msg: &Message<String, T>,
        tag: IrnTag,
        ttl: u64,
        prompt: bool,
    ) -> Result<()> {
        let cipher = msg
            .encrypt(key, None, None, None)
            .map_err(|e| anyhow!("cifrado: {e:?}"))?;
        self.request(
            "irn_publish",
            json!({
                "topic": topic,
                "message": cipher,
                "ttl": ttl,
                "tag": tag as u16,
                "prompt": prompt,
            }),
        )
        .await?;
        Ok(())
    }

    /// Espera el primer mensaje entrante en `topic` que descifre con `key` y
    /// cumpla `pred`. El resto se descarta (con log).
    async fn wait_for<F>(
        &mut self,
        topic: &str,
        key: [u8; 32],
        timeout: Duration,
        describing: &str,
        mut pred: F,
    ) -> Result<Message<String, Value>>
    where
        F: FnMut(&Message<String, Value>) -> bool,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let (msg_topic, sealed) = tokio::time::timeout_at(deadline, self.inbox.recv())
                .await
                .map_err(|_| anyhow!("timeout esperando {describing} ({}s)", timeout.as_secs()))?
                .ok_or_else(|| anyhow!("conexión WS terminada esperando {describing}"))?;
            if msg_topic != topic {
                log::debug!("mensaje en otro topic ({msg_topic}) ignorado");
                continue;
            }
            match Message::decrypt(&sealed, key, None) {
                Ok(msg) => {
                    if pred(&msg) {
                        return Ok(msg);
                    }
                    log::debug!("mensaje no relevante: {msg:?}");
                }
                Err(e) => log::debug!("mensaje no descifrable (ignorado): {e:?}"),
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let project_id = std::env::var("WC_PROJECT_ID")
        .unwrap_or_else(|_| FALLBACK_PROJECT_ID.to_string());
    let smoke = std::env::var("WC_SMOKE").is_ok();

    let metadata = Metadata {
        name: "hyperT (spike WalletConnect)".to_string(),
        description: "Prueba de firma Fase 2 — Hyperliquid TUI. Solo personal_sign, sin transacciones.".to_string(),
        url: "https://hypert.local".to_string(),
        icons: vec![],
    };

    // Conexión WS al relay, autenticada con el JWT EdDSA del SDK
    let jwt = RelayAuth::new(random_bytes32()).sign_jwt(RELAY_WS);
    let url = format!("{RELAY_WS}/?auth={jwt}&projectId={project_id}&ua=wc-2%2Frust-hypert-spike%2F0.1.0");
    let mut relay = RelayWs::connect(&url).await?;
    println!("Conectado al relay {RELAY_WS} (WebSocket).");

    // 1. Pairing topic + URI
    let pairing_key = random_bytes32();
    let pairing_topic = hex::encode(sha256(pairing_key));
    let dapp_secret = random_bytes32();
    let dapp_pub = hex::encode(x25519_public(dapp_secret));
    let expiry = unix_timestamp().map_err(|e| anyhow!("{e:?}"))? + 300;
    let uri = format!(
        "wc:{pairing_topic}@2?relay-protocol=irn&symKey={}&expiryTimestamp={expiry}",
        hex::encode(pairing_key)
    );

    let qr = QrCode::new(uri.as_bytes()).context("generando QR")?;
    let invert = std::env::var("WC_QR_INVERT").is_ok();
    let (dark, light) = if invert {
        (Dense1x2::Dark, Dense1x2::Light)
    } else {
        // por defecto invertido: en terminal claro-sobre-oscuro esto deja los
        // módulos del QR oscuros sobre fondo claro, que es lo que espera la cámara
        (Dense1x2::Light, Dense1x2::Dark)
    };
    let qr_text = qr.render::<Dense1x2>().dark_color(dark).light_color(light).build();

    println!("\n{qr_text}\n");
    println!("URI (por si prefieres pegarla a mano):\n{uri}\n");
    println!("→ MetaMask móvil: botón de escanear (arriba a la derecha) → apunta al QR.");
    println!("  Si el QR no lee, relanza con WC_QR_INVERT=1.\n");

    // 2. Suscribirse y publicar wc_sessionPropose
    relay.subscribe(&pairing_topic).await?;

    let eip155 = Namespace {
        accounts: None,
        chains: vec![CHAIN.to_string()],
        methods: vec![
            "personal_sign".to_string(),
            "eth_sendTransaction".to_string(),
            "eth_signTypedData_v4".to_string(),
        ],
        events: vec!["chainChanged".to_string(), "accountsChanged".to_string()],
    };
    let propose_id = payload_id();
    let propose = Message {
        jsonrpc: "2.0".to_string(),
        method: Some("wc_sessionPropose".to_string()),
        params: Some(SessionProposeParams {
            required_namespaces: HashMap::from([("eip155".to_string(), eip155)]),
            optional_namespaces: HashMap::new(),
            relays: vec![Relay { protocol: "irn".to_string() }],
            pairing_topic: pairing_topic.clone(),
            proposer: Participant { public_key: dapp_pub.clone(), metadata },
            expiry_timestamp: expiry,
        }),
        result: None,
        error: None,
        id: Id::Number(Number::from(propose_id)),
    };
    relay
        .publish(&pairing_topic, pairing_key, &propose, IrnTag::SessionPropose, 300, true)
        .await?;
    println!("[1/4] Propuesta de sesión publicada en el relay. Esperando aprobación en MetaMask…");

    if smoke {
        println!("WC_SMOKE=1 → fin del smoke test (WS + auth + subscribe + publish OK).");
        return Ok(());
    }

    // 3. Esperar la respuesta de aprobación (result con responderPublicKey)
    let approval = relay
        .wait_for(
            &pairing_topic,
            pairing_key,
            Duration::from_secs(300),
            "la aprobación del pairing",
            |m| id_matches(&m.id, propose_id) && (m.result.is_some() || m.error.is_some()),
        )
        .await?;
    if let Some(err) = &approval.error {
        bail!("MetaMask rechazó la conexión: {err:?}");
    }
    let responder_pub_hex = approval
        .result
        .as_ref()
        .and_then(|r| r.get("responderPublicKey"))
        .and_then(Value::as_str)
        .context("respuesta de aprobación sin responderPublicKey")?
        .to_string();
    println!("[2/4] Pairing aprobado. Derivando clave de sesión…");

    // 4. Clave/topic de sesión y wc_sessionSettle del wallet
    let responder_pub: [u8; 32] = hex::decode_to_array(&responder_pub_hex)
        .map_err(|e| anyhow!("responderPublicKey inválida: {e:?}"))?;
    let session_key = derive_sym_key(dapp_secret, responder_pub);
    let session_topic = hex::encode(sha256(session_key));
    relay.subscribe(&session_topic).await?;

    let settle = relay
        .wait_for(
            &session_topic,
            session_key,
            Duration::from_secs(120),
            "wc_sessionSettle del wallet",
            |m| m.method.as_deref() == Some("wc_sessionSettle"),
        )
        .await?;

    let account_str = settle
        .params
        .as_ref()
        .and_then(|p| p.get("namespaces"))
        .and_then(|n| n.get("eip155"))
        .and_then(|n| n.get("accounts"))
        .and_then(|a| a.get(0))
        .and_then(Value::as_str)
        .context("sessionSettle sin cuentas eip155")?
        .to_string();
    let address: Address = account_str
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("cuenta no parseable: {account_str}"))?;

    // Confirmar el settle (result: true) para que MetaMask dé la sesión por activa
    let ack: Message<String, bool> = Message::result(true, settle.id.clone());
    relay
        .publish(&session_topic, session_key, &ack, IrnTag::SessionSettleResponse, 300, false)
        .await?;
    println!("[3/4] Sesión establecida con {address}. Enviando petición de firma…");

    // 5. personal_sign de prueba
    let text = format!(
        "hyperT spike Fase 2 — mensaje de prueba, sin valor legal ni económico. ts={}",
        unix_timestamp().map_err(|e| anyhow!("{e:?}"))?
    );
    let msg_hex = format!("0x{}", hex::encode(text.as_bytes()));
    let req_id = payload_id();
    let request = Message {
        jsonrpc: "2.0".to_string(),
        method: Some("wc_sessionRequest".to_string()),
        params: Some(SessionRequestParams {
            session_id: None,
            scope: None,
            chain_id: CHAIN.to_string(),
            request: SessionRequestObject {
                method: SessionRequestMethod::PersonalSign,
                params: SessionRequestData::PersonalSign {
                    message: msg_hex,
                    account: address,
                },
                expiry_timestamp: unix_timestamp().map_err(|e| anyhow!("{e:?}"))? + 300,
            },
        }),
        result: None,
        error: None,
        id: Id::Number(Number::from(req_id)),
    };
    relay
        .publish(&session_topic, session_key, &request, IrnTag::SessionRequest, 300, true)
        .await?;
    println!("    Mensaje a firmar: \"{text}\"");
    println!("    Revisa MetaMask y aprueba la firma (es un personal_sign, NO una transacción).");

    let response = relay
        .wait_for(
            &session_topic,
            session_key,
            Duration::from_secs(300),
            "la firma desde MetaMask",
            |m| id_matches(&m.id, req_id) && (m.result.is_some() || m.error.is_some()),
        )
        .await?;
    if let Some(err) = &response.error {
        bail!("MetaMask devolvió error a personal_sign: {err:?}");
    }
    let sig_hex = response
        .result
        .as_ref()
        .and_then(Value::as_str)
        .context("resultado de personal_sign no es un string")?;

    // 6. Verificación EIP-191: la firma debe recuperar la dirección de la sesión
    let signature = Signature::from_str(sig_hex)
        .with_context(|| format!("firma no parseable: {sig_hex}"))?;
    let recovered = signature
        .recover_address_from_msg(text.as_bytes())
        .context("no se pudo recuperar la dirección de la firma")?;

    println!("\n[4/4] Firma recibida: {sig_hex}");
    println!("      Dirección de la sesión : {address}");
    println!("      Dirección recuperada   : {recovered}");
    if recovered == address {
        println!("\n✅ SPIKE OK — la firma vuelve correctamente y verifica contra la cuenta conectada.");
    } else {
        println!("\n❌ La firma llegó pero NO verifica contra la cuenta de la sesión.");
    }
    Ok(())
}

fn x25519_public(secret: [u8; 32]) -> [u8; 32] {
    let secret = x25519_dalek::StaticSecret::from(secret);
    x25519_dalek::PublicKey::from(&secret).to_bytes()
}
