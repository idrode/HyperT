//! Backfill opcional del historial de OI y del delta por vela desde un
//! servidor externo (el daemon del RedMagic; ver CLAUDE.md, sección Servidor
//! RedMagic, paso 3).
//!
//! El TUI acumula ΔOI y delta por vela en RAM: al arrancar, todas las ventanas
//! muestran "—" hasta que pasa el uptime suficiente (la de 24h tarda un día).
//! Si hay un servidor que lleva grabando esas series, este módulo las trae al
//! arrancar y siembra el historial en memoria.
//!
//! ESTRICTAMENTE OPCIONAL: sin `--oi-source`/`OI_SOURCE_URL`, o con el
//! servidor caído/lento/respondiendo basura, no se hace nada y la app se
//! comporta exactamente como antes. Nada aquí bloquea el arranque: corre en su
//! propia tarea, con timeouts cortos, y como mucho emite una tira de error.
//!
//! API esperada del servidor:
//!   GET /health -> {"status":"ok"}
//!   GET /oi?coin=BTC&since_ms=0    -> [{"ts_ms","oi","oi_notional","mark_px","funding"}, …] asc
//!   GET /delta?coin=BTC&since_ms=0 -> [{"minute_ms","buy_vol","sell_vol"}, …] asc

use std::time::Duration;

use hyperliquid_rust_sdk::BaseUrl;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;

use super::{new_client_retrying, now_ms, pf};
use crate::app::{DELTA_LOOKBACK_MS, OI_LOOKBACK_MS};
use types::{BackfillDelta, BackfillOi, DataMsg};

use super::types;

/// Pares a sembrar al arrancar: los de mayor OI, que son justo los que pintan
/// Ranking (ordenable por ΔOI), Heatmap top-OI (top 30) y Flujo de Dinero.
const SEED_PAIRS: usize = 30;
/// Timeouts cortos a propósito: un servidor caído o lento no debe notarse.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQ_TIMEOUT: Duration = Duration::from_secs(8);
/// Pausa entre peticiones consecutivas al daemon (suele ser un teléfono).
const STEP: Duration = Duration::from_millis(100);

/// URL del servidor de backfill, o None si no se configuró.
/// Acepta `--oi-source=URL`, `--oi-source URL` y la variable `OI_SOURCE_URL`.
pub fn source_url() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let from_args = args.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix("--oi-source=") {
            Some(v.to_string())
        } else if a == "--oi-source" {
            args.get(i + 1).cloned()
        } else {
            None
        }
    });
    let raw = from_args.or_else(|| std::env::var("OI_SOURCE_URL").ok())?;
    let raw = raw.trim().trim_end_matches('/').to_string();
    if raw.is_empty() {
        return None;
    }
    // sin esquema explícito se asume http (red local, ej. 192.168.1.105:8787)
    if raw.contains("://") {
        Some(raw)
    } else {
        Some(format!("http://{raw}"))
    }
}

pub fn spawn(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    url: String,
    coin_rx: watch::Receiver<Option<String>>,
) {
    tokio::spawn(run(base, tx, url, coin_rx));
}

async fn run(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    url: String,
    mut coin_rx: watch::Receiver<Option<String>>,
) {
    let Ok(http) = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQ_TIMEOUT)
        .build()
    else {
        return;
    };

    if !health_ok(&http, &url).await {
        let _ = tx.send(DataMsg::RestError(format!(
            "backfill: {url} no responde — historial solo en memoria"
        )));
        return;
    }

    // El daemon no sabe qué pares interesan: se eligen aquí, por OI, con la
    // misma fuente que el poller de contextos.
    let info = new_client_retrying(base, &tx).await;
    let mut coins: Vec<String> = match info.meta_and_asset_contexts().await {
        Ok((meta, ctxs)) => {
            let mut v: Vec<(String, f64)> = meta
                .universe
                .iter()
                .zip(ctxs.iter())
                .map(|(am, cx)| {
                    (
                        am.name.clone(),
                        pf(&cx.open_interest) * pf(&cx.mark_px).max(0.0),
                    )
                })
                .filter(|(_, ntl)| *ntl > 0.0)
                .collect();
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            v.truncate(SEED_PAIRS);
            v.into_iter().map(|(c, _)| c).collect()
        }
        Err(_) => Vec::new(),
    };
    // el par seleccionado siempre, aunque no esté entre los de más OI
    if let Some(sel) = coin_rx.borrow().clone() {
        if !coins.contains(&sel) {
            coins.insert(0, sel);
        }
    }

    let selected = coin_rx.borrow().clone();
    for coin in coins {
        // el delta por vela solo vive para el par seleccionado (se resetea al
        // cambiar de par), así que no tiene sentido pedirlo para los demás
        let want_delta = selected.as_deref() == Some(coin.as_str());
        fetch_coin(&http, &url, &tx, &coin, want_delta).await;
        tokio::time::sleep(STEP).await;
    }

    // al cambiar de par, su DeltaState nace vacío: sembrarlo también.
    while coin_rx.changed().await.is_ok() {
        let Some(coin) = coin_rx.borrow_and_update().clone() else {
            continue;
        };
        fetch_coin(&http, &url, &tx, &coin, true).await;
    }
}

