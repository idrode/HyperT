pub mod opens;
pub mod types;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::Address;
use hyperliquid_rust_sdk::{
    BaseUrl, InfoClient, Message, Subscription, UserStateResponse, UserTokenBalance,
    MAINNET_API_URL, TESTNET_API_URL,
};
use tokio::sync::mpsc::{self, unbounded_channel, UnboundedSender};
use tokio::sync::watch;
use tokio::time::sleep;

use types::{
    AccountMode, AccountSnapshot, CandlePoint, CtxSnapshot, DataMsg, ExtraReq, FillInfo, PairMeta,
    PosInfo, SpotSnapshot, TransferInfo, WhaleInfo,
};

const CTX_POLL_SECS: u64 = 5;
/// ~30d de funding horario para que el percentil de la Vista 6 signifique
/// algo; la API corta cada respuesta a ~500 entradas, así que se pagina.
const FUNDING_LOOKBACK_MS: u64 = 30 * 24 * 3600 * 1000;
/// Una respuesta de fundingHistory por debajo de esto se considera la última
/// página (el tope duro de la API es 500).
const FUNDING_PAGE_FULL: usize = 450;
/// Nº de velas objetivo por fetch (lookback = intervalo × esto).
/// 180 = 60 de warmup de la SMA de ΔOI + 120 de lookback del histograma
/// de densidad (src/liqdens.rs); la vista de par solo pinta las que caben.
const CANDLE_COUNT: u64 = 180;
/// Cuentas top del leaderboard a trackear como whales. Las top por valor
/// suelen ser vaults sin perps, así que hace falta margen para que queden
/// suficientes cuentas con posiciones tras filtrar.
const WHALE_COUNT: usize = 100;
const WHALE_POLL_SECS: u64 = 60;
/// Pausa entre clearinghouseState consecutivos para no agotar el rate limit.
const WHALE_STEP_MS: u64 = 200;
const LEADERBOARD_REFRESH: Duration = Duration::from_secs(30 * 60);
/// Cuántos fallos del escaneo de whales se detallan (dirección + error) en la
/// tira de error. Suficiente para ver si todos comparten causa (rate limit,
/// timeout) sin desbordar una línea de estado si fallan decenas de cuentas.
const WHALE_ERR_SAMPLES: usize = 3;
const WALLET_POLL_SECS: u64 = 10;
/// El historial de fills (userFills) cambia despacio comparado con el estado
/// de posiciones: se refresca más espaciado que el clearinghouseState de 10s.
const FILLS_REFRESH: Duration = Duration::from_secs(60);
/// Las aperturas se reconstruyen con decenas de peticiones: se rehacen solo al
/// cambiar de wallet observada o cada 10 minutos.
const OPENS_REFRESH: Duration = Duration::from_secs(600);
/// Cadencia del saldo USDC on-chain (RPC público de Arbitrum: ser educados).
const USDC_POLL_SECS: u64 = 30;

/// RPC público + contrato del USDC NATIVO de Circle según la chain CAIP-2 de
/// la sesión WC. Direcciones verificadas el 2026-07-19 contra
/// developers.circle.com/stablecoins/usdc-contract-addresses y contra
/// name()/decimals() on-chain. OJO: el USDC.e puenteado
/// (0xFF970A61A04b1cA14834A43f5dE4533eBDDB5CC8) también responde
/// symbol()="USDC" — NO es el oficial de Circle, no usarlo.
/// `ARB_RPC_URL` fuerza otro RPC (se aplica a la chain activa).
fn usdc_net(chain: &str) -> Option<(String, &'static str)> {
    let (rpc, contract) = match chain {
        "eip155:42161" => (
            "https://arb1.arbitrum.io/rpc",
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        ),
        "eip155:421614" => (
            "https://sepolia-rollup.arbitrum.io/rpc",
            "0x75faf114eafb1bdbe2f0316df893fd58ce46aa4d",
        ),
        _ => return None,
    };
    let rpc = std::env::var("ARB_RPC_URL").unwrap_or_else(|_| rpc.to_string());
    Some((rpc, contract))
}

/// Mínimo del bridge en unidades base del USDC (6 decimales): 5 USDC. Por
/// debajo el bridge NO acredita y los fondos se pierden (doc de Bridge2).
pub const MIN_DEPOSIT_UNITS: u128 = 5_000_000;

/// Ruta del depósito real (Pieza 2): a qué contratos y por qué RPC.
pub struct DepositRoute {
    pub rpc: String,
    /// Contrato del USDC nativo (el `to` de la transacción: es un transfer).
    pub usdc: &'static str,
    /// Bridge2 de Hyperliquid, destinatario del transfer. El depósito se
    /// acredita a la dirección REMITENTE en <1 min.
    pub bridge: &'static str,
}

/// Mecanismo verificado el 2026-07-19 contra la doc oficial de Bridge2
/// (hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/bridge2) y
/// Arbiscan: NO hay approve ni función deposit() en el flujo de usuario — el
/// depósito es un transfer simple de USDC nativo al bridge, acreditado al
/// remitente. Solo mainnet a propósito: el plan de Fase 2 llena testnet vía
/// faucet (tras un depósito mainnet), no por su bridge, y el token mock del
/// bridge de testnet no está verificado — no mapearlo a ciegas.
pub fn deposit_route(chain: &str) -> Option<DepositRoute> {
    if chain != "eip155:42161" {
        return None;
    }
    let (rpc, usdc) = usdc_net(chain)?;
    Some(DepositRoute {
        rpc,
        usdc,
        bridge: "0x2df1c51e09aecf9cacb7bc98cb1742757f163df7",
    })
}

/// Comisión fija del retiro que Hyperliquid descuenta de la cantidad pedida
/// (doc oficial exchange-endpoint, 2026-07-19: "$1 fee for withdrawing").
pub const WITHDRAW_FEE_UNITS: u128 = 1_000_000;

/// Ruta del retiro (paso 5 de Fase 2): a qué Exchange API se envía la
/// solicitud firmada y por dónde se vigila la llegada del USDC a la wallet.
pub struct WithdrawRoute {
    /// Base del Exchange API (el POST va a `{api}/exchange`).
    pub api: &'static str,
    /// Campo `hyperliquidChain` del action y del typed data EIP-712.
    pub hl_chain: &'static str,
    /// Chain id numérico del dominio EIP-712 (`signatureChainId`).
    pub chain_id: u64,
    /// RPC de Arbitrum para vigilar la llegada on-chain.
    pub rpc: String,
    /// Contrato del USDC en el que aparece el saldo retirado.
    pub usdc: &'static str,
}

/// Misma función para ambas redes: la chain CAIP-2 de la sesión WC (que ya
/// deriva del flag --testnet en main.rs) decide endpoint y dominio de firma.
/// Retiro verificado contra la doc oficial (withdraw3): Hyperliquid procesa
/// la solicitud y envía USDC a `destination` en Arbitrum en ~5 min, con $1
/// de comisión descontado de la cantidad.
///
/// OJO token de llegada: en testnet el bridge (0x08cf…6f89, doc de Bridge2)
/// reparte SU mock USDC (0x1baa…34d5), NO el USDC de Circle en Sepolia que
/// usa la Pieza 1 — vigilar el contrato equivocado = no ver llegar nunca.
pub fn withdraw_route(chain: &str) -> Option<WithdrawRoute> {
    let (api, hl_chain, chain_id, usdc) = match chain {
        "eip155:42161" => (
            MAINNET_API_URL,
            "Mainnet",
            42_161,
            // mainnet retira el mismo USDC nativo de Circle de la Pieza 1
            "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
        ),
        "eip155:421614" => (
            TESTNET_API_URL,
            "Testnet",
            421_614,
            // mock USDC del bridge de testnet (doc oficial de Bridge2)
            "0x1baabb04529d43a73232b713c0fe471f7c7334d5",
        ),
        _ => return None,
    };
    let (rpc, _) = usdc_net(chain)?;
    Some(WithdrawRoute {
        api,
        hl_chain,
        chain_id,
        rpc,
        usdc,
    })
}

/// Calldata de `balanceOf(address)` (selector 0x70a08231 + address a 32 bytes).
fn balanceof_calldata(addr: Address) -> String {
    let hex: String = addr.as_slice().iter().map(|b| format!("{b:02x}")).collect();
    format!("0x70a08231{hex:0>64}")
}

