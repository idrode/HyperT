//! Panel de ejecución REAL (paso 7 de Fase 2): órdenes contra el Exchange
//! API de Hyperliquid firmadas con la agent wallet (permiso de TRADING
//! solamente — sin retiro/transferencia, por diseño del protocolo).
//!
//! Redes habilitadas de forma EXPLÍCITA y separada (paso 7.5): testnet con
//! `secrets/agent_testnet.json` y mainnet con `secrets/agent_mainnet.json`
//! (cada archivo lleva su `hyperliquid_chain` y el loader lo verifica — una
//! key de testnet jamás firma contra mainnet). Cualquier otra red se rechaza.
//! En mainnet la confirmación del panel exige además escribir CONFIRMO
//! (dinero real), ver `App::handle_exec_capture`.
//!
//! Hallazgos del SDK pineado (rev aac75585) verificados en su código fuente:
//! - `order`/`bulk_order` (Limit tif Gtc/Ioc + Trigger tpsl "sl"/"tp"),
//!   `cancel` por oid, `update_leverage`. Todo firmado sign_l1_action con el
//!   wallet del cliente (la agent key) — el servidor resuelve la maestra.
//! - `market_open`/`market_close` NO sirven con agent wallet: consultan
//!   `user_state(wallet.address())`, la dirección del AGENT, que no tiene
//!   posiciones (son de la maestra). Las órdenes a mercado se construyen
//!   aquí como IOC agresivas con el mid en vivo del propio TUI.
//! - Los helpers de redondeo del SDK son privados — espejo propio abajo,
//!   con la MISMA regla (5 cifras significativas, máx 6−szDecimals para
//!   perps) y tests. Un precio/tamaño mal redondeado = orden rechazada.