/// Trae el historial de un par y lo envía al App. Cualquier fallo (servidor
/// caído a media sesión, JSON inesperado) se traga en silencio: el par se
/// queda con el comportamiento normal de acumular en memoria.
async fn fetch_coin(
    http: &reqwest::Client,
    url: &str,
    tx: &UnboundedSender<DataMsg>,
    coin: &str,
    want_delta: bool,
) {
    let now = now_ms();
    let oi = fetch_oi(http, url, coin, now.saturating_sub(OI_LOOKBACK_MS)).await;
    let delta = if want_delta {
        fetch_delta(http, url, coin, now.saturating_sub(DELTA_LOOKBACK_MS)).await
    } else {
        Vec::new()
    };
    if oi.is_empty() && delta.is_empty() {
        return;
    }
    let _ = tx.send(DataMsg::Backfill {
        coin: coin.to_string(),
        oi,
        delta,
    });
}

async fn health_ok(http: &reqwest::Client, url: &str) -> bool {
    let Ok(resp) = http.get(format!("{url}/health")).send().await else {
        return false;
    };
    resp.status().is_success()
}

async fn get_array(http: &reqwest::Client, url: String) -> Vec<Value> {
    let Ok(resp) = http.get(url).send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    match resp.json::<Value>().await {
        Ok(Value::Array(v)) => v,
        _ => Vec::new(),
    }
}

async fn fetch_oi(http: &reqwest::Client, url: &str, coin: &str, since_ms: u64) -> Vec<BackfillOi> {
    let rows = get_array(http, format!("{url}/oi?coin={coin}&since_ms={since_ms}")).await;
    let mut out: Vec<BackfillOi> = rows
        .iter()
        .filter_map(|v| {
            let ts_ms = num(v, "ts_ms")? as u64;
            let oi = num(v, "oi")?;
            let mark_px = num(v, "mark_px")?;
            if ts_ms == 0 || oi <= 0.0 || mark_px <= 0.0 {
                return None;
            }
            Some(BackfillOi { ts_ms, oi, mark_px })
        })
        .collect();
    // el contrato dice ascendente, pero no se depende de la buena fe del server
    out.sort_by_key(|p| p.ts_ms);
    out
}

async fn fetch_delta(
    http: &reqwest::Client,
    url: &str,
    coin: &str,
    since_ms: u64,
) -> Vec<BackfillDelta> {
    let rows = get_array(http, format!("{url}/delta?coin={coin}&since_ms={since_ms}")).await;
    let mut out: Vec<BackfillDelta> = rows
        .iter()
        .filter_map(|v| {
            let minute_ms = num(v, "minute_ms")? as u64;
            let buy_vol = num(v, "buy_vol")?;
            let sell_vol = num(v, "sell_vol")?;
            if minute_ms == 0 || buy_vol < 0.0 || sell_vol < 0.0 {
                return None;
            }
            Some(BackfillDelta {
                minute_ms,
                buy_vol,
                sell_vol,
            })
        })
        .collect();
    out.sort_by_key(|b| b.minute_ms);
    out
}

/// Campo numérico tolerante: acepta número JSON o string numérica.
fn num(v: &Value, key: &str) -> Option<f64> {
    let f = match v.get(key)? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.parse().ok()?,
        _ => return None,
    };
    f.is_finite().then_some(f)
}