/// Palabra ABI (uint256 en hex) → f64. Acumula dígito a dígito para no poder
/// desbordar jamás por una respuesta rara del RPC; None si no es un word hex.
fn abi_word_f64(hex: &str) -> Option<f64> {
    let h = hex.strip_prefix("0x")?;
    if h.is_empty() || h.len() > 64 {
        return None;
    }
    h.chars().try_fold(0.0f64, |acc, c| {
        c.to_digit(16).map(|d| acc * 16.0 + d as f64)
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn pf(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

/// Lanza las tareas de fondo de la capa de datos (todas solo lectura):
/// poller de metaAndAssetCtxs, WebSocket allMids, BBO del par seleccionado,
/// fetcher de velas/funding, tracker de whales, watcher de la wallet y saldo
/// USDC on-chain de la cuenta maestra.
pub fn spawn_data_tasks(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    extra_rx: mpsc::Receiver<ExtraReq>,
    wallet_rx: watch::Receiver<Vec<Address>>,
    usdc_rx: watch::Receiver<Option<(Address, String)>>,
    coin_rx: watch::Receiver<Option<String>>,
) {
    tokio::spawn(ctx_poller(base, tx.clone()));
    tokio::spawn(ws_mids(base, tx.clone()));
    tokio::spawn(ws_coin_bbo(base, tx.clone(), coin_rx.clone()));
    tokio::spawn(ws_coin_trades(base, tx.clone(), coin_rx));
    tokio::spawn(extra_fetcher(base, tx.clone(), extra_rx));
    tokio::spawn(whale_watcher(base, tx.clone()));
    tokio::spawn(spot_watcher(base, tx.clone(), usdc_rx.clone()));
    tokio::spawn(usdc_watcher(tx.clone(), usdc_rx));
    tokio::spawn(wallet_watcher(base, tx, wallet_rx));
}

async fn new_client_retrying(base: BaseUrl, tx: &UnboundedSender<DataMsg>) -> InfoClient {
    loop {
        match InfoClient::new(None, Some(base)).await {
            Ok(c) => return c,
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("init REST: {e}")));
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn ctx_poller(base: BaseUrl, tx: UnboundedSender<DataMsg>) {
    let info = new_client_retrying(base, &tx).await;
    loop {
        match info.meta_and_asset_contexts().await {
            Ok((meta, ctxs)) => {
                let t = Instant::now();
                let t_ms = now_ms();
                let mut out = Vec::with_capacity(ctxs.len());
                for (am, cx) in meta.universe.iter().zip(ctxs.iter()) {
                    let snap = CtxSnapshot {
                        t,
                        t_ms,
                        mark_px: pf(&cx.mark_px),
                        mid_px: cx.mid_px.as_deref().map(pf),
                        oracle_px: pf(&cx.oracle_px),
                        funding: pf(&cx.funding),
                        open_interest: pf(&cx.open_interest),
                        premium: cx.premium.as_deref().map(pf),
                        day_ntl_vlm: pf(&cx.day_ntl_vlm),
                        prev_day_px: pf(&cx.prev_day_px),
                    };
                    // mark 0 => activo deslistado/sin mercado
                    if snap.mark_px > 0.0 {
                        out.push((
                            PairMeta {
                                name: am.name.clone(),
                                sz_decimals: am.sz_decimals,
                                max_leverage: am.max_leverage,
                            },
                            snap,
                        ));
                    }
                }
                let _ = tx.send(DataMsg::Ctxs(out));
                sleep(Duration::from_secs(CTX_POLL_SECS)).await;
            }
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("metaAndAssetCtxs: {e}")));
                sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

async fn ws_mids(base: BaseUrl, tx: UnboundedSender<DataMsg>) {
    loop {
        let mut info = match InfoClient::with_reconnect(None, Some(base)).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(DataMsg::WsStatus(false));
                let _ = tx.send(DataMsg::RestError(format!("init WS: {e}")));
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let (wtx, mut wrx) = unbounded_channel();
        if let Err(e) = info.subscribe(Subscription::AllMids, wtx).await {
            let _ = tx.send(DataMsg::RestError(format!("subscribe allMids: {e}")));
            sleep(Duration::from_secs(5)).await;
            continue;
        }
        while let Some(msg) = wrx.recv().await {
            match msg {
                Message::AllMids(am) => {
                    // los mids spot llegan como "@<idx>"; solo nos interesan perps
                    let mids: Vec<(String, f64)> = am
                        .data
                        .mids
                        .iter()
                        .filter(|(k, _)| !k.starts_with('@'))
                        .map(|(k, v)| (k.clone(), pf(v)))
                        .collect();
                    let _ = tx.send(DataMsg::WsStatus(true));
                    let _ = tx.send(DataMsg::Mids(mids));
                }
                Message::NoData => {
                    let _ = tx.send(DataMsg::WsStatus(false));
                }
                _ => {}
            }
        }
        // el stream murió: reconectar desde cero
        let _ = tx.send(DataMsg::WsStatus(false));
        sleep(Duration::from_secs(3)).await;
    }
}

/// Mid en vivo del par seleccionado vía suscripción `bbo` por-coin.
///
/// Razón de ser: allMids llega solo cada ~5s (cadencia del SERVIDOR — medida
/// 2026-07-10 con el SDK y con un WS crudo sin SDK, ~5000ms de gap en ambos;
/// no es un bug de cliente). El BBO por-coin empuja en cada cambio del mejor
/// bid/ask (~100-500ms en pares líquidos), que es lo que usa hyperliquid.xyz.
/// Se suscribe solo al par seleccionado y se cambia al navegar.
async fn ws_coin_bbo(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    mut coin_rx: watch::Receiver<Option<String>>,
) {
    loop {
        let mut info = match InfoClient::with_reconnect(None, Some(base)).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("init WS bbo: {e}")));
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let (wtx, mut wrx) = unbounded_channel();
        let mut coin: Option<String> = None;
        let mut sub_id: Option<u32> = None;
        loop {
            // (re)suscribir si hay par seleccionado y no hay suscripción viva
            let target = coin_rx.borrow_and_update().clone();
            if target != coin || (sub_id.is_none() && target.is_some()) {
                if let Some(id) = sub_id.take() {
                    let _ = info.unsubscribe(id).await;
                }
                coin = target;
                if let Some(c) = &coin {
                    match info
                        .subscribe(Subscription::Bbo { coin: c.clone() }, wtx.clone())
                        .await
                    {
                        Ok(id) => sub_id = Some(id),
                        Err(e) => {
                            let _ = tx.send(DataMsg::RestError(format!("bbo {c}: {e}")));
                        }
                    }
                }
            }
            tokio::select! {
                changed = coin_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                msg = wrx.recv() => {
                    let Some(msg) = msg else { break };
                    if let Message::Bbo(b) = msg {
                        let bid = b.data.bbo.first().and_then(|x| x.as_ref());
                        let ask = b.data.bbo.get(1).and_then(|x| x.as_ref());
                        if let (Some(bid), Some(ask)) = (bid, ask) {
                            let mid = (pf(&bid.px) + pf(&ask.px)) / 2.0;
                            if mid > 0.0 {
                                let _ = tx.send(DataMsg::CoinMid { coin: b.data.coin, mid });
                            }
                        }
                    }
                }
                // reintento periódico si la suscripción falló al crearse
                _ = sleep(Duration::from_secs(3)), if sub_id.is_none() && coin.is_some() => {}
            }
        }
        // canal muerto: recrear cliente desde cero
        sleep(Duration::from_secs(3)).await;
    }
}

/// Trades en vivo del par seleccionado vía canal `trades` por-coin, para el
/// CVD de la Vista 6. Mismo ciclo de vida que el BBO por-par: se suscribe solo
/// al seleccionado y cambia al navegar. Cada trade trae el lado agresor
/// (B = compra, A = venta); se agrega el notional por batch y el TUI acumula.
async fn ws_coin_trades(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    mut coin_rx: watch::Receiver<Option<String>>,
) {
    loop {
        let mut info = match InfoClient::with_reconnect(None, Some(base)).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("init WS trades: {e}")));
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let (wtx, mut wrx) = unbounded_channel();
        let mut coin: Option<String> = None;
        let mut sub_id: Option<u32> = None;
        loop {
            let target = coin_rx.borrow_and_update().clone();
            if target != coin || (sub_id.is_none() && target.is_some()) {
                if let Some(id) = sub_id.take() {
                    let _ = info.unsubscribe(id).await;
                }
                coin = target;
                if let Some(c) = &coin {
                    match info
                        .subscribe(Subscription::Trades { coin: c.clone() }, wtx.clone())
                        .await
                    {
                        Ok(id) => sub_id = Some(id),
                        Err(e) => {
                            let _ = tx.send(DataMsg::RestError(format!("trades {c}: {e}")));
                        }
                    }
                }
            }
            tokio::select! {
                changed = coin_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                msg = wrx.recv() => {
                    let Some(msg) = msg else { break };
                    if let Message::Trades(t) = msg {
                        let (mut buy, mut sell) = (0.0_f64, 0.0_f64);
                        let mut batch_coin: Option<String> = None;
                        let mut t_ms = 0u64;
                        for tr in &t.data {
                            let ntl = pf(&tr.px) * pf(&tr.sz);
                            if tr.side == "B" {
                                buy += ntl;
                            } else {
                                sell += ntl;
                            }
                            t_ms = t_ms.max(tr.time);
                            batch_coin = Some(tr.coin.clone());
                        }
                        if let Some(c) = batch_coin {
                            let _ = tx.send(DataMsg::CoinTrades {
                                coin: c,
                                buy_ntl: buy,
                                sell_ntl: sell,
                                t_ms,
                            });
                        }
                    }
                }
                _ = sleep(Duration::from_secs(3)), if sub_id.is_none() && coin.is_some() => {}
            }
        }
        sleep(Duration::from_secs(3)).await;
    }
}

/// fundingHistory paginado: encadena peticiones por startTime hasta cubrir el
/// lookback completo (30d ≈ 720 entradas horarias, ~2 páginas de 500).
async fn fetch_funding_hist(
    info: &InfoClient,
    coin: &str,
    start: u64,
    now: u64,
) -> Result<Vec<(u64, f64)>, hyperliquid_rust_sdk::Error> {
    let mut out: Vec<(u64, f64)> = Vec::new();
    let mut from = start;
    for _ in 0..6 {
        let fs = info.funding_history(coin.to_string(), from, None).await?;
        let Some(last) = fs.last() else { break };
        let last_t = last.time;
        out.extend(fs.iter().map(|f| (f.time, pf(&f.funding_rate))));
        if fs.len() < FUNDING_PAGE_FULL || last_t <= from || last_t >= now {
            break;
        }
        from = last_t + 1;
    }
    out.sort_by_key(|(t, _)| *t);
    out.dedup_by_key(|(t, _)| *t);
    Ok(out)
}

async fn extra_fetcher(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    mut rx: mpsc::Receiver<ExtraReq>,
) {
    let info = new_client_retrying(base, &tx).await;
    while let Some(mut req) = rx.recv().await {
        // si el usuario navegó rápido, quédate solo con la última petición
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let now = now_ms();
        let lookback = req.interval.ms() * CANDLE_COUNT;
        let candles = info
            .candles_snapshot(
                req.coin.clone(),
                req.interval.api().to_string(),
                now - lookback,
                now,
            )
            .await;
        let funding = fetch_funding_hist(&info, &req.coin, now - FUNDING_LOOKBACK_MS, now).await;
        match (candles, funding) {
            (Ok(cs), Ok(funding_hist)) => {
                let candles: Vec<CandlePoint> = cs
                    .iter()
                    .map(|c| CandlePoint {
                        t_close: c.time_close,
                        open: pf(&c.open),
                        high: pf(&c.high),
                        low: pf(&c.low),
                        close: pf(&c.close),
                        volume: pf(&c.vlm),
                    })
                    .collect();
                let _ = tx.send(DataMsg::PairExtra {
                    coin: req.coin,
                    interval: req.interval,
                    candles,
                    funding_hist,
                });
            }
            (Err(e), _) | (_, Err(e)) => {
                let _ = tx.send(DataMsg::RestError(format!("extra {}: {e}", req.coin)));
            }
        }
    }
}

/// Snapshot de cuenta a partir del user_state crudo — compartido entre el
/// wallet_watcher periódico y el refresh inmediato del trader (paso 7).
pub(crate) fn account_snapshot(addr: String, st: &UserStateResponse) -> AccountSnapshot {
    AccountSnapshot {
        addr,
        account_value: pf(&st.margin_summary.account_value),
        withdrawable: pf(&st.withdrawable),
        total_margin_used: pf(&st.margin_summary.total_margin_used),
        total_ntl_pos: pf(&st.margin_summary.total_ntl_pos),
        positions: parse_positions(st),
    }
}

/// Traduce un fill del SDK (`UserFillsResponse`, todo strings) a `FillInfo`.
fn parse_fill(f: &hyperliquid_rust_sdk::UserFillsResponse) -> FillInfo {
    FillInfo {
        coin: f.coin.clone(),
        dir: f.dir.clone(),
        px: pf(&f.px),
        sz: pf(&f.sz),
        start_position: pf(&f.start_position),
        closed_pnl: pf(&f.closed_pnl),
        fee: pf(&f.fee),
        time_ms: f.time,
    }
}

/// Mismo `FillInfo` pero desde el JSON crudo: `userFillsByTime` no está en el
/// SDK pineado, así que su respuesta se parsea a mano (misma forma que
/// `userFills`, verificado contra la API real).
fn parse_fill_json(f: &serde_json::Value) -> Option<FillInfo> {
    let g = |k: &str| f.get(k).and_then(|v| v.as_str()).map(pf).unwrap_or(0.0);
    Some(FillInfo {
        coin: f.get("coin")?.as_str()?.to_string(),
        dir: f
            .get("dir")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        px: g("px"),
        sz: g("sz"),
        start_position: g("startPosition"),
        closed_pnl: g("closedPnl"),
        fee: g("fee"),
        time_ms: f.get("time")?.as_u64()?,
    })
}

fn parse_positions(st: &UserStateResponse) -> Vec<PosInfo> {
    st.asset_positions
        .iter()
        .map(|ap| {
            let p = &ap.position;
            PosInfo {
                coin: p.coin.clone(),
                szi: pf(&p.szi),
                entry_px: p.entry_px.as_deref().map(pf),
                position_value: pf(&p.position_value),
                unrealized_pnl: pf(&p.unrealized_pnl),
                roe: pf(&p.return_on_equity),
                leverage: p.leverage.value,
                is_cross: p.leverage.type_string == "cross",
                liq_px: p.liquidation_px.as_deref().map(pf),
                since_open_funding: pf(&p.cum_funding.since_open),
            }
        })
        .filter(|p| p.szi != 0.0)
        .collect()
}

/// Descarga el leaderboard (JSON grande, ~30MB) y devuelve las top cuentas por valor.
async fn fetch_leaderboard(url: &str) -> anyhow::Result<Vec<Address>> {
    let v: serde_json::Value = reqwest::get(url).await?.json().await?;
    let rows = v
        .get("leaderboardRows")
        .and_then(|r| r.as_array())
        .ok_or_else(|| anyhow::anyhow!("leaderboard: formato inesperado"))?;
    let mut accs: Vec<(f64, Address)> = rows
        .iter()
        .filter_map(|r| {
            let av: f64 = r.get("accountValue")?.as_str()?.parse().ok()?;
            let addr: Address = r.get("ethAddress")?.as_str()?.parse().ok()?;
            Some((av, addr))
        })
        .collect();
    accs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    accs.truncate(WHALE_COUNT);
    Ok(accs.into_iter().map(|(_, a)| a).collect())
}

/// Mensaje de la tira para fallos del escaneo de whales: conteo + las primeras
/// direcciones que fallaron con su error real, para poder diagnosticar el fallo
/// a posteriori sin tener que reproducirlo.
fn whale_err_msg(errs: usize, total: usize, samples: &[String]) -> String {
    let mut m = format!("whales: {errs}/{total} cuentas fallaron");
    if !samples.is_empty() {
        m.push_str(" · ");
        m.push_str(&samples.join(" | "));
        if errs > samples.len() {
            m.push_str(&format!(" (+{} más)", errs - samples.len()));
        }
    }
    m
}

async fn whale_watcher(base: BaseUrl, tx: UnboundedSender<DataMsg>) {
    let info = new_client_retrying(base, &tx).await;
    let lb_url = match base {
        BaseUrl::Testnet => "https://stats-data.hyperliquid.xyz/Testnet/leaderboard",
        _ => "https://stats-data.hyperliquid.xyz/Mainnet/leaderboard",
    };
    let mut addrs: Vec<Address> = Vec::new();
    let mut lb_at: Option<Instant> = None;
    let mut first_scan = true;
    loop {
        let stale = lb_at.is_none_or(|t| t.elapsed() > LEADERBOARD_REFRESH);
        if addrs.is_empty() || stale {
            let _ = tx.send(DataMsg::WhaleStatus(
                "descargando leaderboard (~30MB)…".to_string(),
            ));
            match fetch_leaderboard(lb_url).await {
                Ok(a) => {
                    let _ = tx.send(DataMsg::WhaleStatus(format!(
                        "trackeando top {} cuentas del leaderboard",
                        a.len()
                    )));
                    addrs = a;
                    lb_at = Some(Instant::now());
                }
                Err(e) => {
                    let _ = tx.send(DataMsg::WhaleStatus(format!("leaderboard: {e}")));
                    sleep(Duration::from_secs(60)).await;
                    continue;
                }
            }
        }
        // batchClearinghouseStates (info.user_states) devuelve null/500 en la
        // API pública — verificado 2026-07-09 — así que se consulta
        // clearinghouseState dirección a dirección, con pausa entre llamadas.
        let mut whales: Vec<WhaleInfo> = Vec::new();
        let mut errs = 0usize;
        // muestras de los primeros fallos (dirección + error real). Descartar el
        // error con `Err(_)` dejaba la tira con un conteo pelado ("N/100 cuentas
        // fallaron") imposible de diagnosticar sin reproducir el fallo primero.
        let mut err_samples: Vec<String> = Vec::new();
        for (i, a) in addrs.iter().enumerate() {
            match info.user_state(*a).await {
                Ok(st) => {
                    let positions = parse_positions(&st);
                    if !positions.is_empty() {
                        whales.push(WhaleInfo {
                            addr: format!("{a}"),
                            account_value: pf(&st.margin_summary.account_value),
                            positions,
                        });
                    }
                }
                Err(e) => {
                    errs += 1;
                    if err_samples.len() < WHALE_ERR_SAMPLES {
                        err_samples.push(format!("{a}: {e}"));
                    }
                }
            }
            // en el primer escaneo, ir volcando parciales para que la vista
            // no espere ~30s en blanco; en rescans se sustituye entera al final
            if first_scan && (i + 1) % 10 == 0 {
                let _ = tx.send(DataMsg::WhaleStatus(format!(
                    "escaneando cuentas {}/{}…",
                    i + 1,
                    addrs.len()
                )));
                let _ = tx.send(DataMsg::Whales(whales.clone()));
            }
            sleep(Duration::from_millis(WHALE_STEP_MS)).await;
        }
        if errs > 0 {
            let _ = tx.send(DataMsg::RestError(whale_err_msg(
                errs,
                addrs.len(),
                &err_samples,
            )));
        }
        let _ = tx.send(DataMsg::WhaleStatus(format!(
            "top {} leaderboard · {} con posiciones",
            addrs.len(),
            whales.len()
        )));
        let _ = tx.send(DataMsg::Whales(whales));
        first_scan = false;
        sleep(Duration::from_secs(WHALE_POLL_SECS)).await;
    }
}

/// `eth_call` JSON-RPC de solo lectura (sin cuenta, sin gas, sin firma).
async fn eth_call(
    client: &reqwest::Client,
    rpc: &str,
    to: &str,
    data: String,
) -> Result<String, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{"to": to, "data": data}, "latest"],
    });
    let v: serde_json::Value = client
        .post(rpc)
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    if let Some(err) = v.get("error") {
        return Err(err.to_string());
    }
    v.get("result")
        .and_then(|r| r.as_str())
        .map(str::to_string)
        .ok_or_else(|| "respuesta sin result".into())
}