use alloy::signers::local::PrivateKeySigner;
use hyperliquid_rust_sdk::{
    BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ExchangeClient,
    ExchangeDataStatus, ExchangeResponseStatus, InfoClient,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::data::types::DataMsg;
use crate::wallet::agent::LoadedAgent;

/// Deslizamiento máximo de las órdenes "a mercado" (IOC agresiva): 5%, el
/// mismo default del SDK. El precio real de ejecución es el del libro; esto
/// solo acota el peor caso.
const MARKET_SLIPPAGE: f64 = 0.05;

/// Comando del panel de ejecución hacia la tarea del trader. Cada comando
/// lleva TODO lo necesario (mid en vivo, decimales) — la tarea no lee estado
/// de la App.
#[derive(Debug, Clone)]
pub enum TraderCmd {
    /// Abrir posición (entrada mercado o límite) con SL/TP opcionales.
    Open {
        coin: String,
        is_buy: bool,
        lev: u32,
        /// None = mercado (IOC al mid ± slippage); Some = límite Gtc.
        limit_px: Option<f64>,
        sz: f64,
        sz_decimals: u32,
        /// Mid en vivo al confirmar — precio base de la IOC de mercado.
        mid: f64,
        sl: Option<f64>,
        tp: Option<f64>,
    },
    /// Cerrar posición a mercado (reduce-only IOC del tamaño completo).
    Close {
        coin: String,
        /// Tamaño con signo de la posición real (>0 long): decide el lado.
        szi: f64,
        sz_decimals: u32,
        mid: f64,
    },
    /// Cancelar una orden abierta por oid.
    Cancel { coin: String, oid: u64 },
    /// Reemplazar los triggers SL/TP de una posición: cancela los actuales
    /// y coloca los nuevos (reduce-only, tamaño completo de la posición).
    SetTriggers {
        coin: String,
        szi: f64,
        sz_decimals: u32,
        cancel_oids: Vec<u64>,
        sl: Option<f64>,
        tp: Option<f64>,
    },
}

/// Fase/resultado de una acción real, para la línea de estado del panel.
#[derive(Debug, Clone)]
pub enum ExecEvent {
    /// En curso (amarillo/estado): "enviando orden…", "SL/TP colocados…".
    Phase(String),
    /// Acción completada (verde).
    Done(String),
    /// Fallo con el motivo EXACTO que devolvió Hyperliquid (rojo).
    Failed(String),
}

/// Redondeo de tamaño a los decimales del activo (regla de Hyperliquid:
/// szDecimals por activo). Espejo del `round_to_decimals` privado del SDK.
pub fn round_sz(sz: f64, sz_decimals: u32) -> f64 {
    let f = 10f64.powi(sz_decimals as i32);
    (sz * f).round() / f
}

/// Redondeo de precio de perps: 5 cifras significativas Y como máximo
/// 6 − szDecimals decimales (regla documentada de Hyperliquid; espejo del
/// `round_to_significant_and_decimal` privado del SDK, que es lo que este
/// mismo SDK envía en sus órdenes de mercado).
pub fn round_px(px: f64, sz_decimals: u32) -> f64 {
    let max_dec = 6u32.saturating_sub(sz_decimals);
    let abs = px.abs();
    if abs == 0.0 {
        return 0.0;
    }
    let magnitude = abs.log10().floor() as i32;
    let scale = 10f64.powi(5 - magnitude - 1);
    let rounded = (abs * scale).round() / scale;
    let f = 10f64.powi(max_dec as i32);
    (rounded.copysign(px) * f).round() / f
}

/// Precio de la pata "a mercado": mid ± slippage, redondeado. `is_buy`
/// empuja el tope a favor de cruzar el libro, nunca en contra.
fn market_px(mid: f64, is_buy: bool, sz_decimals: u32) -> f64 {
    let factor = if is_buy {
        1.0 + MARKET_SLIPPAGE
    } else {
        1.0 - MARKET_SLIPPAGE
    };
    round_px(mid * factor, sz_decimals)
}

/// Primer status de la respuesta del exchange, aplanado a Result:
/// - Filled/Resting/Success/Waiting* → Ok con el texto humano
/// - Error (interno o de status HTTP-ok) → Err con el motivo exacto
fn flatten_response(resp: ExchangeResponseStatus) -> Result<String, String> {
    match resp {
        ExchangeResponseStatus::Err(e) => Err(e),
        ExchangeResponseStatus::Ok(r) => {
            let Some(st) = r.data.and_then(|d| d.statuses.into_iter().next()) else {
                // sin statuses: acciones tipo updateLeverage responden así
                return Ok("ok".into());
            };
            match st {
                ExchangeDataStatus::Error(e) => Err(e),
                ExchangeDataStatus::Filled(f) => {
                    Ok(format!("llenada {} @ {}", f.total_sz, f.avg_px))
                }
                ExchangeDataStatus::Resting(r) => Ok(format!("descansando (oid {})", r.oid)),
                ExchangeDataStatus::Success => Ok("ok".into()),
                ExchangeDataStatus::WaitingForFill => Ok("esperando fill".into()),
                ExchangeDataStatus::WaitingForTrigger => Ok("esperando trigger".into()),
            }
        }
    }
}

/// Tamaño llenado de la respuesta (para dimensionar los triggers SL/TP con
/// lo que DE VERDAD se llenó, no con lo pedido).
fn filled_sz(resp: &ExchangeResponseStatus) -> Option<f64> {
    if let ExchangeResponseStatus::Ok(r) = resp {
        if let Some(ExchangeDataStatus::Filled(f)) = r.data.as_ref()?.statuses.first() {
            return f.total_sz.parse().ok();
        }
    }
    None
}

/// Request de un trigger SL/TP de cierre: reduce-only, lado contrario a la
/// posición, ejecución a mercado al disparar (tpsl "sl"/"tp"). El limit_px
/// del trigger a mercado es el propio trigger (mismo criterio que la app
/// web de Hyperliquid: el matching aplica su propio tope de slippage).
fn trigger_req(
    coin: &str,
    pos_is_long: bool,
    sz: f64,
    sz_decimals: u32,
    px: f64,
    is_sl: bool,
) -> ClientOrderRequest {
    let px = round_px(px, sz_decimals);
    ClientOrderRequest {
        asset: coin.to_string(),
        is_buy: !pos_is_long,
        reduce_only: true,
        limit_px: px,
        sz: round_sz(sz, sz_decimals),
        cloid: None,
        order_type: ClientOrder::Trigger(hyperliquid_rust_sdk::ClientTrigger {
            is_market: true,
            trigger_px: px,
            tpsl: if is_sl { "sl" } else { "tp" }.to_string(),
        }),
    }
}

/// Lanza la tarea del trader. Solo se arma contra las dos redes habilitadas
/// EXPLÍCITAMENTE (paso 7.5): testnet y mainnet, cada una con su propia agent
/// key por archivo (el caller carga `agent_<red>.json` y el loader verifica
/// el `hyperliquid_chain` del archivo). Cualquier otra red se rechaza.
pub fn spawn(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    rx: UnboundedReceiver<TraderCmd>,
    agent: LoadedAgent,
) {
    let net = match base {
        BaseUrl::Testnet => "testnet",
        BaseUrl::Mainnet => "mainnet",
        _ => {
            let _ = tx.send(DataMsg::Exec(ExecEvent::Failed(
                "trading real deshabilitado en esta red".into(),
            )));
            return;
        }
    };
    tokio::spawn(run(tx, rx, agent, base, net));
}

async fn run(
    tx: UnboundedSender<DataMsg>,
    mut rx: UnboundedReceiver<TraderCmd>,
    agent: LoadedAgent,
    base: BaseUrl,
    net: &'static str,
) {
    // la clave solo vive aquí: en el signer de esta tarea
    let signer: PrivateKeySigner = match alloy_primitives::hex::decode(&agent.priv_hex)
        .ok()
        .and_then(|b| PrivateKeySigner::from_slice(&b).ok())
    {
        Some(s) => s,
        None => {
            let _ = tx.send(DataMsg::Exec(ExecEvent::Failed(
                "agent key ilegible — reautoriza con `a`".into(),
            )));
            return;
        }
    };
    let exchange = loop {
        match ExchangeClient::new(None, signer.clone(), Some(base), None, None).await {
            Ok(c) => break c,
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("trader init: {e}")));
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };
    let info = loop {
        match InfoClient::new(None, Some(base)).await {
            Ok(c) => break c,
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("trader info: {e}")));
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };
    let _ = tx.send(DataMsg::Exec(ExecEvent::Done(format!(
        "trading REAL armado ({net}) · agent {}…",
        &agent.address[..8.min(agent.address.len())]
    ))));

    while let Some(cmd) = rx.recv().await {
        let ev = handle(&exchange, &tx, &cmd).await;
        let _ = tx.send(DataMsg::Exec(match ev {
            Ok(msg) => ExecEvent::Done(msg),
            Err(e) => ExecEvent::Failed(e),
        }));
        refresh(&info, &tx, &agent).await;
    }
}

