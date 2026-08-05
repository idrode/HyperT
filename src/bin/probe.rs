//! Sonda de validación de datos en vivo (sin TUI): comprueba Info API y WebSocket.
//! Uso: cargo run --bin probe [--testnet]

use std::time::{Duration, Instant};

use hyperliquid_rust_sdk::{BaseUrl, InfoClient, Message, Subscription};
use tokio::sync::mpsc::unbounded_channel;
use tokio::time::timeout;

/// Ventana de observación del WS. 5s daba falsos diagnósticos: hace falta
/// margen para ver la cadencia real y posibles cortes/reconexiones.
const WS_WINDOW_SECS: u64 = 30;

/// Logger mínimo a stderr para ver los error!/warn! internos del SDK
/// (reconexiones, fallos de resuscripción), que sin logger se pierden.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("[sdk {}] {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

static LOGGER: StderrLog = StderrLog;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    let testnet = std::env::args().any(|a| a == "--testnet");
    let base = if testnet {
        BaseUrl::Testnet
    } else {
        BaseUrl::Mainnet
    };

    println!("== REST: metaAndAssetCtxs ==");
    let info = InfoClient::new(None, Some(base)).await?;
    let (meta, ctxs) = info.meta_and_asset_contexts().await?;
    let mut pairs: Vec<(String, f64, f64, f64)> = meta
        .universe
        .iter()
        .zip(ctxs.iter())
        .filter_map(|(m, c)| {
            let mark: f64 = c.mark_px.parse().ok()?;
            let oi: f64 = c.open_interest.parse().ok()?;
            let funding: f64 = c.funding.parse().ok()?;
            (mark > 0.0).then(|| (m.name.clone(), mark, oi * mark, funding))
        })
        .collect();
    println!("perps activos: {}", pairs.len());
    pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    println!("top 5 por OI notional:");
    for (name, mark, oi_ntl, funding) in pairs.iter().take(5) {
        println!(
            "  {name:>8}  mark {mark:>12.4}  OI ${:>10.1}M  funding {:+.5}%/h ({:+.1}% APR)",
            oi_ntl / 1e6,
            funding * 100.0,
            funding * 24.0 * 365.0 * 100.0
        );
    }

    println!("== WS: allMids ({WS_WINDOW_SECS}s) ==");
    let mut ws = InfoClient::with_reconnect(None, Some(base)).await?;
    let (tx, mut rx) = unbounded_channel();
    ws.subscribe(Subscription::AllMids, tx).await?;
    let t0 = Instant::now();
    let mut msgs = 0u32;
    let mut others = 0u32;
    let mut nodata = 0u32;
    let mut errors = 0u32;
    let mut btc_mid = String::new();
    let mut btc_changes = 0u32;
    let mut last_at: Option<Instant> = None;
    let mut max_gap = Duration::ZERO;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(WS_WINDOW_SECS);
    while let Ok(Some(msg)) = timeout_at_or_break(deadline, &mut rx).await {
        let el = t0.elapsed();
        match msg {
            Message::AllMids(am) => {
                msgs += 1;
                let gap = last_at.map(|p| p.elapsed()).unwrap_or_default();
                max_gap = max_gap.max(gap);
                last_at = Some(Instant::now());
                let mid = am.data.mids.get("BTC").cloned().unwrap_or_default();
                let changed = mid != btc_mid;
                if changed && !btc_mid.is_empty() {
                    btc_changes += 1;
                }
                btc_mid = mid;
                // primeros mensajes y luego 1 de cada 10, para ver la cadencia
                if msgs <= 5 || msgs.is_multiple_of(10) {
                    println!(
                        "  [{:>6.2}s] allMids #{msgs} (gap {:>5}ms) BTC {btc_mid}{}",
                        el.as_secs_f64(),
                        gap.as_millis(),
                        if changed { "" } else { " (=)" },
                    );
                }
            }
            Message::NoData => {
                nodata += 1;
                println!(
                    "  [{:>6.2}s] ¡NoData! (stream caído, reconectando)",
                    el.as_secs_f64()
                );
            }
            Message::HyperliquidError(e) => {
                errors += 1;
                println!("  [{:>6.2}s] ¡HyperliquidError! {e}", el.as_secs_f64());
            }
            m => {
                others += 1;
                println!("  [{:>6.2}s] otro mensaje: {m:?}", el.as_secs_f64());
            }
        }
    }
    let gap_medio = if msgs > 1 {
        t0.elapsed().as_millis() as f64 / msgs as f64
    } else {
        f64::NAN
    };
    println!(
        "allMids en {WS_WINDOW_SECS}s: {msgs} (gap medio {gap_medio:.0}ms, máx {}ms) · \
         cambios de mid BTC: {btc_changes} · otros: {others} · NoData: {nodata} · errores: {errors}",
        max_gap.as_millis()
    );
    println!("mid BTC final: {btc_mid}");
    if msgs == 0 {
        anyhow::bail!("el WebSocket no entregó datos");
    }
    println!(
        "nota: ~1 allMids/5s es la cadencia del SERVIDOR (verificada también \
         con WS crudo sin SDK el 2026-07-10), no un bug del cliente."
    );

    println!("== WS: bbo BTC (10s) — feed por-coin del par seleccionado ==");
    let (tx2, mut rx2) = unbounded_channel();
    ws.subscribe(
        Subscription::Bbo {
            coin: "BTC".to_string(),
        },
        tx2,
    )
    .await?;
    let t0 = Instant::now();
    let mut bbo_msgs = 0u32;
    let mut bbo_mid = 0.0_f64;
    let mut last_at: Option<Instant> = None;
    let mut max_gap = Duration::ZERO;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while let Ok(Some(msg)) = timeout_at_or_break(deadline, &mut rx2).await {
        if let Message::Bbo(b) = msg {
            bbo_msgs += 1;
            let gap = last_at.map(|p| p.elapsed()).unwrap_or_default();
            max_gap = max_gap.max(gap);
            last_at = Some(Instant::now());
            let bid = b.data.bbo.first().and_then(|x| x.as_ref());
            let ask = b.data.bbo.get(1).and_then(|x| x.as_ref());
            if let (Some(bid), Some(ask)) = (bid, ask) {
                let bid: f64 = bid.px.parse().unwrap_or(0.0);
                let ask: f64 = ask.px.parse().unwrap_or(0.0);
                bbo_mid = (bid + ask) / 2.0;
            }
            if bbo_msgs <= 3 {
                println!(
                    "  [{:>5.2}s] bbo #{bbo_msgs} (gap {:>4}ms) mid BTC {bbo_mid}",
                    t0.elapsed().as_secs_f64(),
                    gap.as_millis(),
                );
            }
        }
    }
    println!(
        "bbo BTC en 10s: {bbo_msgs} mensajes (gap máx {}ms) | mid final {bbo_mid} — \
         esto debe ser sub-segundo; es el feed que usa el TUI para el par seleccionado",
        max_gap.as_millis()
    );
    if bbo_msgs == 0 {
        anyhow::bail!("la suscripción bbo no entregó datos");
    }
    println!("OK: REST y WS entregan datos en vivo.");
    Ok(())
}

async fn timeout_at_or_break<T>(
    deadline: tokio::time::Instant,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<T>,
) -> Result<Option<T>, tokio::time::error::Elapsed> {
    timeout(
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        rx.recv(),
    )
    .await
}