/// decimals() + balanceOf() del contrato USDC → saldo en unidades humanas.
/// Leer decimals en cada ciclo re-verifica de paso que el contrato responde
/// como un ERC20 sano (un RPC/contrato equivocado devuelve "0x" y se corta).
pub(crate) async fn fetch_usdc_balance(
    client: &reqwest::Client,
    rpc: &str,
    contract: &str,
    addr: Address,
) -> Result<f64, String> {
    let dec = abi_word_f64(&eth_call(client, rpc, contract, "0x313ce567".into()).await?)
        .filter(|d| (1.0..=30.0).contains(d))
        .ok_or("decimals() ilegible")?;
    let bal = abi_word_f64(&eth_call(client, rpc, contract, balanceof_calldata(addr)).await?)
        .ok_or("balanceOf() ilegible")?;
    Ok(bal / 10f64.powf(dec))
}

/// Pieza 1 del depósito: saldo USDC on-chain de la cuenta maestra WC, vía
/// eth_call de SOLO LECTURA a un RPC público — esta tarea no firma jamás.
async fn usdc_watcher(
    tx: UnboundedSender<DataMsg>,
    mut rx: watch::Receiver<Option<(Address, String)>>,
) {
    let client = reqwest::Client::new();
    loop {
        let target = rx.borrow_and_update().clone();
        let Some((addr, chain)) = target else {
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        };
        match usdc_net(&chain) {
            Some((rpc, contract)) => {
                match fetch_usdc_balance(&client, &rpc, contract, addr).await {
                    Ok(v) => {
                        let _ = tx.send(DataMsg::UsdcBalance {
                            addr: format!("{addr}"),
                            usdc: Some(v),
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(DataMsg::RestError(format!("usdc: {e}")));
                    }
                }
            }
            // chain sin mapeo de USDC: informar para que la UI no diga "cargando"
            None => {
                let _ = tx.send(DataMsg::UsdcBalance {
                    addr: format!("{addr}"),
                    usdc: None,
                });
            }
        }
        tokio::select! {
            _ = sleep(Duration::from_secs(USDC_POLL_SECS)) => {}
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

/// Reduce los balances spot al resumen que pinta la Vista 8: USDC (total y
/// hold) aparte, y el resto de tokens con saldo > 0 solo enumerados.
pub fn spot_snapshot(
    addr: String,
    balances: &[UserTokenBalance],
    usdc_avail: Option<f64>,
) -> SpotSnapshot {
    let mut snap = SpotSnapshot {
        addr,
        usdc_total: 0.0,
        usdc_hold: 0.0,
        usdc_avail,
        others: Vec::new(),
    };
    for b in balances {
        let total = pf(&b.total);
        if b.coin == "USDC" {
            snap.usdc_total = total;
            snap.usdc_hold = pf(&b.hold);
        } else if total > 0.0 {
            snap.others.push((b.coin.clone(), total));
        }
    }
    snap
}

/// Base REST del /info según la red (el `get_url()` del SDK es pub(crate)).
fn info_api(base: BaseUrl) -> &'static str {
    match base {
        BaseUrl::Testnet => TESTNET_API_URL,
        _ => MAINNET_API_URL,
    }
}

/// POST de solo lectura a `{api}/info`.
async fn info_post(
    client: &reqwest::Client,
    api: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    client
        .post(format!("{api}/info"))
        .timeout(Duration::from_secs(10))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())
}

/// USDC disponible tras margen de mantenimiento del spotClearinghouseState
/// crudo: entrada `[0, "x.y"]` (token 0 = USDC) de
/// `tokenToAvailableAfterMaintenance`. None si la API no trae el campo.
fn usdc_avail_after_maint(v: &serde_json::Value) -> Option<f64> {
    v.get("tokenToAvailableAfterMaintenance")?
        .as_array()?
        .iter()
        .find_map(|e| {
            let pair = e.as_array()?;
            if pair.first()?.as_u64()? != 0 {
                return None;
            }
            pair.get(1)?.as_str()?.parse().ok()
        })
}

/// spotClearinghouseState crudo (no el del SDK: este trae también
/// `tokenToAvailableAfterMaintenance`, que el SDK pineado no expone).
async fn fetch_spot_state(
    client: &reqwest::Client,
    api: &str,
    user: &str,
) -> Result<(Vec<UserTokenBalance>, Option<f64>), String> {
    let v = info_post(
        client,
        api,
        serde_json::json!({"type": "spotClearinghouseState", "user": user}),
    )
    .await?;
    let balances: Vec<UserTokenBalance> = serde_json::from_value(v["balances"].clone())
        .map_err(|e| format!("balances ilegibles: {e}"))?;
    Ok((balances, usdc_avail_after_maint(&v)))
}

/// "unifiedAccount" → Unified; cualquier otro string ("default",
/// "portfolioMargin", …) → Standard, conservando el comportamiento clásico.
/// Solo el valor unificado cambia ramas: un valor desconocido nunca debe
/// deshabilitar funcionalidad por sí solo.
pub fn parse_account_mode(s: &str) -> AccountMode {
    if s == "unifiedAccount" {
        AccountMode::Unified
    } else {
        AccountMode::Standard(s.to_string())
    }
}

/// Modo de cuenta vía `userAbstraction` (/info, solo lectura). El SDK
/// pineado no lo expone — POST crudo. Verificado en vivo el 2026-07-20:
/// responde un string JSON plano ("unifiedAccount" / "default").
async fn fetch_account_mode(
    client: &reqwest::Client,
    api: &str,
    user: &str,
) -> Result<AccountMode, String> {
    let v = info_post(
        client,
        api,
        serde_json::json!({"type": "userAbstraction", "user": user}),
    )
    .await?;
    v.as_str()
        .map(parse_account_mode)
        .ok_or_else(|| format!("respuesta inesperada: {v}"))
}

/// ¿Es una dirección de SISTEMA de Hyperliquid (protocolo), y no la wallet de
/// una persona? Mover fondos de HyperCore al lado EVM se registra como un
/// `spotTransfer`/`send` normal hacia una de estas, así que el filtro por TIPO
/// de evento (que ya descarta bridge/liquidation/accountClassTransfer) no las
/// captura: hay que descartarlas por DIRECCIÓN o se listan como si fueran
/// contrapartes reales en las listas de wallets relacionadas (Vista 9).
///
/// Dos familias, ambas confirmadas contra ledgers reales de mainnet
/// (2026-08-16, barrido de 60 cuentas top del leaderboard):
/// - Puente por token: `0x2000…0000` + índice del token en los últimos bytes.
///   NO es una única dirección — se observaron 10 distintas, cada una con su
///   token coherente: `…0000` USDC (303 entradas), `…c5` UBTC, `…f1` FEUSD,
///   `…eb` USDE, `…dd` UETH, `…01` PURR, `…79` KHYPE… Por eso se reconoce la
///   familia entera (primer byte 0x20 + relleno de ceros, índice en los dos
///   últimos bytes) y no un literal: excluir solo la de USDC dejaría pasar
///   como "wallet relacionada" el puente de todos los demás tokens.
/// - HYPE nativo: `0x2222…2222`, caso especial fuera de esa numeración
///   (4 `spotTransfer` de HYPE observados en un ledger real).
fn es_direccion_de_sistema(addr: &str) -> bool {
    let lower = addr.to_ascii_lowercase();
    let h = lower.strip_prefix("0x").unwrap_or(&lower);
    if h.len() != 40 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    if h.chars().all(|c| c == '2') {
        return true; // HYPE nativo en HyperEVM
    }
    // 0x20 + ceros + índice de token en los dos últimos bytes.
    h.starts_with("20") && h[2..36].chars().all(|c| c == '0')
}

/// Reduce una entrada de `userNonFundingLedgerUpdates` a una transferencia con
/// contraparte, vista DESDE `me`. None = la entrada no relaciona a `me` con
/// otra wallet y no pinta nada en las listas de wallets relacionadas.
///
/// Formas verificadas contra la API real de mainnet el 2026-08-06 (una entrada
/// es `{time, hash, delta:{type, …}}`):
/// - `internalTransfer` / `subAccountTransfer`: `user`→`destination`, `usdc`.
/// - `spotTransfer` / `send`: `user`→`destination`, `token`, `amount`,
///   `usdcValue`. Un `send` de la cuenta a sí misma (mover entre dexes) se
///   descarta: no hay wallet relacionada.
/// - `vaultDeposit`/`vaultWithdraw`/`vaultDistribution`: la contraparte es el
///   `vault`, y el sentido lo marca el propio tipo.
/// - `deposit`/`withdraw` (bridge), `liquidation`, `accountClassTransfer`,
///   `spotGenesis`, `rewardsClaim`…: sin contraparte on-Hyperliquid → None.
fn parse_transfer(e: &serde_json::Value, me: &str) -> Option<TransferInfo> {
    let time_ms = e["time"].as_u64()?;
    let d = &e["delta"];
    let kind = d["type"].as_str()?.to_string();
    let num = |v: &serde_json::Value| v.as_str().and_then(|s| s.parse::<f64>().ok());
    let eq = |a: &str| a.eq_ignore_ascii_case(me);

    let (counterparty, incoming, token, amount, usd) = match kind.as_str() {
        "internalTransfer" | "subAccountTransfer" => {
            let from = d["user"].as_str()?;
            let to = d["destination"].as_str()?;
            let usdc = num(&d["usdc"])?;
            let incoming = eq(to);
            let other = if incoming { from } else { to };
            (
                other.to_string(),
                incoming,
                "USDC".to_string(),
                usdc,
                Some(usdc),
            )
        }
        "spotTransfer" | "send" => {
            let from = d["user"].as_str()?;
            let to = d["destination"].as_str()?;
            if eq(from) == eq(to) {
                return None; // traspaso a uno mismo (o entrada ajena)
            }
            let incoming = eq(to);
            let other = if incoming { from } else { to };
            (
                other.to_string(),
                incoming,
                d["token"].as_str().unwrap_or("?").to_string(),
                num(&d["amount"])?,
                num(&d["usdcValue"]),
            )
        }
        "vaultDeposit" | "vaultWithdraw" | "vaultDistribution" => {
            let vault = d["vault"].as_str()?;
            let usdc = num(&d["usdc"])?;
            let incoming = kind != "vaultDeposit";
            (
                vault.to_string(),
                incoming,
                "USDC".to_string(),
                usdc,
                Some(usdc),
            )
        }
        _ => return None,
    };
    // Una entrada que no involucra a la cuenta observada no debe colarse.
    if eq(&counterparty) {
        return None;
    }
    // Ni una dirección de sistema: no es la wallet de nadie (ver más abajo).
    if es_direccion_de_sistema(&counterparty) {
        return None;
    }
    Some(TransferInfo {
        counterparty,
        incoming,
        kind,
        token,
        amount,
        usd,
        time_ms,
    })
}

/// Transferencias con contraparte de `user` vía `userNonFundingLedgerUpdates`
/// (/info, solo lectura; el SDK pineado no lo expone → POST crudo). `startTime`
/// es obligatorio en la petición; se pide el historial completo (0). La
/// respuesta viene más ANTIGUA primero: se invierte para dejar lo reciente
/// arriba, igual que `userFills`.
async fn fetch_transfers(
    client: &reqwest::Client,
    api: &str,
    user: &str,
) -> Result<Vec<TransferInfo>, String> {
    let v = info_post(
        client,
        api,
        serde_json::json!({
            "type": "userNonFundingLedgerUpdates",
            "user": user,
            "startTime": 0,
        }),
    )
    .await?;
    let arr = v
        .as_array()
        .ok_or_else(|| format!("respuesta inesperada: {v}"))?;
    let mut out: Vec<TransferInfo> = arr.iter().filter_map(|e| parse_transfer(e, user)).collect();
    out.reverse();
    Ok(out)
}

/// Reduce una orden del array de `frontendOpenOrders` (forma verificada
/// contra la API real el 2026-07-20: coin, side B/A, limitPx, sz, oid,
/// orderType, isTrigger, triggerPx, reduceOnly, …). None = entrada ilegible
/// (mejor omitir una orden rara que pintar basura o reventar el watcher).
fn parse_open_order(o: &serde_json::Value) -> Option<types::LiveOrd> {
    let is_trigger = o["isTrigger"].as_bool().unwrap_or(false);
    let px_field = if is_trigger { "triggerPx" } else { "limitPx" };
    Some(types::LiveOrd {
        coin: o["coin"].as_str()?.to_string(),
        is_buy: o["side"].as_str()? == "B",
        kind: o["orderType"].as_str().unwrap_or("Limit").to_string(),
        px: o[px_field].as_str()?.parse().ok()?,
        sz: o["sz"].as_str()?.parse().ok()?,
        oid: o["oid"].as_u64()?,
        reduce_only: o["reduceOnly"].as_bool().unwrap_or(false),
        is_trigger,
    })
}

/// Órdenes abiertas REALES vía `frontendOpenOrders` (no el `openOrders` del
/// SDK: este trae tipo de orden y triggers, imprescindible para distinguir
/// SL/TP de límites en el panel de ejecución).
pub(crate) async fn fetch_open_orders(
    client: &reqwest::Client,
    api: &str,
    user: &str,
) -> Result<Vec<types::LiveOrd>, String> {
    let v = info_post(
        client,
        api,
        serde_json::json!({"type": "frontendOpenOrders", "user": user}),
    )
    .await?;
    let arr = v
        .as_array()
        .ok_or_else(|| format!("respuesta inesperada: {v}"))?;
    Ok(arr.iter().filter_map(parse_open_order).collect())
}

/// Watcher de órdenes abiertas de la cuenta de TRADING (paso 7) — dirección
/// fija (la maestra del agent), solo se lanza con el trading real armado.
/// El trader además emite el mismo mensaje tras cada acción (refresh
/// inmediato); este ciclo cubre cambios externos (fills, triggers saltados).
pub fn spawn_orders_watcher(base: BaseUrl, tx: UnboundedSender<DataMsg>, master: Address) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let api = info_api(base);
        let user = format!("{master}");
        loop {
            match fetch_open_orders(&client, api, &user).await {
                Ok(orders) => {
                    let _ = tx.send(DataMsg::OpenOrders {
                        addr: user.clone(),
                        orders,
                    });
                }
                Err(e) => {
                    let _ = tx.send(DataMsg::RestError(format!("órdenes: {e}")));
                }
            }
            sleep(Duration::from_secs(WALLET_POLL_SECS)).await;
        }
    });
}