/// Ejecuta un comando contra el exchange; Ok/Err van a la línea de estado.
async fn handle(
    ex: &ExchangeClient,
    tx: &UnboundedSender<DataMsg>,
    cmd: &TraderCmd,
) -> Result<String, String> {
    let phase = |s: String| {
        let _ = tx.send(DataMsg::Exec(ExecEvent::Phase(s)));
    };
    match cmd {
        TraderCmd::Open {
            coin,
            is_buy,
            lev,
            limit_px,
            sz,
            sz_decimals,
            mid,
            sl,
            tp,
        } => {
            // 1. apalancamiento (aislado — coherente con la liq estimada que
            //    muestra el formulario) ANTES de la orden
            phase(format!("fijando {lev}× aislado en {coin}…"));
            let r = ex
                .update_leverage(*lev, coin, false, None)
                .await
                .map_err(|e| format!("leverage: {e}"))?;
            flatten_response(r).map_err(|e| format!("leverage: {e}"))?;

            // 2. entrada: límite Gtc o "mercado" (IOC agresiva sobre el mid)
            let (px, tif) = match limit_px {
                Some(p) => (round_px(*p, *sz_decimals), "Gtc"),
                None => {
                    if *mid <= 0.0 {
                        return Err("sin mid en vivo para la orden a mercado".into());
                    }
                    (market_px(*mid, *is_buy, *sz_decimals), "Ioc")
                }
            };
            phase(format!(
                "enviando {} {coin}…",
                if *is_buy { "LONG" } else { "SHORT" }
            ));
            let resp = ex
                .order(
                    ClientOrderRequest {
                        asset: coin.clone(),
                        is_buy: *is_buy,
                        reduce_only: false,
                        limit_px: px,
                        sz: round_sz(*sz, *sz_decimals),
                        cloid: None,
                        order_type: ClientOrder::Limit(ClientLimit {
                            tif: tif.to_string(),
                        }),
                    },
                    None,
                )
                .await
                .map_err(|e| format!("orden: {e}"))?;
            let fill = filled_sz(&resp);
            let entry_msg = flatten_response(resp).map_err(|e| format!("orden: {e}"))?;

            // 3. triggers SL/TP: solo con fill real (una límite descansando
            //    no tiene posición que proteger — se editan tras el fill)
            if sl.is_some() || tp.is_some() {
                match fill {
                    Some(f) if f > 0.0 => {
                        phase("colocando SL/TP…".into());
                        let mut reqs = Vec::new();
                        if let Some(p) = sl {
                            reqs.push(trigger_req(coin, *is_buy, f, *sz_decimals, *p, true));
                        }
                        if let Some(p) = tp {
                            reqs.push(trigger_req(coin, *is_buy, f, *sz_decimals, *p, false));
                        }
                        let r = ex
                            .bulk_order(reqs, None)
                            .await
                            .map_err(|e| format!("SL/TP: {e}"))?;
                        flatten_response(r).map_err(|e| {
                            format!("posición ABIERTA pero SL/TP falló: {e} — ponlos con e")
                        })?;
                        return Ok(format!("orden {entry_msg} · SL/TP colocados"));
                    }
                    _ => {
                        return Ok(format!(
                            "orden {entry_msg} · SL/TP NO colocados (sin fill aún — \
                             ponlos con e cuando la posición exista)"
                        ));
                    }
                }
            }
            Ok(format!("orden {entry_msg}"))
        }
        TraderCmd::Close {
            coin,
            szi,
            sz_decimals,
            mid,
        } => {
            if *mid <= 0.0 {
                return Err("sin mid en vivo para cerrar a mercado".into());
            }
            // cerrar un long = vender (y viceversa), reduce-only IOC
            let is_buy = *szi < 0.0;
            phase(format!("cerrando {coin} a mercado…"));
            let resp = ex
                .order(
                    ClientOrderRequest {
                        asset: coin.clone(),
                        is_buy,
                        reduce_only: true,
                        limit_px: market_px(*mid, is_buy, *sz_decimals),
                        sz: round_sz(szi.abs(), *sz_decimals),
                        cloid: None,
                        order_type: ClientOrder::Limit(ClientLimit {
                            tif: "Ioc".to_string(),
                        }),
                    },
                    None,
                )
                .await
                .map_err(|e| format!("cierre: {e}"))?;
            let msg = flatten_response(resp).map_err(|e| format!("cierre: {e}"))?;
            Ok(format!("cierre {msg}"))
        }
        TraderCmd::Cancel { coin, oid } => {
            phase(format!("cancelando orden {oid}…"));
            let resp = ex
                .cancel(
                    ClientCancelRequest {
                        asset: coin.clone(),
                        oid: *oid,
                    },
                    None,
                )
                .await
                .map_err(|e| format!("cancelación: {e}"))?;
            flatten_response(resp).map_err(|e| format!("cancelación: {e}"))?;
            Ok(format!("orden {oid} cancelada"))
        }
        TraderCmd::SetTriggers {
            coin,
            szi,
            sz_decimals,
            cancel_oids,
            sl,
            tp,
        } => {
            if !cancel_oids.is_empty() {
                phase("retirando SL/TP anteriores…".into());
                let cancels = cancel_oids
                    .iter()
                    .map(|oid| ClientCancelRequest {
                        asset: coin.clone(),
                        oid: *oid,
                    })
                    .collect();
                let r = ex
                    .bulk_cancel(cancels, None)
                    .await
                    .map_err(|e| format!("retirando triggers: {e}"))?;
                flatten_response(r).map_err(|e| format!("retirando triggers: {e}"))?;
            }
            let mut reqs = Vec::new();
            let long = *szi >= 0.0;
            if let Some(p) = sl {
                reqs.push(trigger_req(coin, long, szi.abs(), *sz_decimals, *p, true));
            }
            if let Some(p) = tp {
                reqs.push(trigger_req(coin, long, szi.abs(), *sz_decimals, *p, false));
            }
            if reqs.is_empty() {
                return Ok("SL/TP retirados".into());
            }
            phase("colocando SL/TP nuevos…".into());
            let r = ex
                .bulk_order(reqs, None)
                .await
                .map_err(|e| format!("SL/TP: {e}"))?;
            flatten_response(r).map_err(|e| format!("SL/TP: {e}"))?;
            Ok("SL/TP actualizados".into())
        }
    }
}