/// Saldo SPOT dentro de Hyperliquid (spotClearinghouseState) + modo de
/// cuenta (userAbstraction) de la maestra WC — separado a propósito del
/// clearinghouseState de perps: son dos saldos distintos y el faucet de
/// testnet acredita en SPOT (confusión de 2026-07-20). El modo de cuenta va
/// en el mismo ciclo porque decide cuál de los dos saldos es significativo
/// (cuenta unificada: solo el spot). Reusa el target (addr, chain) del canal
/// del USDC on-chain: la chain aquí no decide nada (el endpoint viene de
/// `base`), solo interesa la dirección.
async fn spot_watcher(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    mut rx: watch::Receiver<Option<(Address, String)>>,
) {
    let client = reqwest::Client::new();
    let api = info_api(base);
    loop {
        let target = rx.borrow_and_update().clone();
        let Some((addr, _)) = target else {
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        };
        let user = format!("{addr}");
        match fetch_spot_state(&client, api, &user).await {
            Ok((balances, avail)) => {
                let _ = tx.send(DataMsg::SpotState(spot_snapshot(
                    user.clone(),
                    &balances,
                    avail,
                )));
            }
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("spot: {e}")));
            }
        }
        match fetch_account_mode(&client, api, &user).await {
            Ok(mode) => {
                let _ = tx.send(DataMsg::AccountMode { addr: user, mode });
            }
            Err(e) => {
                let _ = tx.send(DataMsg::RestError(format!("modo de cuenta: {e}")));
            }
        }
        tokio::select! {
            _ = sleep(Duration::from_secs(WALLET_POLL_SECS)) => {}
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

/// Observa las cuentas pedidas por la UI — la watch-only de la Vista 9 y la
/// maestra WC de la Vista 8 — con un clearinghouseState por dirección y ciclo.
async fn wallet_watcher(
    base: BaseUrl,
    tx: UnboundedSender<DataMsg>,
    mut rx: watch::Receiver<Vec<Address>>,
) {
    let info = new_client_retrying(base, &tx).await;
    let http = reqwest::Client::new();
    let api = info_api(base);
    // Última vez que se pidió userFills por dirección, para espaciarlo (60s)
    // respecto al clearinghouseState (10s) — el historial cambia despacio.
    let mut fills_at: std::collections::HashMap<Address, Instant> =
        std::collections::HashMap::new();
    // Igual para la reconstrucción de aperturas, que es mucho más cara, y las
    // posiciones del último snapshot (entrada del resolver).
    let mut opens_at: std::collections::HashMap<Address, Instant> =
        std::collections::HashMap::new();
    let mut last_positions: std::collections::HashMap<Address, Vec<(String, f64, f64)>> =
        std::collections::HashMap::new();
    loop {
        let targets = rx.borrow_and_update().clone();
        if targets.is_empty() {
            if rx.changed().await.is_err() {
                return;
            }
            continue;
        }
        // Purga direcciones que ya no se observan para no filtrar memoria.
        fills_at.retain(|a, _| targets.contains(a));
        opens_at.retain(|a, _| targets.contains(a));
        last_positions.retain(|a, _| targets.contains(a));
        // la watch-only de la Vista 9 es la PRIMERA de la lista de targets
        // (`push_wallet_targets` la pone antes que la maestra WC); la maestra
        // no necesita este barrido: la Vista 8 no muestra antigüedad.
        let watch_only = targets.first().copied();
        for addr in targets {
            match info.user_state(addr).await {
                Ok(st) => {
                    let snap = account_snapshot(format!("{addr}"), &st);
                    last_positions.insert(
                        addr,
                        snap.positions
                            .iter()
                            .map(|p| (p.coin.clone(), p.szi, p.since_open_funding))
                            .collect(),
                    );
                    let _ = tx.send(DataMsg::WalletState(snap));
                }
                Err(e) => {
                    let _ = tx.send(DataMsg::RestError(format!("wallet: {e}")));
                }
            }
            // Historial de operaciones, refrescado más espaciado (o al instante
            // la primera vez que se ve la dirección, tras un cambio de `e`).
            let due = fills_at
                .get(&addr)
                .is_none_or(|t| t.elapsed() > FILLS_REFRESH);
            if due {
                if let Ok(raw) = info.user_fills(addr).await {
                    let fills = raw.iter().map(parse_fill).collect();
                    let _ = tx.send(DataMsg::WalletFills {
                        addr: format!("{addr}"),
                        fills,
                    });
                }
                // Wallets relacionadas: mismo ritmo que los fills (el ledger de
                // transferencias cambia aún más despacio que las operaciones).
                let user = format!("{addr}");
                match fetch_transfers(&http, api, &user).await {
                    Ok(transfers) => {
                        let _ = tx.send(DataMsg::WalletTransfers {
                            addr: user,
                            transfers,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(DataMsg::RestError(format!("ledger: {e}")));
                    }
                }
                fills_at.insert(addr, Instant::now());
            }
            // Reconstrucción de aperturas: decenas de peticiones, así que va en
            // su propia tarea (no bloquea el ciclo de 10s) y muy espaciada. Solo
            // para la watch-only: la maestra WC comparte watcher pero la Vista 8
            // no muestra antigüedad.
            let opens_due = opens_at
                .get(&addr)
                .is_none_or(|t| t.elapsed() > OPENS_REFRESH);
            if opens_due && watch_only.as_ref() == Some(&addr) {
                if let Some(pos) = last_positions.get(&addr) {
                    if !pos.is_empty() {
                        opens_at.insert(addr, Instant::now());
                        let (c, a, u, tx2, pos) = (
                            http.clone(),
                            api.to_string(),
                            format!("{addr}"),
                            tx.clone(),
                            pos.clone(),
                        );
                        tokio::spawn(async move {
                            let now = now_ms();
                            let opens = opens::resolve(&c, &a, &u, &pos, now).await;
                            let _ = tx2.send(DataMsg::WalletOpens { addr: u, opens });
                        });
                    }
                }
            }
        }
        tokio::select! {
            _ = sleep(Duration::from_secs(WALLET_POLL_SECS)) => {}
            r = rx.changed() => {
                if r.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La tira de fallos de whales debe llevar el error real y las direcciones,
    /// no solo el conteo — sin eso el "65/100 cuentas fallaron" no se puede
    /// diagnosticar salvo reproduciéndolo.
    #[test]
    fn whale_err_msg_incluye_direcciones_y_error() {
        let s = vec![
            "0xaaa: 429 Too Many Requests".to_string(),
            "0xbbb: timeout".to_string(),
        ];
        let m = whale_err_msg(65, 100, &s);
        assert!(m.starts_with("whales: 65/100 cuentas fallaron"));
        assert!(m.contains("0xaaa: 429 Too Many Requests"));
        assert!(m.contains("0xbbb: timeout"));
        assert!(m.contains("(+63 más)"));
        // sin muestras (caso imposible hoy, pero no debe inventar sufijos)
        assert_eq!(whale_err_msg(2, 100, &[]), "whales: 2/100 cuentas fallaron");
        // si se detallan todos, no se añade el "+N más"
        assert!(!whale_err_msg(2, 100, &s).contains("más"));
    }

    /// Reducción del spotClearinghouseState real (forma verificada contra
    /// testnet el 2026-07-20): USDC separado con su hold, otros tokens solo
    /// si tienen saldo, y los saldos a cero no ensucian la lista.
    #[test]
    fn spot_snapshot_reduccion() {
        let bal = |coin: &str, total: &str, hold: &str| UserTokenBalance {
            coin: coin.into(),
            hold: hold.into(),
            total: total.into(),
            entry_ntl: "0.0".into(),
        };
        let s = spot_snapshot(
            "0xdead".into(),
            &[
                bal("USDC", "999.0", "1.5"),
                bal("TZERO", "0.0", "0.0"),
                bal("HORSE", "12.0", "0.0"),
            ],
            Some(997.5),
        );
        assert_eq!(s.usdc_total, 999.0);
        assert_eq!(s.usdc_hold, 1.5);
        assert_eq!(s.usdc_avail, Some(997.5));
        assert_eq!(s.others, vec![("HORSE".to_string(), 12.0)]);

        // sin USDC en la respuesta: 0.0 honesto, no basura
        let s = spot_snapshot("0xdead".into(), &[bal("TZERO", "0.0", "0.0")], None);
        assert_eq!(s.usdc_total, 0.0);
        assert!(s.usdc_avail.is_none());
        assert!(s.others.is_empty());
    }

    /// Reducción de `userNonFundingLedgerUpdates` con entradas COPIADAS de la
    /// respuesta real de mainnet (2026-08-06): solo salen los movimientos con
    /// contraparte, con el sentido visto desde la cuenta observada.
    #[test]
    fn transferencias_solo_con_contraparte() {
        let me = "0xC272Fa7d73E8ed66E65A6281570d3788beA5E7A4";
        let raw = serde_json::json!([
            {"time": 1708226524752u64, "hash": "0x17", "delta": {"type": "deposit", "usdc": "5.0"}},
            {"time": 1710245187638u64, "hash": "0x06", "delta": {"type": "internalTransfer",
                "usdc": "1.0", "user": "0xc272fa7d73e8ed66e65a6281570d3788bea5e7a4",
                "destination": "0x2df1c51e09aecf9cacb7bc98cb1742757f163df7", "fee": "0.0"}},
            {"time": 1713367479014u64, "hash": "0x8f", "delta": {"type": "accountClassTransfer",
                "usdc": "0.954857", "toPerp": false}},
            {"time": 1742149624653u64, "hash": "0x45", "delta": {"type": "spotTransfer",
                "token": "FLY", "amount": "2000.0", "usdcValue": "3.408",
                "user": "0x0168985218db3c45d8271ee48466ed93a5df873a",
                "destination": "0xc272fa7d73e8ed66e65a6281570d3788bea5e7a4", "fee": "0.0"}},
            {"time": 1764813982494u64, "hash": "0x4d", "delta": {"type": "send",
                "user": "0xc272fa7d73e8ed66e65a6281570d3788bea5e7a4",
                "destination": "0xc272fa7d73e8ed66e65a6281570d3788bea5e7a4",
                "token": "USDC", "amount": "0.15", "usdcValue": "0.15"}},
        ]);
        let out: Vec<TransferInfo> = raw
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| parse_transfer(e, me))
            .collect();

        // depósito de bridge, accountClassTransfer y el send a uno mismo caen.
        assert_eq!(out.len(), 2);
        let sent = &out[0];
        assert_eq!(sent.kind, "internalTransfer");
        assert!(!sent.incoming);
        assert_eq!(
            sent.counterparty,
            "0x2df1c51e09aecf9cacb7bc98cb1742757f163df7"
        );
        assert_eq!(sent.amount, 1.0);

        let recv = &out[1];
        assert!(recv.incoming);
        assert_eq!(recv.token, "FLY");
        assert_eq!(recv.amount, 2000.0);
        assert_eq!(recv.usd, Some(3.408));
        assert_eq!(
            recv.counterparty,
            "0x0168985218db3c45d8271ee48466ed93a5df873a"
        );
    }

    /// Un depósito a vault se cuenta como fondos ENVIADOS al vault, y una
    /// retirada/reparto como recibidos — el sentido lo marca el tipo, no un
    /// par user/destination (que estas entradas no traen).
    #[test]
    fn transferencias_de_vault_toman_el_sentido_del_tipo() {
        let me = "0xC272Fa7d73E8ed66E65A6281570d3788beA5E7A4";
        let ent = |t: &str| {
            serde_json::json!({"time": 1, "hash": "0x1", "delta": {"type": t,
                "vault": "0xdfc24b077bc1425ad1dea75bcb6f8158e10df303", "usdc": "10.0"}})
        };
        assert!(!parse_transfer(&ent("vaultDeposit"), me).unwrap().incoming);
        assert!(parse_transfer(&ent("vaultWithdraw"), me).unwrap().incoming);
        assert!(
            parse_transfer(&ent("vaultDistribution"), me)
                .unwrap()
                .incoming
        );
    }

    /// Las direcciones de sistema no son wallets relacionadas. Los casos
    /// positivos son direcciones REALES observadas en ledgers de mainnet
    /// (barrido de 60 cuentas del leaderboard, 2026-08-16); los negativos
    /// incluyen direcciones normales que empiezan por 0x20 o por 0x2222 sin
    /// serlo, para que el filtro no se lleve por delante a gente de verdad.
    #[test]
    fn direcciones_de_sistema_no_son_contrapartes() {
        for a in [
            "0x2000000000000000000000000000000000000000", // USDC
            "0x20000000000000000000000000000000000000c5", // UBTC
            "0x20000000000000000000000000000000000000f1", // FEUSD
            "0x2000000000000000000000000000000000000001", // PURR
            "0x2222222222222222222222222222222222222222", // HYPE nativo
            "0x2000000000000000000000000000000000000000"
                .to_uppercase()
                .as_str(),
        ] {
            assert!(es_direccion_de_sistema(a), "debería ser de sistema: {a}");
        }
        for a in [
            "0x2000000000000000000000000000000000010000", // ceros rotos: wallet
            "0x2222222222222222222222222222222222222223",
            "0x20f496c9486be5924a93d67e98298733bb47057c",
            "0xf3f496c9486be5924a93d67e98298733bb47057c",
            "",
            "0x20",
        ] {
            assert!(!es_direccion_de_sistema(a), "NO es de sistema: {a}");
        }
    }

    /// Un `spotTransfer` hacia el puente HyperCore↔HyperEVM se descarta entero,
    /// aunque sea un tipo de evento con contraparte válida. Entrada calcada de
    /// una real de mainnet (HYPE al sistema 0x2222…2222).
    #[test]
    fn transferencia_al_puente_evm_no_es_wallet_relacionada() {
        let me = "0x0168985218db3c45d8271ee48466ed93a5df873a";
        let ent = |dest: &str| {
            serde_json::json!({"time": 1, "hash": "0x1", "delta": {"type": "spotTransfer",
                "user": me, "destination": dest, "token": "HYPE",
                "amount": "2.0", "usdcValue": "80.0"}})
        };
        assert!(parse_transfer(&ent("0x2222222222222222222222222222222222222222"), me).is_none());
        assert!(parse_transfer(&ent("0x20000000000000000000000000000000000000c5"), me).is_none());
        // una contraparte real sigue apareciendo
        assert!(parse_transfer(&ent("0xc272fa7d73e8ed66e65a6281570d3788bea5e7a4"), me).is_some());
    }

    /// Sonda real contra mainnet: el endpoint existe, responde una lista y sus
    /// entradas se reducen sin panic. Ignorada por defecto (toca la red).
    #[tokio::test]
    #[ignore]
    async fn ledger_real_mainnet() {
        let user = "0xf3f496c9486be5924a93d67e98298733bb47057c";
        let out = fetch_transfers(&reqwest::Client::new(), MAINNET_API_URL, user)
            .await
            .expect("userNonFundingLedgerUpdates responde");
        println!("{} transferencias con contraparte", out.len());
        for t in out.iter().take(5) {
            println!("{t:?}");
        }
    }

    /// El modo de cuenta según la respuesta real de userAbstraction
    /// (verificada en vivo 2026-07-20): SOLO "unifiedAccount" activa la rama
    /// unificada; cualquier otro valor conserva el comportamiento clásico.
    #[test]
    fn modo_de_cuenta_solo_unifica_con_el_valor_exacto() {
        assert_eq!(parse_account_mode("unifiedAccount"), AccountMode::Unified);
        assert_eq!(
            parse_account_mode("default"),
            AccountMode::Standard("default".into())
        );
        assert_eq!(
            parse_account_mode("portfolioMargin"),
            AccountMode::Standard("portfolioMargin".into())
        );
    }

    /// Extracción del disponible tras mantenimiento con la forma EXACTA de la
    /// respuesta real de mainnet (2026-07-20): pares [token, "cantidad"].
    #[test]
    fn disponible_tras_mantenimiento_token_usdc() {
        let v = serde_json::json!({
            "balances": [],
            "tokenToAvailableAfterMaintenance": [[1, "3.0"], [0, "5.000708"]],
        });
        assert_eq!(usdc_avail_after_maint(&v), Some(5.000708));
        // sin el campo (cuenta estándar) o sin token 0: None honesto
        assert_eq!(
            usdc_avail_after_maint(&serde_json::json!({"balances": []})),
            None
        );
        let v = serde_json::json!({"tokenToAvailableAfterMaintenance": [[7, "9.0"]]});
        assert_eq!(usdc_avail_after_maint(&v), None);
    }

    #[test]
    fn calldata_balanceof_formato() {
        let a: Address = "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7"
            .parse()
            .unwrap();
        let d = balanceof_calldata(a);
        assert_eq!(d.len(), 2 + 8 + 64);
        assert!(d.starts_with("0x70a08231000000000000000000000000a877bf18"));
        assert!(d.ends_with("fe78fa7"));
    }

    #[test]
    fn abi_word_parse() {
        assert!(abi_word_f64("0x").is_none()); // eth_call a dirección sin código
        assert!(abi_word_f64("0xzz").is_none());
        assert!(abi_word_f64("06").is_none()); // sin prefijo 0x
        assert_eq!(abi_word_f64("0x06"), Some(6.0));
        let w = format!("0x{:0>64}", "5f5e100"); // 1e8 (100 USDC en 6 dec)
        assert_eq!(abi_word_f64(&w), Some(1e8));
    }

    /// El bridge y el token del depósito, byte a byte contra lo verificado
    /// en la doc de Bridge2 — y NUNCA una ruta para chains sin verificar.
    #[test]
    fn ruta_de_deposito_solo_mainnet() {
        let r = deposit_route("eip155:42161").unwrap();
        assert_eq!(r.bridge, "0x2df1c51e09aecf9cacb7bc98cb1742757f163df7");
        assert_eq!(r.usdc, "0xaf88d065e77c8cc2239327c5edb3a432268e5831");
        assert!(deposit_route("eip155:421614").is_none());
        assert!(deposit_route("eip155:1").is_none());
    }

    /// El retiro tiene ruta en AMBAS redes (misma función, endpoint por
    /// chain de sesión) — y nunca en chains sin verificar.
    #[test]
    fn ruta_de_retiro_por_red() {
        let m = withdraw_route("eip155:42161").unwrap();
        assert_eq!(m.api, "https://api.hyperliquid.xyz");
        assert_eq!(m.hl_chain, "Mainnet");
        assert_eq!(m.chain_id, 42_161);
        assert_eq!(m.usdc, "0xaf88d065e77c8cc2239327c5edb3a432268e5831");

        let t = withdraw_route("eip155:421614").unwrap();
        assert_eq!(t.api, "https://api.hyperliquid-testnet.xyz");
        assert_eq!(t.hl_chain, "Testnet");
        assert_eq!(t.chain_id, 421_614);
        // el mock USDC del bridge de testnet, NO el de Circle de la Pieza 1
        assert_eq!(t.usdc, "0x1baabb04529d43a73232b713c0fe471f7c7334d5");

        assert!(withdraw_route("eip155:1").is_none());
    }

    #[test]
    fn usdc_net_mapeo() {
        let (_, c) = usdc_net("eip155:42161").unwrap();
        assert_eq!(c, "0xaf88d065e77c8cc2239327c5edb3a432268e5831");
        assert!(usdc_net("eip155:1").is_none());
    }

    /// Parseo de frontendOpenOrders con la forma EXACTA capturada de la API
    /// real (2026-07-20): límite normal + trigger SL reduce-only; una entrada
    /// ilegible se omite sin tumbar el resto.
    #[test]
    fn parse_open_orders_forma_real() {
        let limit = serde_json::json!({
            "coin": "SOL", "side": "B", "limitPx": "75.904", "sz": "5.71",
            "oid": 499130054099u64, "timestamp": 1784527958399u64,
            "triggerCondition": "N/A", "isTrigger": false, "triggerPx": "0.0",
            "children": [], "isPositionTpsl": false, "reduceOnly": false,
            "orderType": "Limit", "origSz": "5.71", "tif": "Alo", "cloid": null
        });
        let o = parse_open_order(&limit).unwrap();
        assert!(o.is_buy && !o.is_trigger && !o.reduce_only);
        assert_eq!(o.px, 75.904);
        assert_eq!(o.oid, 499_130_054_099);
        assert!(!o.is_close_trigger() && !o.is_sl() && !o.is_tp());

        let sl = serde_json::json!({
            "coin": "BTC", "side": "A", "limitPx": "95000.0", "sz": "0.01",
            "oid": 7u64, "isTrigger": true, "triggerPx": "95000.0",
            "reduceOnly": true, "orderType": "Stop Market"
        });
        let o = parse_open_order(&sl).unwrap();
        assert!(o.is_trigger && o.reduce_only && o.is_close_trigger());
        assert!(o.is_sl() && !o.is_tp());
        assert_eq!(o.px, 95_000.0, "el precio de un trigger es triggerPx");

        let tp = serde_json::json!({
            "coin": "BTC", "side": "A", "limitPx": "120000.0", "sz": "0.01",
            "oid": 8u64, "isTrigger": true, "triggerPx": "120000.0",
            "reduceOnly": true, "orderType": "Take Profit Market"
        });
        assert!(parse_open_order(&tp).unwrap().is_tp());

        // ilegible (sin oid): se omite
        assert!(parse_open_order(&serde_json::json!({"coin": "X"})).is_none());
    }

    /// Contra la API real de AMBAS redes con la cuenta real del usuario
    /// (necesita red): valida que el código de producción del modo de cuenta
    /// y del spot state (con disponible tras mantenimiento) parsea lo que la
    /// API devuelve de verdad. `cargo test modo_de_cuenta_real -- --ignored
    /// --nocapture`
    #[tokio::test]
    #[ignore]
    async fn modo_de_cuenta_real_ambas_redes() {
        let client = reqwest::Client::new();
        let user = "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7";
        for base in [BaseUrl::Mainnet, BaseUrl::Testnet] {
            let api = info_api(base);
            let mode = fetch_account_mode(&client, api, user).await.unwrap();
            let (balances, avail) = fetch_spot_state(&client, api, user).await.unwrap();
            let snap = spot_snapshot(user.to_string(), &balances, avail);
            eprintln!(
                "{api}: modo {mode:?} · USDC spot {:.6} · disp. tras mant. {avail:?}",
                snap.usdc_total
            );
            // la cuenta real está en modo unificado en ambas redes
            // (verificado por primera vez el 2026-07-20)
            assert_eq!(mode, AccountMode::Unified);
            assert!(snap.usdc_total > 0.0, "la cuenta real tiene saldo spot");
            assert!(avail.is_some(), "unificada debe reportar el disponible");
        }
    }

    /// Contra el RPC real de Arbitrum (necesita red):
    /// `cargo test usdc_real_mainnet -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn usdc_real_mainnet() {
        let (rpc, contract) = usdc_net("eip155:42161").unwrap();
        let addr: Address = "0xa877Bf18FCd88c3D919b2f7351d8612A7Fe78Fa7"
            .parse()
            .unwrap();
        let v = fetch_usdc_balance(&reqwest::Client::new(), &rpc, contract, addr)
            .await
            .unwrap();
        assert!(v.is_finite() && v >= 0.0);
        eprintln!("saldo USDC on-chain de {addr}: {v:.2}");
    }
}