/// Tras cada acción: re-lee posiciones y órdenes abiertas de la maestra y
/// las emite por los mismos mensajes que los watchers periódicos — el panel
/// se refresca al momento, sin esperar el ciclo de 10s.
async fn refresh(info: &InfoClient, tx: &UnboundedSender<DataMsg>, agent: &LoadedAgent) {
    match info.user_state(agent.master).await {
        Ok(st) => {
            let _ = tx.send(DataMsg::WalletState(crate::data::account_snapshot(
                format!("{}", agent.master),
                &st,
            )));
        }
        Err(e) => {
            let _ = tx.send(DataMsg::RestError(format!("trader refresh: {e}")));
        }
    }
    let client = reqwest::Client::new();
    match crate::data::fetch_open_orders(
        &client,
        hyperliquid_rust_sdk::TESTNET_API_URL,
        &format!("{}", agent.master),
    )
    .await
    {
        Ok(orders) => {
            let _ = tx.send(DataMsg::OpenOrders {
                addr: format!("{}", agent.master),
                orders,
            });
        }
        Err(e) => {
            let _ = tx.send(DataMsg::RestError(format!("trader órdenes: {e}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Redondeo de precio con la regla real de Hyperliquid (5 cifras
    /// significativas, máx 6−szDecimals decimales) — los mismos casos que
    /// valida el SDK en sus tests internos de market_open.
    #[test]
    fn redondeo_de_precio_5_cifras_y_decimales() {
        // BTC (szDecimals 5): precios grandes → 5 cifras significativas
        assert_eq!(round_px(109_354.7, 5), 109_350.0);
        // ETH-ish: 4 decimales máx con szDecimals 2
        assert_eq!(round_px(3_456.78, 2), 3_456.8);
        // precio pequeño: tras las 5 cifras aplica el tope de decimales
        // (6−0=6), igual que el SDK — 0.0012346 se recorta a 0.001235
        assert_eq!(round_px(0.001234567, 0), 0.001235);
        // el slippage de compra empuja hacia arriba, el de venta hacia abajo
        let up = market_px(100.0, true, 2);
        let down = market_px(100.0, false, 2);
        assert!(up > 100.0 && down < 100.0);
        assert_eq!(up, 105.0);
        assert_eq!(down, 95.0);
    }

    #[test]
    fn redondeo_de_tamano_por_decimales_del_activo() {
        assert_eq!(round_sz(0.123456, 4), 0.1235);
        assert_eq!(round_sz(5.0, 0), 5.0);
        assert_eq!(round_sz(1.9999999, 5), 2.0);
    }

    /// Los triggers de cierre van SIEMPRE reduce-only y al lado contrario de
    /// la posición, con el tpsl correcto — un trigger mal armado podría
    /// AUMENTAR la posición en vez de cerrarla.
    #[test]
    fn trigger_de_cierre_reduce_only_y_lado_contrario() {
        let sl = trigger_req("BTC", true, 0.01, 5, 95_000.0, true);
        assert!(sl.reduce_only);
        assert!(!sl.is_buy, "el SL de un long vende");
        match &sl.order_type {
            ClientOrder::Trigger(t) => {
                assert!(t.is_market);
                assert_eq!(t.tpsl, "sl");
                assert_eq!(t.trigger_px, 95_000.0);
            }
            other => panic!("esperaba trigger, hay {other:?}"),
        }
        let tp = trigger_req("BTC", false, 0.01, 5, 90_000.0, false);
        assert!(tp.is_buy, "el TP de un short compra");
        match &tp.order_type {
            ClientOrder::Trigger(t) => assert_eq!(t.tpsl, "tp"),
            other => panic!("esperaba trigger, hay {other:?}"),
        }
    }

    /// La respuesta del exchange se aplana con los errores INTERNOS a la
    /// vista: un statuses[0] = Error con status HTTP ok sigue siendo fallo.
    #[test]
    fn respuesta_aplanada_expone_error_interno() {
        let err: ExchangeResponseStatus = serde_json::from_str(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Order must have minimum value of $10."}]}}}"#,
        )
        .unwrap();
        assert_eq!(
            flatten_response(err),
            Err("Order must have minimum value of $10.".to_string())
        );

        let filled: ExchangeResponseStatus = serde_json::from_str(
            r#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"filled":{"totalSz":"0.01","avgPx":"65000.0","oid":77}}]}}}"#,
        )
        .unwrap();
        assert!(filled_sz(&filled) == Some(0.01));
        assert!(flatten_response(filled).unwrap().contains("llenada"));

        let top: ExchangeResponseStatus = serde_json::from_str(
            r#"{"status":"err","response":"User or API Wallet does not exist."}"#,
        )
        .unwrap();
        assert!(flatten_response(top).is_err());
    }

    /// Sonda REAL contra testnet (necesita red): valida el camino completo
    /// firma-L1 + wire del SDK con una agent key BASURA recién generada — el
    /// servidor debe rechazarla porque esa key no está autorizada por nadie,
    /// SIN tocar ninguna cuenta real.
    /// `cargo test probe_orden_real_testnet -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn probe_orden_real_testnet() {
        let signer = PrivateKeySigner::random();
        let ex = ExchangeClient::new(None, signer, Some(BaseUrl::Testnet), None, None)
            .await
            .unwrap();
        let resp = ex
            .order(
                ClientOrderRequest {
                    asset: "BTC".into(),
                    is_buy: true,
                    reduce_only: false,
                    limit_px: 1000.0, // muy lejos del mercado a propósito
                    sz: 0.001,
                    cloid: None,
                    order_type: ClientOrder::Limit(ClientLimit { tif: "Gtc".into() }),
                },
                None,
            )
            .await
            .unwrap();
        eprintln!("respuesta de testnet a la key basura: {resp:?}");
        // el rechazo DEBE llegar del lado del servidor: si esto fuera Ok, una
        // key sin autorizar estaría operando — parar todo
        assert!(
            flatten_response(resp).is_err(),
            "testnet aceptó una orden de una key sin autorizar"
        );
    }
}
