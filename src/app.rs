use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use alloy_primitives::Address;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::widgets::TableState;
use tokio::sync::{mpsc, watch};

use crate::data::types::{
    AccountMode, AccountSnapshot, CandlePoint, CtxSnapshot, DataMsg, ExtraReq, FillInfo, Interval,
    LiveOrd, PairMeta, SpotSnapshot, WhaleInfo,
};
use crate::exec::{self, Confirm, ExecState, Focus, Hit, SlTpEdit};
use crate::trader::{ExecEvent, TraderCmd};
use crate::liqdens::LiqBar;
use crate::signals::{self, Dmi, Regime};
use crate::ui::fmt::fmt_px;
use crate::ui::oscimg::Gfx;
use crate::wallet::walletconnect::{
    self, AgentReq, AgentStatus, DepositReq, DepositStatus, TransferReq, TransferStatus, WcCmd,
    WcStatus, WithdrawReq, WithdrawStatus,
};
use crate::{flow, liq, search};

/// ~1h de historia de contextos a 1 muestra / 5s.
pub const HIST_CAP: usize = 720;
/// ~30min de mids WS a 1 muestra / 2s.
pub const MID_HIST_CAP: usize = 900;
pub const MID_THROTTLE: Duration = Duration::from_secs(2);
pub const OI_WIN_SHORT: Duration = Duration::from_secs(5 * 60);
pub const OI_WIN_LONG: Duration = Duration::from_secs(60 * 60);
/// Cadencia de re-fetch de velas/funding mientras Vista 2 o 3 están activas:
/// más frecuente en temporalidades cortas (la vela en curso cambia rápido),
/// más laxa en las largas (re-pedir 1d cada pocos segundos sería ruido).
fn extra_ttl(iv: Interval) -> Duration {
    Duration::from_secs(match iv {
        Interval::M1 => 5,
        Interval::M5 => 10,
        Interval::M15 => 20,
        Interval::H1 => 30,
        Interval::H4 => 60,
        Interval::H12 => 120,
        Interval::D1 => 180,
    })
}
/// Debounce de peticiones extra en vuelo: evita re-encolar la misma petición
/// en cada tick del bucle de UI mientras la respuesta aún no ha llegado.
const EXTRA_INFLIGHT: Duration = Duration::from_secs(3);
/// Sin datos WS de un par en este margen, el mid del par se considera stale y
/// el poller REST vuelve a alimentarlo (allMids llega cada ~5s; 3× margen).
pub const MID_STALE: Duration = Duration::from_secs(15);
/// Rangos de precio del mapa de liquidaciones (±%).
const LIQ_RANGES: [f64; 3] = [0.05, 0.15, 0.30];
/// Buckets de OI por cierre de vela retenidos por par e intervalo
/// (la señal de densidad usa hasta 180: 60 de warmup + 120 de lookback).
const OI_CANDLE_CAP: usize = 200;
/// Muestreo del historial lento de la Vista 6 (ventanas 1h/4h/24h).
const SLOW_THROTTLE: Duration = Duration::from_secs(60);
/// ~25h de muestras lentas: ventana máxima 24h + margen. Como el resto del
/// historial, vive en memoria: las ventanas largas requieren uptime.
const SLOW_HIST_CAP: usize = 1500;
/// Rango de la asimetría de liquidaciones de la Vista 6 (±3%).
const FLOW_LIQ_RANGE: f64 = 0.03;
const FLOW_LIQ_BUCKETS: usize = 60;
/// Ventana de la divergencia CVD/precio y del premium sostenido.
pub const FLOW_CVD_WIN: Duration = Duration::from_secs(15 * 60);
pub const FLOW_PREM_WIN: Duration = Duration::from_secs(60 * 60);
/// El CVD de la ventana debe superar este % del volumen típico de la misma
/// ventana para considerarse agresión significativa.
const CVD_MIN_VOL_FRAC: f64 = 0.05;
const CVD_THROTTLE: Duration = Duration::from_secs(2);
/// ~30min de curva CVD (basta para la ventana de divergencia de 15m).
const CVD_HIST_CAP: usize = 900;

/// Ventanas de la rotación de capital (Vista 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowWin {
    H1,
    H4,
    D1,
}

impl FlowWin {
    pub const ALL: [FlowWin; 3] = [FlowWin::H1, FlowWin::H4, FlowWin::D1];

    pub fn dur(&self) -> Duration {
        match self {
            FlowWin::H1 => Duration::from_secs(3600),
            FlowWin::H4 => Duration::from_secs(4 * 3600),
            FlowWin::D1 => Duration::from_secs(24 * 3600),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FlowWin::H1 => "1h",
            FlowWin::H4 => "4h",
            FlowWin::D1 => "24h",
        }
    }

    pub fn next(self) -> Self {
        match self {
            FlowWin::H1 => FlowWin::H4,
            FlowWin::H4 => FlowWin::D1,
            FlowWin::D1 => FlowWin::H1,
        }
    }
}

/// Modos de orden de la tabla de rotación (Vista 6). Sea cual sea el modo,
/// los pares sin dato para la clave van al final — nunca ordenados como cero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowSort {
    /// |ΔOI$| de la ventana activa: dónde se mueve más dinero (el original).
    Rotation,
    /// Asimetría de combustible de liqs ±3%: positivo = combustible abajo =
    /// sesgo bajista arriba de la tabla (r invierte para ver el lado alcista).
    Fuel,
    /// Confluencia: score compuesto y combustible apuntando al mismo lado.
    Confluence,
}

impl FlowSort {
    pub fn next(self) -> Self {
        match self {
            FlowSort::Rotation => FlowSort::Fuel,
            FlowSort::Fuel => FlowSort::Confluence,
            FlowSort::Confluence => FlowSort::Rotation,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            FlowSort::Rotation => crate::i18n::t().fs_rotation,
            FlowSort::Fuel => crate::i18n::t().fs_fuel,
            FlowSort::Confluence => crate::i18n::t().fs_confluence,
        }
    }
}

/// CVD del par seleccionado (canal `trades`). El acumulado solo tiene sentido
/// desde que empezó el tracking, así que se resetea al cambiar de par.
pub struct CvdState {
    pub coin: String,
    /// Σ (compras agresoras − ventas agresoras) en USD desde `since`.
    pub cum: f64,
    pub since: Instant,
    /// Curva (t, cum) muestreada cada ~2s para la ventana de divergencia.
    pub hist: VecDeque<(Instant, f64)>,
    pub last_trade_at: Option<Instant>,
}

impl CvdState {
    fn new(coin: String) -> Self {
        let now = Instant::now();
        Self {
            coin,
            cum: 0.0,
            since: now,
            hist: VecDeque::from([(now, 0.0)]),
            last_trade_at: None,
        }
    }

    fn push(&mut self, buy_ntl: f64, sell_ntl: f64) {
        let now = Instant::now();
        self.cum += buy_ntl - sell_ntl;
        self.last_trade_at = Some(now);
        let push = self
            .hist
            .back()
            .is_none_or(|(t, _)| now.duration_since(*t) >= CVD_THROTTLE);
        if push {
            self.hist.push_back((now, self.cum));
            while self.hist.len() > CVD_HIST_CAP {
                self.hist.pop_front();
            }
        }
    }
}

/// Máximo de buckets de 1 min del delta por vela (~48h; una vela de 1d cubre
/// 1440). El minuto es el divisor común de todas las temporalidades: una vela
/// suma sus minutos, así el mismo acumulado sirve para cualquier TF sin
/// re-bucketizar.
const DELTA_MIN_CAP: usize = 2880;

/// Delta por vela del par seleccionado (mismo canal `trades` que el CVD):
/// volumen comprador vs. vendedor agresor agregado por minuto, para pintar una
/// barra por vela bajo las velas de la Vista 2. Solo tiene sentido desde que
/// empezó el tracking (igual que el CVD), así que se resetea al cambiar de par.
pub struct DeltaState {
    pub coin: String,
    /// (minuto_ms alineado, compra_ntl, venta_ntl), ascendente por minuto.
    mins: VecDeque<(u64, f64, f64)>,
    start: Instant,
    /// Segundos de actividad desde `start` en el último trade — clave estable
    /// de caché del raster entre trades (evita re-rasterizar por frame).
    last_secs: u64,
}

impl DeltaState {
    fn new(coin: String) -> Self {
        Self {
            coin,
            mins: VecDeque::new(),
            start: Instant::now(),
            last_secs: 0,
        }
    }

    fn push(&mut self, buy_ntl: f64, sell_ntl: f64, t_ms: u64) {
        self.last_secs = self.start.elapsed().as_secs();
        let min = t_ms - t_ms % 60_000;
        match self.mins.back_mut() {
            Some((m, b, s)) if *m == min => {
                *b += buy_ntl;
                *s += sell_ntl;
            }
            // batches fuera de orden (min anterior al último) se descartan: el
            // WS entrega los trades en orden y el desfase sería de <1 batch
            Some((m, ..)) if *m > min => {}
            _ => {
                self.mins.push_back((min, buy_ntl, sell_ntl));
                while self.mins.len() > DELTA_MIN_CAP {
                    self.mins.pop_front();
                }
            }
        }
    }

    /// Delta neto (compra − venta, USD) por vela: alinea los buckets de minuto a
    /// la ventana [apertura, cierre] de cada vela. `None` si la vela empezó
    /// antes de que arrancara el tracking (warmup honesto, como el CVD) — nunca
    /// se muestra un delta parcial como si fuera completo.
    pub fn per_candle(&self, candles: &[CandlePoint], iv_ms: u64) -> Vec<Option<f64>> {
        let first_min = self.mins.front().map(|(m, ..)| *m);
        candles
            .iter()
            .map(|c| {
                let first = first_min?;
                // apertura de la vela alineada al minuto (todas las TF de HL son
                // múltiplos de 1 min y cierran en frontera de intervalo)
                let open = c.t_close.saturating_sub(iv_ms).saturating_add(1);
                let lo = open - open % 60_000;
                if lo < first {
                    return None; // vela previa o parcialmente previa al tracking
                }
                let net: f64 = self
                    .mins
                    .iter()
                    .filter(|(m, ..)| *m >= lo && *m <= c.t_close)
                    .map(|(_, b, s)| b - s)
                    .sum();
                Some(net)
            })
            .collect()
    }

    /// Clave de caché del raster: cambia al llegar trades nuevos (por segundo),
    /// al cambiar de TF o al desplazarse la ventana visible.
    pub fn raster_key(&self, iv_ms: u64, start: usize, len: usize) -> u64 {
        self.last_secs
            ^ iv_ms.rotate_left(17)
            ^ ((start as u64) << 40)
            ^ ((len as u64) << 20)
    }
}

pub struct HistPoint {
    pub t: Instant,
    pub mark: f64,
    pub oi: f64,
}

/// Muestra downsampleada (1/min) para las ventanas largas de la Vista 6.
pub struct SlowPoint {
    pub t: Instant,
    pub oi: f64,
    pub mark: f64,
    /// Volumen notional rolling 24h en el instante de la muestra.
    pub vol24: f64,
    pub premium_bps: Option<f64>,
}

/// Punto más cercano a `window` atrás en una serie temporal ordenada; None si
/// la serie no cubre aún ~3/4 de la ventana (mismo criterio que hist_point_at:
/// el historial es en memoria y las ventanas muestran "—" hasta acumularse).
fn point_at<T>(dq: &VecDeque<T>, window: Duration, t_of: impl Fn(&T) -> Instant) -> Option<&T> {
    let target = Instant::now().checked_sub(window)?;
    let front = dq.front()?;
    if t_of(front) > target + window / 4 {
        return None;
    }
    dq.iter().find(|h| t_of(h) >= target)
}

pub struct PairExtraData {
    pub interval: Interval,
    pub candles: Vec<CandlePoint>,
    pub funding_hist: Vec<(u64, f64)>,
    pub rsi: Option<f64>,
    pub dmi: Option<Dmi>,
    /// Series del panel whales+RSI (Vista 3); `rsi`/`dmi` de arriba son sus
    /// últimos valores — un único cálculo de TA compartido con Vista 2.
    pub panel: signals::WhalePanel,
    pub fetched: Instant,
    /// Solo avanza cuando las velas realmente cambian: es la clave de caché de
    /// los paneles raster (Vistas 2 y 3) — un re-fetch periódico con datos
    /// idénticos no debe re-rasterizar ni retransmitir la imagen.
    pub stamp: Instant,
}

pub struct PairState {
    pub meta: PairMeta,
    /// Último mid conocido (WS si está vivo; si no, del contexto REST).
    pub mid: f64,
    pub ctx: Option<CtxSnapshot>,
    /// Última vez que el mid llegó por WS (allMids o BBO); None = nunca.
    /// El REST solo pisa el mid si esto está más viejo que MID_STALE.
    pub mid_at: Option<Instant>,
    pub hist: VecDeque<HistPoint>,
    pub mid_hist: VecDeque<(Instant, f64)>,
    /// Historial lento (1 muestra/min, ~25h) para la rotación de la Vista 6.
    pub slow_hist: VecDeque<SlowPoint>,
    pub extra: Option<PairExtraData>,
    /// OI alineado a cierres de vela: (ms del cierre exacto, último OI visto
    /// en ese bucket). En memoria, crece con el uptime — como el hist de ΔOI.
    pub oi_candles: HashMap<Interval, VecDeque<(u64, f64)>>,
}

impl PairState {
    fn new(meta: PairMeta) -> Self {
        Self {
            meta,
            mid: 0.0,
            ctx: None,
            mid_at: None,
            hist: VecDeque::with_capacity(HIST_CAP),
            mid_hist: VecDeque::with_capacity(MID_HIST_CAP),
            slow_hist: VecDeque::new(),
            extra: None,
            oi_candles: HashMap::new(),
        }
    }

    /// Registra el OI del snapshot en el bucket de cierre de vela de cada
    /// intervalo; al pasar el cierre, el valor que queda es ≈ OI al cierre.
    fn push_oi_candles(&mut self, t_ms: u64, oi: f64) {
        for iv in Interval::ALL {
            let int = iv.ms();
            let close = (t_ms / int + 1) * int;
            let dq = self.oi_candles.entry(iv).or_default();
            match dq.back_mut() {
                Some(last) if last.0 == close => last.1 = oi,
                _ => {
                    dq.push_back((close, oi));
                    while dq.len() > OI_CANDLE_CAP {
                        dq.pop_front();
                    }
                }
            }
        }
    }

    /// Velas de `extra` emparejadas con su OI al cierre, para la señal de
    /// densidad de liquidación. Solo la cola contigua más reciente (un hueco
    /// en el OI acumulado o en las velas corta la serie): ΔOI debe ser
    /// estrictamente vela a vela.
    pub fn liq_bars(&self) -> Vec<LiqBar> {
        let Some(e) = &self.extra else {
            return Vec::new();
        };
        let Some(oi) = self.oi_candles.get(&e.interval) else {
            return Vec::new();
        };
        let int = e.interval.ms();
        // índice de bucket redondeado: el t_close de la API llega como
        // frontera−1ms y el del poller como frontera exacta
        let bidx = |ms: u64| (ms + int / 2) / int;
        let by_idx: HashMap<u64, f64> = oi.iter().map(|(t, v)| (bidx(*t), *v)).collect();
        let mut bars: Vec<LiqBar> = Vec::new();
        let mut expect: Option<u64> = None;
        for c in e.candles.iter().rev() {
            let idx = bidx(c.t_close);
            if expect.is_some_and(|x| x != idx) {
                break;
            }
            let Some(&oi_v) = by_idx.get(&idx) else {
                break;
            };
            bars.push(LiqBar {
                open: c.open,
                high: c.high,
                low: c.low,
                close: c.close,
                oi: oi_v,
            });
            expect = idx.checked_sub(1);
        }
        bars.reverse();
        bars
    }

    /// Muestrea el contexto actual en el historial lento (throttle 1/min).
    fn push_slow(&mut self, t: Instant) {
        let push = self
            .slow_hist
            .back()
            .is_none_or(|p| t.duration_since(p.t) >= SLOW_THROTTLE);
        if !push {
            return;
        }
        let Some(c) = self.ctx else {
            return;
        };
        let premium_bps = self.premium_bps();
        self.slow_hist.push_back(SlowPoint {
            t,
            oi: c.open_interest,
            mark: c.mark_px,
            vol24: c.day_ntl_vlm,
            premium_bps,
        });
        while self.slow_hist.len() > SLOW_HIST_CAP {
            self.slow_hist.pop_front();
        }
    }

    /// Añade un punto al historial de mid respetando throttle y capacidad.
    fn push_mid(&mut self, t: Instant, mid: f64) {
        let push = self
            .mid_hist
            .back()
            .is_none_or(|(last, _)| t.duration_since(*last) >= MID_THROTTLE);
        if push {
            self.mid_hist.push_back((t, mid));
            while self.mid_hist.len() > MID_HIST_CAP {
                self.mid_hist.pop_front();
            }
        }
    }

    pub fn oi_notional(&self) -> f64 {
        self.ctx.map(|c| c.open_interest * c.mark_px).unwrap_or(0.0)
    }

    pub fn volume24(&self) -> f64 {
        self.ctx.map(|c| c.day_ntl_vlm).unwrap_or(0.0)
    }

    pub fn chg24_pct(&self) -> Option<f64> {
        let c = self.ctx?;
        if c.prev_day_px <= 0.0 || self.mid <= 0.0 {
            return None;
        }
        Some((self.mid / c.prev_day_px - 1.0) * 100.0)
    }

    /// Funding horario en % (0.00125 significa 0.00125%/h).
    pub fn funding_hourly_pct(&self) -> Option<f64> {
        self.ctx.map(|c| c.funding * 100.0)
    }

    /// Funding anualizado en % (horario × 24 × 365).
    pub fn funding_apr_pct(&self) -> Option<f64> {
        self.ctx.map(|c| c.funding * 24.0 * 365.0 * 100.0)
    }

    /// Premium perp vs oracle en puntos básicos.
    pub fn premium_bps(&self) -> Option<f64> {
        let c = self.ctx?;
        if let Some(p) = c.premium {
            return Some(p * 10_000.0);
        }
        if c.oracle_px > 0.0 {
            Some((c.mark_px / c.oracle_px - 1.0) * 10_000.0)
        } else {
            None
        }
    }

    /// Punto de historia más cercano a `window` atrás; None si aún no hay
    /// historia suficiente (menos de ~3/4 de la ventana).
    ///
    /// NOTA: el historial de OI/mark vive solo en memoria (no se persiste),
    /// así que tras arrancar la app las columnas ΔOI muestran "—" hasta haber
    /// acumulado la ventana completa (5m/1h). Es comportamiento esperado, no
    /// un bug — no "arreglarlo" añadiendo persistencia sin que se pida.
    fn hist_point_at(&self, window: Duration) -> Option<&HistPoint> {
        point_at(&self.hist, window, |h| h.t)
    }

    /// Como hist_point_at pero sobre el historial lento (ventanas 1h/4h/24h).
    fn slow_point_at(&self, window: Duration) -> Option<&SlowPoint> {
        point_at(&self.slow_hist, window, |p| p.t)
    }

    /// ΔOI de la ventana en USD: contratos nuevos × mark actual, para aislar
    /// el flujo de posicionamiento del movimiento del precio.
    pub fn oi_delta_usd(&self, window: Duration) -> Option<f64> {
        let old = self.slow_point_at(window)?;
        let cur = self.slow_hist.back()?;
        if old.oi <= 0.0 || cur.mark <= 0.0 {
            return None;
        }
        Some((cur.oi - old.oi) * cur.mark)
    }

    /// ΔOI % de la ventana sobre el historial lento (en unidades base: mide
    /// posicionamiento puro, sin efecto precio).
    pub fn oi_delta_pct_slow(&self, window: Duration) -> Option<f64> {
        let old = self.slow_point_at(window)?;
        let cur = self.slow_hist.back()?;
        if old.oi <= 0.0 {
            return None;
        }
        Some((cur.oi / old.oi - 1.0) * 100.0)
    }

    /// Volumen notional estimado negociado en la ventana (ver flow::window_vol_est).
    pub fn window_vol_est(&self, window: Duration) -> Option<f64> {
        let old = self.slow_point_at(window)?;
        let cur = self.slow_hist.back()?;
        let w_frac = window.as_secs_f64() / 86_400.0;
        Some(flow::window_vol_est(cur.vol24, old.vol24, w_frac))
    }

    /// Volumen de la ventana sobre su volumen típico (rolling 24h prorrateado):
    /// >1 la ventana negoció más de lo normal, <1 menos.
    pub fn window_vol_ratio(&self, window: Duration) -> Option<f64> {
        let est = self.window_vol_est(window)?;
        let cur = self.slow_hist.back()?;
        let typical = cur.vol24 * window.as_secs_f64() / 86_400.0;
        if typical <= 0.0 {
            return None;
        }
        Some(est / typical)
    }

    /// Premium medio sostenido de la ventana en bps (media de muestras lentas,
    /// no el instantáneo): confirmación de presión agresiva persistente.
    pub fn premium_mean_bps(&self, window: Duration) -> Option<f64> {
        // slow_point_at como guard de cobertura de la ventana
        let start = self.slow_point_at(window)?.t;
        let vals: Vec<f64> = self
            .slow_hist
            .iter()
            .filter(|p| p.t >= start)
            .filter_map(|p| p.premium_bps)
            .collect();
        if vals.is_empty() {
            return None;
        }
        Some(vals.iter().sum::<f64>() / vals.len() as f64)
    }

    /// Percentil (0-100) del funding actual contra su histórico descargado
    /// (~30d si la paginación llegó entera). None sin `extra` o con historia
    /// corta — solo el par seleccionado tiene funding_hist.
    pub fn funding_percentile(&self) -> Option<f64> {
        let cur = self.ctx?.funding;
        let e = self.extra.as_ref()?;
        let hist: Vec<f64> = e.funding_hist.iter().map(|(_, f)| *f).collect();
        flow::percentile_rank(&hist, cur)
    }

    pub fn oi_delta_pct(&self, window: Duration) -> Option<f64> {
        let old = self.hist_point_at(window)?;
        let cur = self.hist.back()?;
        if old.oi <= 0.0 {
            return None;
        }
        Some((cur.oi / old.oi - 1.0) * 100.0)
    }

    pub fn px_delta_pct(&self, window: Duration) -> Option<f64> {
        let old = self.hist_point_at(window)?;
        let cur = self.hist.back()?;
        if old.mark <= 0.0 {
            return None;
        }
        Some((cur.mark / old.mark - 1.0) * 100.0)
    }

    pub fn regime(&self, window: Duration) -> Regime {
        match (self.px_delta_pct(window), self.oi_delta_pct(window)) {
            (Some(p), Some(o)) => signals::classify(p, o),
            _ => Regime::Flat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Ranking,
    Pair,
    Heatmap,
    Whales,
    Wallet,
    Liq,
    /// Panel Ballenas + RSI/ADX/DMI (port del Pine whales+RSI), por par.
    WhaleRsi,
    /// Fondos (Fase 2): conexión de la cuenta maestra vía WalletConnect.
    Funds,
    /// Flujo de Dinero / Posicionamiento (Vista 6, solo lectura).
    Flow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortCol {
    Coin,
    Px,
    Chg24,
    FundApr,
    Premium,
    OiNotional,
    OiD5m,
    OiD1h,
    Vol24,
}

impl SortCol {
    pub fn next(self) -> Self {
        match self {
            SortCol::Coin => SortCol::Px,
            SortCol::Px => SortCol::Chg24,
            SortCol::Chg24 => SortCol::FundApr,
            SortCol::FundApr => SortCol::Premium,
            SortCol::Premium => SortCol::OiNotional,
            SortCol::OiNotional => SortCol::OiD5m,
            SortCol::OiD5m => SortCol::OiD1h,
            SortCol::OiD1h => SortCol::Vol24,
            SortCol::Vol24 => SortCol::Coin,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SortCol::Coin => crate::i18n::t().sc_coin,
            SortCol::Px => crate::i18n::t().sc_px,
            SortCol::Chg24 => "24h%",
            SortCol::FundApr => "Funding APR",
            SortCol::Premium => "Premium",
            SortCol::OiNotional => "OI $",
            SortCol::OiD5m => "ΔOI 5m",
            SortCol::OiD1h => "ΔOI 1h",
            SortCol::Vol24 => "Vol 24h",
        }
    }

    fn default_desc(&self) -> bool {
        !matches!(self, SortCol::Coin)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeatMetric {
    FundApr,
    OiD1h,
    Chg24,
}

impl HeatMetric {
    pub fn next(self) -> Self {
        match self {
            HeatMetric::FundApr => HeatMetric::OiD1h,
            HeatMetric::OiD1h => HeatMetric::Chg24,
            HeatMetric::Chg24 => HeatMetric::FundApr,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            HeatMetric::FundApr => "funding APR",
            HeatMetric::OiD1h => "ΔOI 1h",
            HeatMetric::Chg24 => crate::i18n::t().hm_chg24,
        }
    }
}

/// Modal del depósito real (Pieza 2, Vista 8). Captura todo el teclado
/// mientras está abierto, como los modales del panel de ejecución.
#[derive(Debug, Clone)]
pub enum DepositUi {
    /// Editando la cantidad (USDC, hasta 6 decimales).
    Amount { buf: String, err: Option<String> },
    /// Resumen completo antes de pedir la firma; la cantidad ya validada.
    Confirm { usdc: f64, units: u128 },
}

/// Modal del retiro real (paso 5, Vista 8) — mismo contrato que `DepositUi`:
/// captura todo el teclado, cantidad → resumen de confirmación.
#[derive(Debug, Clone)]
pub enum WithdrawUi {
    Amount { buf: String, err: Option<String> },
    Confirm { usdc: f64, units: u128 },
}

/// Modal de la transferencia interna spot⇄perps (Vista 8) — mismo contrato
/// que `WithdrawUi` más el sentido, que se puede alternar en el paso de
/// cantidad (Tab/←→) y se arrastra al resumen.
#[derive(Debug, Clone)]
pub enum TransferUi {
    Amount {
        /// true = spot → perps; false = perps → spot.
        to_perp: bool,
        buf: String,
        err: Option<String>,
    },
    Confirm {
        to_perp: bool,
        usdc: f64,
        units: u128,
    },
}

/// Modal de la autorización de agent wallet (paso 6, Vista 8): un único paso
/// de confirmación (no hay cantidad que teclear — la clave ya está generada
/// en memoria y solo se persiste si la maestra firma y el servidor acepta).
#[derive(Debug, Clone)]
pub struct AgentUi {
    /// Dirección pública del agent nuevo (checksummed) — se muestra para
    /// compararla con la que MetaMask enseñará dentro de la firma.
    pub agent_addr: String,
    /// Agent anterior de esta red (si hay clave guardada): la aprobación
    /// nueva lo INVALIDA en el servidor — avisarlo antes de firmar.
    pub replaces: Option<String>,
    /// Clave privada generada; solo la lee `agent_confirm_yes`. Nunca se
    /// muestra ni se loguea.
    priv_hex: String,
}

pub struct App {
    pub pairs: HashMap<String, PairState>,
    pub view: View,
    pub sort: SortCol,
    pub sort_desc: bool,
    pub sel: usize,
    pub selected_coin: Option<String>,
    pub heat_metric: HeatMetric,
    pub interval: Interval,
    pub show_help: bool,
    /// ¿El frame recién dibujado pintó algún overlay (ayuda, buscador, modal)?
    /// Lo marcan los propios sitios de dibujo (por eso `Cell`: varios reciben
    /// `&App`), y el bucle de `tui` lo usa para forzar un redibujado completo
    /// al cerrarse — ver la nota del artefacto Kitty en `Tui::run`.
    pub overlay_drawn: std::cell::Cell<bool>,
    pub ws_ok: bool,
    pub last_ctx_at: Option<Instant>,
    pub last_err: Option<String>,
    pub should_quit: bool,
    pub table_state: TableState,
    pub net_label: &'static str,
    // whales
    pub whales: Vec<WhaleInfo>,
    pub whale_status: Option<String>,
    pub whales_at: Option<Instant>,
    pub whale_sel: usize,
    pub whales_state: TableState,
    /// Modal de dirección completa de whale (Vista 7): (dirección, feedback de copia).
    pub whale_modal: Option<(String, Option<String>)>,
    /// Rect de la zona de datos de la tabla de whales (para mapear clicks a filas).
    pub whale_rows_area: Option<Rect>,
    // wallet watch-only
    pub wallet: Option<AccountSnapshot>,
    pub wallet_at: Option<Instant>,
    pub wallet_addr: Option<String>,
    /// Dirección watch-only parseada, para reconstruir la lista del watcher.
    wallet_target: Option<Address>,
    /// Historial de fills (`userFills`) de la dirección observada (Vista 9),
    /// más reciente primero. Alimenta win rate, PnL realizado y la sección de
    /// operaciones cerradas.
    pub wallet_fills: Vec<FillInfo>,
    pub wallet_fills_at: Option<Instant>,
    /// Fila seleccionada en la tabla de posiciones abiertas (Vista 9).
    pub wallet_sel: usize,
    pub wallet_state: TableState,
    /// Si Some, modal abierto con el detalle (fecha apertura + funding) de la
    /// posición abierta de este `coin`.
    pub wallet_pos_modal: Option<String>,
    /// Rect de la zona de datos de la tabla de posiciones (mapear clicks).
    pub wallet_rows_area: Option<Rect>,
    pub input_mode: bool,
    pub input_buf: String,
    pub input_err: Option<String>,
    // fondos (WalletConnect, Vista 8)
    pub wc: WcStatus,
    /// Saldo/posiciones reales de la cuenta maestra conectada por WC, leídos
    /// por el mismo watcher clearinghouseState de la Vista 9; None sin sesión.
    pub funds: Option<AccountSnapshot>,
    pub funds_at: Option<Instant>,
    /// Dirección de la sesión WC parseada (observación + enrutado de snapshots).
    funds_target: Option<Address>,
    /// Saldo USDC on-chain (wallet en Arbitrum) de la maestra — Pieza 1 del
    /// depósito. None = sin respuesta aún; Some(None) = chain de la sesión
    /// sin mapeo de USDC; Some(Some(v)) = saldo en USDC.
    pub usdc: Option<Option<f64>>,
    pub usdc_at: Option<Instant>,
    /// (dirección, chain CAIP-2) enviadas al watcher de USDC on-chain.
    usdc_target: Option<(Address, String)>,
    /// Modal del depósito real (Pieza 2): cantidad → resumen de confirmación.
    pub deposit_ui: Option<DepositUi>,
    /// Última fase del depósito real (firma / tx en vuelo / confirmado / fallo).
    pub deposit: Option<DepositStatus>,
    /// Modal del retiro real (paso 5): cantidad → resumen de confirmación.
    pub withdraw_ui: Option<WithdrawUi>,
    /// Última fase del retiro real (firma / aceptado / llegado / fallo).
    pub withdraw: Option<WithdrawStatus>,
    /// Modal de la autorización de agent wallet (paso 6).
    pub agent_ui: Option<AgentUi>,
    /// Última fase de la autorización del agent (firma / aceptado /
    /// verificado / fallo).
    pub agent: Option<AgentStatus>,
    /// Saldo SPOT dentro de Hyperliquid de la maestra (spotClearinghouseState)
    /// — separado del de perps (`funds`); None hasta la primera lectura.
    pub spot: Option<SpotSnapshot>,
    pub spot_at: Option<Instant>,
    /// Modo de cuenta de la maestra (`userAbstraction`): decide si el margen
    /// operable sale de spot (unificada) o de perps (estándar). None hasta la
    /// primera lectura — mientras tanto se conserva el comportamiento clásico.
    pub account_mode: Option<AccountMode>,
    /// Modal de la transferencia spot⇄perps: sentido+cantidad → confirmación.
    pub transfer_ui: Option<TransferUi>,
    /// Última fase de la transferencia (firma / aceptada / reflejada / fallo).
    pub transfer: Option<TransferStatus>,
    /// Panel de ejecución de la Vista 8 — maqueta, o REAL si `trade` está
    /// armado (paso 7: agent key de testnet presente).
    pub exec: ExecState,
    /// Trading real armado (solo testnet en esta fase): cuenta de trading
    /// (maestra del agent) + canal hacia la tarea del trader.
    pub trade: Option<TradeArm>,
    /// Órdenes abiertas REALES de la cuenta de trading; None hasta leerlas.
    pub live_orders: Option<Vec<LiveOrd>>,
    pub live_orders_at: Option<Instant>,
    // flujo de dinero (Vista 6)
    pub flow_sel: usize,
    pub flow_state: TableState,
    pub flow_win: FlowWin,
    pub flow_sort: FlowSort,
    /// Sentido del orden de la Vista 6 (desc = mayor magnitud/sesgo arriba).
    pub flow_desc: bool,
    /// Buscador incremental `/` compartido entre Ranking y Flujo.
    pub search: search::SearchState,
    /// CVD del par seleccionado; None hasta el primer batch de trades.
    pub cvd: Option<CvdState>,
    /// Delta por vela del par seleccionado (Vista 2); None hasta el primer
    /// batch de trades. Comparte el canal `trades` con el CVD.
    pub delta: Option<DeltaState>,
    // liquidaciones
    pub liq_range_idx: usize,
    /// Última posición del ratón (col, fila) para el hover de velas.
    pub mouse_pos: Option<(u16, u16)>,
    /// Protocolo gráfico + cachés de imagen de los paneles de indicadores.
    pub gfx: Gfx,
    /// Último mensaje WS recibido (allMids o BBO), para el indicador de frescura.
    pub last_ws_at: Option<Instant>,
    extra_tx: mpsc::Sender<ExtraReq>,
    /// Última petición extra encolada (coin, interval, cuándo): debounce para
    /// no re-encolar lo mismo en cada tick mientras la respuesta está en vuelo.
    extra_req: Option<(String, Interval, Instant)>,
    wallet_tx: watch::Sender<Vec<Address>>,
    /// (dirección, chain) de la maestra hacia el watcher de USDC on-chain.
    usdc_tx: watch::Sender<Option<(Address, String)>>,
    /// Par seleccionado hacia la tarea de BBO por-coin.
    coin_tx: watch::Sender<Option<String>>,
    /// Comandos hacia el gestor de sesión WalletConnect.
    wc_tx: mpsc::UnboundedSender<WcCmd>,
}

/// Contexto del trading real: la cuenta cuyas posiciones/órdenes se muestran
/// y el canal de comandos hacia el trader (que firma con la agent key).
pub struct TradeArm {
    pub master: Address,
    /// Dirección canónica (checksummed) — la misma forma que usan los
    /// mensajes de los watchers para enrutar.
    pub master_fmt: String,
    /// Dirección pública del agent (solo informativa, para la UI).
    pub agent_addr: String,
    tx: mpsc::UnboundedSender<TraderCmd>,
}

impl App {
    pub fn new(
        extra_tx: mpsc::Sender<ExtraReq>,
        wallet_tx: watch::Sender<Vec<Address>>,
        usdc_tx: watch::Sender<Option<(Address, String)>>,
        coin_tx: watch::Sender<Option<String>>,
        wc_tx: mpsc::UnboundedSender<WcCmd>,
        net_label: &'static str,
        gfx: Gfx,
    ) -> Self {
        Self {
            pairs: HashMap::new(),
            view: View::Ranking,
            sort: SortCol::OiNotional,
            sort_desc: true,
            sel: 0,
            selected_coin: None,
            heat_metric: HeatMetric::FundApr,
            interval: Interval::H1,
            show_help: false,
            overlay_drawn: std::cell::Cell::new(false),
            ws_ok: false,
            last_ctx_at: None,
            last_err: None,
            should_quit: false,
            table_state: TableState::default(),
            net_label,
            whales: Vec::new(),
            whale_status: None,
            whales_at: None,
            whale_sel: 0,
            whales_state: TableState::default(),
            whale_modal: None,
            whale_rows_area: None,
            wallet: None,
            wallet_at: None,
            wallet_addr: None,
            wallet_target: None,
            wallet_fills: Vec::new(),
            wallet_fills_at: None,
            wallet_sel: 0,
            wallet_state: TableState::default(),
            wallet_pos_modal: None,
            wallet_rows_area: None,
            input_mode: false,
            input_buf: String::new(),
            input_err: None,
            wc: WcStatus::Idle,
            funds: None,
            funds_at: None,
            funds_target: None,
            usdc: None,
            usdc_at: None,
            usdc_target: None,
            deposit_ui: None,
            deposit: None,
            withdraw_ui: None,
            withdraw: None,
            agent_ui: None,
            agent: None,
            spot: None,
            spot_at: None,
            account_mode: None,
            trade: None,
            live_orders: None,
            live_orders_at: None,
            transfer_ui: None,
            transfer: None,
            exec: ExecState::new(),
            flow_sel: 0,
            flow_state: TableState::default(),
            flow_win: FlowWin::H1,
            flow_sort: FlowSort::Rotation,
            flow_desc: true,
            search: search::SearchState::default(),
            cvd: None,
            delta: None,
            liq_range_idx: 1,
            mouse_pos: None,
            gfx,
            last_ws_at: None,
            extra_tx,
            extra_req: None,
            wallet_tx,
            usdc_tx,
            coin_tx,
            wc_tx,
        }
    }

    /// Frescura del WS para el indicador de cabecera: hubo mensaje reciente.
    pub fn ws_fresh(&self) -> bool {
        self.last_ws_at.is_some_and(|t| t.elapsed() < MID_STALE)
    }

    /// Propaga el par seleccionado a las tareas por-coin (BBO y trades) y, si
    /// cambió, descarta el CVD del par anterior: el acumulado no es comparable.
    fn sync_selected(&mut self) {
        let cur = self.selected_coin.clone();
        let changed = self.coin_tx.send_if_modified(|v| {
            if *v != cur {
                *v = cur;
                true
            } else {
                false
            }
        });
        if changed {
            self.cvd = None;
            self.delta = None;
        }
    }

    pub fn liq_range(&self) -> f64 {
        LIQ_RANGES[self.liq_range_idx % LIQ_RANGES.len()]
    }

    pub fn apply_msg(&mut self, msg: DataMsg) {
        match msg {
            DataMsg::Ctxs(items) => {
                self.last_ctx_at = Some(Instant::now());
                for (meta, snap) in items {
                    let p = self
                        .pairs
                        .entry(meta.name.clone())
                        .or_insert_with(|| PairState::new(meta.clone()));
                    p.meta = meta;
                    // el REST solo alimenta el mid de un par cuyo WS está
                    // stale (par a par): si el BBO o allMids lo actualizó
                    // hace poco, pisarlo con el snapshot REST provocaría
                    // saltos hacia atrás; si el WS calló en silencio (p. ej.
                    // resuscripción perdida tras reconectar), este fallback
                    // evita que el precio quede congelado indefinidamente
                    let ws_stale = p.mid_at.is_none_or(|t| t.elapsed() > MID_STALE);
                    if let Some(m) = snap.mid_px {
                        if m > 0.0 && (ws_stale || p.mid <= 0.0) {
                            p.mid = m;
                        }
                    } else if p.mid <= 0.0 {
                        p.mid = snap.mark_px;
                    }
                    // y que el sparkline de mid tampoco se congele
                    if ws_stale && p.mid > 0.0 {
                        p.push_mid(snap.t, p.mid);
                    }
                    p.ctx = Some(snap);
                    p.push_slow(snap.t);
                    p.push_oi_candles(snap.t_ms, snap.open_interest);
                    p.hist.push_back(HistPoint {
                        t: snap.t,
                        mark: snap.mark_px,
                        oi: snap.open_interest,
                    });
                    while p.hist.len() > HIST_CAP {
                        p.hist.pop_front();
                    }
                }
                self.clamp_sel();
                // siembra las posiciones demo del panel de ejecución con
                // precios reales, para que su PnL en vivo se mueva de verdad
                if !self.exec.seeded {
                    let mid = |c: &str| self.pairs.get(c).map(|p| p.mid).filter(|m| *m > 0.0);
                    self.exec.seed(mid("BTC"), mid("ETH"), mid("SOL"));
                }
            }
            DataMsg::Mids(mids) => {
                self.ws_ok = true;
                let now = Instant::now();
                self.last_ws_at = Some(now);
                for (coin, mid) in mids {
                    if mid <= 0.0 {
                        continue;
                    }
                    if let Some(p) = self.pairs.get_mut(&coin) {
                        p.mid = mid;
                        p.mid_at = Some(now);
                        p.push_mid(now, mid);
                    }
                }
            }
            DataMsg::CoinMid { coin, mid } => {
                let now = Instant::now();
                self.last_ws_at = Some(now);
                if let Some(p) = self.pairs.get_mut(&coin) {
                    p.mid = mid;
                    p.mid_at = Some(now);
                    p.push_mid(now, mid);
                }
            }
            DataMsg::PairExtra {
                coin,
                interval,
                candles,
                funding_hist,
            } => {
                if let Some(p) = self.pairs.get_mut(&coin) {
                    let stamp = match &p.extra {
                        Some(e) if e.interval == interval && e.candles == candles => e.stamp,
                        _ => Instant::now(),
                    };
                    let closes: Vec<f64> = candles.iter().map(|c| c.close).collect();
                    let highs: Vec<f64> = candles.iter().map(|c| c.high).collect();
                    let lows: Vec<f64> = candles.iter().map(|c| c.low).collect();
                    let vols: Vec<f64> = candles.iter().map(|c| c.volume).collect();
                    let panel = signals::whale_panel(
                        &highs,
                        &lows,
                        &closes,
                        &vols,
                        &signals::WhaleParams::default(),
                    );
                    p.extra = Some(PairExtraData {
                        interval,
                        rsi: panel.last_rsi(),
                        dmi: panel.last_dmi(),
                        panel,
                        candles,
                        funding_hist,
                        fetched: Instant::now(),
                        stamp,
                    });
                }
            }
            DataMsg::CoinTrades {
                coin,
                buy_ntl,
                sell_ntl,
                t_ms,
            } => {
                // batches rezagados de un par ya sustituido se descartan
                if self.selected_coin.as_deref() == Some(coin.as_str()) {
                    if self.cvd.as_ref().is_none_or(|s| s.coin != coin) {
                        self.cvd = Some(CvdState::new(coin.clone()));
                    }
                    if let Some(st) = &mut self.cvd {
                        st.push(buy_ntl, sell_ntl);
                    }
                    if self.delta.as_ref().is_none_or(|s| s.coin != coin) {
                        self.delta = Some(DeltaState::new(coin));
                    }
                    if let Some(st) = &mut self.delta {
                        st.push(buy_ntl, sell_ntl, t_ms);
                    }
                }
            }
            DataMsg::Whales(w) => {
                self.whales = w;
                self.whales_at = Some(Instant::now());
                let n = self.whale_rows_len();
                if self.whale_sel >= n {
                    self.whale_sel = n.saturating_sub(1);
                }
            }
            DataMsg::WhaleStatus(s) => self.whale_status = Some(s),
            DataMsg::WalletState(snap) => {
                let now = Instant::now();
                // la misma respuesta puede alimentar ambos huecos si coinciden
                if self.funds_addr().as_deref() == Some(snap.addr.as_str()) {
                    self.funds = Some(snap.clone());
                    self.funds_at = Some(now);
                }
                // descarta respuestas en vuelo de una dirección ya sustituida
                if self.wallet_addr.as_deref() == Some(snap.addr.as_str()) {
                    self.wallet = Some(snap);
                    self.wallet_at = Some(now);
                }
                self.sync_exec_rows();
            }
            DataMsg::WalletFills { addr, fills } => {
                // solo la dirección watch-only actual (descarta en-vuelo y la
                // maestra WC, que comparte watcher pero no usa este historial)
                if self.wallet_addr.as_deref() == Some(addr.as_str()) {
                    self.wallet_fills = fills;
                    self.wallet_fills_at = Some(Instant::now());
                }
            }
            DataMsg::Wc(s) => {
                self.wc = s;
                self.sync_funds_target();
            }
            DataMsg::UsdcBalance { addr, usdc } => {
                if self.funds_addr().as_deref() == Some(addr.as_str()) {
                    self.usdc = Some(usdc);
                    self.usdc_at = Some(Instant::now());
                }
            }
            DataMsg::SpotState(snap) => {
                if self.funds_addr().as_deref() == Some(snap.addr.as_str()) {
                    self.spot = Some(snap);
                    self.spot_at = Some(Instant::now());
                }
            }
            DataMsg::AccountMode { addr, mode } => {
                if self.funds_addr().as_deref() == Some(addr.as_str()) {
                    self.account_mode = Some(mode);
                }
            }
            DataMsg::OpenOrders { addr, orders } => {
                if self.trade.as_ref().map(|t| t.master_fmt.as_str()) == Some(addr.as_str()) {
                    self.live_orders = Some(orders);
                    self.live_orders_at = Some(Instant::now());
                    self.sync_exec_rows();
                }
            }
            DataMsg::Exec(ev) => match ev {
                ExecEvent::Phase(s) => {
                    self.exec.err = None;
                    self.exec.status = Some(s);
                }
                ExecEvent::Done(s) => {
                    self.exec.err = None;
                    self.exec.status = Some(s);
                }
                ExecEvent::Failed(e) => {
                    self.exec.status = None;
                    self.exec.err = Some(e);
                }
            },
            DataMsg::Deposit(s) => self.deposit = Some(s),
            DataMsg::Withdraw(s) => self.withdraw = Some(s),
            DataMsg::Agent(s) => self.agent = Some(s),
            DataMsg::Transfer(s) => self.transfer = Some(s),
            DataMsg::WsStatus(ok) => self.ws_ok = ok,
            DataMsg::RestError(e) => self.last_err = Some(e),
        }
    }

    /// Nº de filas de la tabla de whales (posiciones aplanadas).
    pub fn whale_rows_len(&self) -> usize {
        self.whales.iter().map(|w| w.positions.len()).sum()
    }

    /// Dirección completa de la whale de la fila `idx`, respetando el mismo
    /// orden (por notional desc) con que se dibuja la tabla en `ui::whales`.
    pub fn whale_addr_at(&self, idx: usize) -> Option<String> {
        let mut flat: Vec<(&str, f64)> = self
            .whales
            .iter()
            .flat_map(|w| w.positions.iter().map(move |p| (w.addr.as_str(), p.position_value)))
            .collect();
        flat.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        flat.get(idx).map(|(a, _)| a.to_string())
    }

    /// Nº de posiciones abiertas de la wallet observada (Vista 9).
    fn wallet_pos_len(&self) -> usize {
        self.wallet.as_ref().map(|w| w.positions.len()).unwrap_or(0)
    }

    fn move_wallet_sel(&mut self, delta: i64) {
        let n = self.wallet_pos_len();
        if n == 0 {
            self.wallet_sel = 0;
            return;
        }
        self.wallet_sel = (self.wallet_sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
    }

    /// Abre el modal de detalle (fecha de apertura + funding acumulado) de la
    /// posición abierta seleccionada.
    fn open_wallet_pos_modal(&mut self) {
        if let Some(w) = self.wallet.as_ref() {
            if let Some(p) = w.positions.get(self.wallet_sel) {
                self.wallet_pos_modal = Some(p.coin.clone());
            }
        }
    }

    /// Abre el modal con la dirección completa de la whale seleccionada.
    fn open_whale_modal(&mut self) {
        if let Some(addr) = self.whale_addr_at(self.whale_sel) {
            self.whale_modal = Some((addr, None));
        }
    }

    /// Copia la dirección del modal al portapapeles del sistema vía OSC 52
    /// (soportado por Kitty/Ghostty/WezTerm; sin dependencia extra).
    fn copy_whale_addr(&mut self) {
        if let Some((addr, feedback)) = self.whale_modal.as_mut() {
            use base64ct::{Base64, Encoding};
            use std::io::Write;
            let b64 = Base64::encode_string(addr.as_bytes());
            let seq = format!("\x1b]52;c;{b64}\x07");
            let ok = std::io::stdout()
                .write_all(seq.as_bytes())
                .and_then(|_| std::io::stdout().flush())
                .is_ok();
            *feedback = Some(if ok {
                crate::i18n::t().wh_copied_ok.into()
            } else {
                crate::i18n::t().wh_copied_fail.into()
            });
        }
    }

    /// (liq_px, notional) de whales para un par, para el mapa de liquidaciones.
    pub fn whale_liqs_for(&self, coin: &str) -> Vec<(f64, f64)> {
        self.whales
            .iter()
            .flat_map(|w| &w.positions)
            .filter(|p| p.coin == coin)
            .filter_map(|p| Some((p.liq_px?, p.position_value)))
            .collect()
    }

    /// (Σ notional long, Σ short) de las whales trackeadas en un par; None si
    /// ninguna whale tiene posición en él (sin datos ≠ 50/50).
    pub fn whale_ntl_for(&self, coin: &str) -> Option<(f64, f64)> {
        let (mut long, mut short) = (0.0, 0.0);
        let mut any = false;
        for p in self
            .whales
            .iter()
            .flat_map(|w| &w.positions)
            .filter(|p| p.coin == coin)
        {
            any = true;
            if p.szi >= 0.0 {
                long += p.position_value;
            } else {
                short += p.position_value;
            }
        }
        any.then_some((long, short))
    }

    /// Combustible de liquidación estimado (abajo, arriba) a ±3% del mark.
    /// Requiere velas (`extra`), así que en la práctica solo el par seleccionado.
    pub fn liq_fuel_for(&self, coin: &str) -> Option<(f64, f64)> {
        let p = self.pairs.get(coin)?;
        let e = p.extra.as_ref()?;
        if p.mid <= 0.0 {
            return None;
        }
        let buckets = liq::estimate(
            &e.candles,
            p.oi_notional(),
            p.mid,
            &self.whale_liqs_for(coin),
            FLOW_LIQ_BUCKETS,
            FLOW_LIQ_RANGE,
        );
        flow::liq_fuel(&buckets, p.mid)
    }

    /// (ΔCVD, Δprecio %) de la ventana para el par seleccionado; None hasta
    /// que el tracking cubra ~3/4 de la ventana.
    pub fn cvd_window(&self, window: Duration) -> Option<(f64, f64)> {
        let st = self.cvd.as_ref()?;
        let p = self.selected_pair()?;
        if st.coin != p.meta.name {
            return None;
        }
        let old_cvd = point_at(&st.hist, window, |x| x.0)?.1;
        let old_px = point_at(&p.mid_hist, window, |x| x.0)?.1;
        if old_px <= 0.0 || p.mid <= 0.0 {
            return None;
        }
        Some((st.cum - old_cvd, (p.mid / old_px - 1.0) * 100.0))
    }

    /// Divergencia CVD/precio del par seleccionado en la ventana estándar.
    pub fn cvd_signal(&self) -> Option<flow::CvdSignal> {
        let (delta, px_chg) = self.cvd_window(FLOW_CVD_WIN)?;
        let p = self.selected_pair()?;
        // umbral de significancia: fracción del volumen típico de la ventana
        let typical = p.volume24() * FLOW_CVD_WIN.as_secs_f64() / 86_400.0;
        Some(flow::cvd_divergence(
            delta,
            px_chg,
            typical * CVD_MIN_VOL_FRAC,
        ))
    }

    /// Entradas del score compuesto de un par. Funding percentil, asimetría de
    /// liqs y CVD dependen de datos que solo se descargan para el par
    /// seleccionado; los demás componentes están disponibles cross-pair.
    pub fn score_inputs(&self, coin: &str) -> flow::ScoreInputs {
        let Some(p) = self.pairs.get(coin) else {
            return flow::ScoreInputs::default();
        };
        let selected = self.selected_coin.as_deref() == Some(coin);
        flow::ScoreInputs {
            funding_pctile: p.funding_percentile(),
            premium_mean_bps: p.premium_mean_bps(FLOW_PREM_WIN),
            whale_pct_long: self
                .whale_ntl_for(coin)
                .filter(|(l, s)| l + s > 0.0)
                .map(|(l, s)| l / (l + s) * 100.0),
            liq_fuel: self.liq_fuel_for(coin),
            cvd: if selected { self.cvd_signal() } else { None },
        }
    }

    /// Asimetría normalizada del combustible de liqs ±3% de un par (+1 = todo
    /// abajo, sesgo bajista). None sin velas: solo pares visitados tienen
    /// `extra`, y el warmup honesto manda esos pares al final del orden.
    pub fn liq_asym_for(&self, coin: &str) -> Option<f64> {
        let (below, above) = self.liq_fuel_for(coin)?;
        flow::liq_asym(below, above)
    }

    /// Pares ordenados según el modo de orden activo de la Vista 6. Los pares
    /// sin dato para la clave van SIEMPRE al final, también con el orden
    /// invertido: sin datos no compiten en el ranking.
    pub fn flow_coins(&self) -> Vec<String> {
        let win = self.flow_win.dur();
        let mut v: Vec<(&PairState, Option<f64>)> = self
            .pairs
            .values()
            .map(|p| {
                let key = match self.flow_sort {
                    FlowSort::Rotation => p.oi_delta_usd(win).map(f64::abs),
                    FlowSort::Fuel => self.liq_asym_for(&p.meta.name),
                    FlowSort::Confluence => flow::confluence(
                        flow::score(&self.score_inputs(&p.meta.name)),
                        self.liq_asym_for(&p.meta.name),
                    ),
                };
                (p, key)
            })
            .collect();
        v.sort_by(|a, b| {
            let names = || a.0.meta.name.cmp(&b.0.meta.name);
            match (a.1, b.1) {
                (Some(x), Some(y)) => {
                    let ord = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
                    let ord = if self.flow_desc { ord.reverse() } else { ord };
                    ord.then_with(names)
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => names(),
            }
        });
        v.into_iter().map(|(p, _)| p.meta.name.clone()).collect()
    }

    /// Resultados del buscador `/` sobre la lista base de la vista activa
    /// (conserva el orden de la tabla; prefijos primero).
    pub fn search_results(&self) -> Vec<String> {
        let base = match self.view {
            View::Flow => self.flow_coins(),
            _ => self.sorted_coins(),
        };
        search::filter_rank(&base, &self.search.query)
    }

    fn move_search_sel(&mut self, delta: i64) {
        let n = self.search_results().len();
        if n == 0 {
            return;
        }
        let cur = self.search.sel.min(n - 1) as i64;
        self.search.sel = (cur + delta).clamp(0, n as i64 - 1) as usize;
    }

    /// Teclas mientras el buscador está abierto: todo carácter imprimible va
    /// a la query (los tickers llevan letras que son atajos en otras vistas).
    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.search.close(),
            KeyCode::Enter => {
                // deja el cursor en el par elegido dentro de la tabla
                // completa, sin saltar de vista ni fijar el par
                let res = self.search_results();
                let pick = res.get(self.search.sel.min(res.len().saturating_sub(1)));
                if let Some(c) = pick {
                    match self.view {
                        View::Flow => {
                            if let Some(i) = self.flow_coins().iter().position(|x| x == c) {
                                self.flow_sel = i;
                            }
                        }
                        // en Fondos el buscador fija el par del formulario
                        // (el par de la orden es el par seleccionado global)
                        View::Funds => {
                            if let Some(i) = self.sorted_coins().iter().position(|x| x == c) {
                                self.sel = i;
                            }
                            self.selected_coin = Some(c.clone());
                            self.sync_selected();
                            self.request_extra(false);
                        }
                        _ => {
                            if let Some(i) = self.sorted_coins().iter().position(|x| x == c) {
                                self.sel = i;
                            }
                        }
                    }
                }
                self.search.close();
            }
            KeyCode::Backspace => {
                self.search.query.pop();
                self.search.sel = 0;
            }
            KeyCode::Down => self.move_search_sel(1),
            KeyCode::Up => self.move_search_sel(-1),
            KeyCode::PageDown => self.move_search_sel(15),
            KeyCode::PageUp => self.move_search_sel(-15),
            KeyCode::Char(c) if c.is_ascii_graphic() => {
                if self.search.query.len() < 12 {
                    self.search.query.push(c);
                    self.search.sel = 0;
                }
            }
            _ => {}
        }
    }

    /// Recoloca el cursor de la Vista 6 sobre el mismo par tras reordenar.
    fn keep_flow_cursor(&mut self, coin: Option<String>) {
        if let Some(c) = coin {
            if let Some(i) = self.flow_coins().iter().position(|x| *x == c) {
                self.flow_sel = i;
            }
        }
    }

    /// Nombres de pares ordenados según la columna activa.
    pub fn sorted_coins(&self) -> Vec<String> {
        let mut v: Vec<&PairState> = self.pairs.values().collect();
        v.sort_by(|a, b| {
            let ord = if self.sort == SortCol::Coin {
                a.meta.name.cmp(&b.meta.name)
            } else {
                self.sort_key(a)
                    .partial_cmp(&self.sort_key(b))
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| a.meta.name.cmp(&b.meta.name))
            };
            if self.sort_desc {
                ord.reverse()
            } else {
                ord
            }
        });
        v.into_iter().map(|p| p.meta.name.clone()).collect()
    }

    fn sort_key(&self, p: &PairState) -> f64 {
        match self.sort {
            SortCol::Coin => 0.0,
            SortCol::Px => p.mid,
            SortCol::Chg24 => p.chg24_pct().unwrap_or(f64::NEG_INFINITY),
            SortCol::FundApr => p.funding_apr_pct().unwrap_or(f64::NEG_INFINITY),
            SortCol::Premium => p.premium_bps().unwrap_or(f64::NEG_INFINITY),
            SortCol::OiNotional => p.oi_notional(),
            SortCol::OiD5m => p.oi_delta_pct(OI_WIN_SHORT).unwrap_or(f64::NEG_INFINITY),
            SortCol::OiD1h => p.oi_delta_pct(OI_WIN_LONG).unwrap_or(f64::NEG_INFINITY),
            SortCol::Vol24 => p.volume24(),
        }
    }

    pub fn selected_pair(&self) -> Option<&PairState> {
        self.pairs.get(self.selected_coin.as_deref()?)
    }

    fn clamp_sel(&mut self) {
        let n = self.pairs.len();
        if n == 0 {
            self.sel = 0;
        } else if self.sel >= n {
            self.sel = n - 1;
        }
    }

    fn move_sel(&mut self, delta: i64) {
        let n = self.pairs.len();
        if n == 0 {
            return;
        }
        self.sel = (self.sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
    }

    fn move_whale_sel(&mut self, delta: i64) {
        let n = self.whale_rows_len();
        if n == 0 {
            return;
        }
        self.whale_sel = (self.whale_sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
    }

    fn move_flow_sel(&mut self, delta: i64) {
        let n = self.pairs.len();
        if n == 0 {
            return;
        }
        self.flow_sel = (self.flow_sel as i64 + delta).clamp(0, n as i64 - 1) as usize;
    }

    /// Entra a la Vista 6; si no hay par seleccionado toma el de la fila
    /// actual de la tabla de rotación para poblar el panel de posicionamiento.
    fn enter_flow(&mut self) {
        self.view = View::Flow;
        if self.selected_coin.is_none() {
            self.selected_coin = self.flow_coins().get(self.flow_sel).cloned();
        }
        if self.selected_coin.is_some() {
            self.sync_selected();
            self.request_extra(false);
        }
    }

    /// Fija la fila seleccionada de la tabla de rotación como par activo
    /// (panel de posicionamiento + suscripciones bbo/trades + velas/funding).
    fn flow_select(&mut self) {
        if let Some(c) = self.flow_coins().get(self.flow_sel) {
            self.selected_coin = Some(c.clone());
            self.sync_selected();
            self.request_extra(false);
        }
    }

    /// Vistas que necesitan un par seleccionado (Par y Liquidaciones).
    fn goto_pair_view(&mut self, view: View) {
        if self.selected_coin.is_none() {
            let coins = self.sorted_coins();
            self.selected_coin = coins.get(self.sel).cloned();
        }
        if self.selected_coin.is_some() {
            self.view = view;
            self.sync_selected();
            self.request_extra(false);
        }
    }

    fn enter_pair_from_sel(&mut self) {
        let coins = self.sorted_coins();
        if let Some(c) = coins.get(self.sel) {
            self.selected_coin = Some(c.clone());
            self.view = View::Pair;
            self.sync_selected();
            self.request_extra(false);
        }
    }

    /// Par anterior/siguiente respetando el orden del ranking.
    fn step_pair(&mut self, delta: i64) {
        let coins = self.sorted_coins();
        if coins.is_empty() {
            return;
        }
        let cur = self
            .selected_coin
            .as_ref()
            .and_then(|c| coins.iter().position(|x| x == c))
            .unwrap_or(0);
        let next = (cur as i64 + delta).clamp(0, coins.len() as i64 - 1) as usize;
        self.sel = next;
        self.selected_coin = Some(coins[next].clone());
        self.sync_selected();
        self.request_extra(false);
    }

    fn request_extra(&mut self, force: bool) {
        let Some(coin) = self.selected_coin.clone() else {
            return;
        };
        if !force {
            if let Some(p) = self.pairs.get(&coin) {
                if let Some(e) = &p.extra {
                    if e.interval == self.interval && e.fetched.elapsed() < extra_ttl(self.interval)
                    {
                        return;
                    }
                }
            }
            // misma petición ya en vuelo (la respuesta tarda <1s normalmente):
            // no re-encolar en cada tick de 250ms del bucle de UI
            if let Some((c, iv, at)) = &self.extra_req {
                if *c == coin && *iv == self.interval && at.elapsed() < EXTRA_INFLIGHT {
                    return;
                }
            }
        }
        self.extra_req = Some((coin.clone(), self.interval, Instant::now()));
        let _ = self.extra_tx.try_send(ExtraReq {
            coin,
            interval: self.interval,
        });
    }

    /// Llamado por el bucle de UI en cada tick: mientras una vista con velas
    /// (Vista 2 o 3) está activa, re-pide velas/funding con cadencia según la
    /// temporalidad — antes solo se pedían al navegar, así que la gráfica se
    /// congelaba hasta salir y volver a entrar.
    pub fn tick_refresh(&mut self) {
        if matches!(self.view, View::Pair | View::WhaleRsi) {
            self.request_extra(false);
        }
    }

    fn cycle_interval(&mut self) {
        self.interval = self.interval.next();
        self.request_extra(false);
    }

    /// Tab avanza en el orden numérico de las teclas 1-9.
    fn cycle_view(&mut self) {
        match self.view {
            View::Ranking => self.goto_pair_view(View::Pair),
            View::Pair => self.goto_pair_view(View::WhaleRsi),
            View::WhaleRsi => self.view = View::Heatmap,
            View::Heatmap => self.goto_pair_view(View::Liq),
            View::Liq => self.enter_flow(),
            View::Flow => self.view = View::Whales,
            View::Whales => self.enter_funds(),
            View::Funds => self.view = View::Wallet,
            View::Wallet => self.view = View::Ranking,
        }
    }

    /// Dirección canónica (mismo formato que `AccountSnapshot.addr`) de la
    /// cuenta maestra WC, para enrutar los snapshots del watcher.
    fn funds_addr(&self) -> Option<String> {
        self.funds_target.map(|a| format!("{a}"))
    }

    /// Reenvía al watcher la lista de cuentas a observar (watch-only + maestra).
    fn push_wallet_targets(&self) {
        let mut targets: Vec<Address> = self.wallet_target.into_iter().collect();
        if let Some(a) = self.funds_target {
            if self.wallet_target != Some(a) {
                targets.push(a);
            }
        }
        let _ = self.wallet_tx.send(targets);
    }

    /// Arma el trading REAL (paso 7, solo testnet): fija la cuenta de trading
    /// y el canal al trader, y pone a los watchers a observarla desde ya —
    /// las posiciones, el saldo y el margen real no esperan a conectar WC.
    pub fn arm_trading(
        &mut self,
        master: Address,
        agent_addr: String,
        tx: mpsc::UnboundedSender<TraderCmd>,
    ) {
        self.trade = Some(TradeArm {
            master,
            master_fmt: format!("{master}"),
            agent_addr,
            tx,
        });
        self.exec.real = true;
        // el panel real no siembra demos ni conserva las que hubiera
        self.exec.positions.clear();
        self.exec.orders.clear();
        self.exec.seeded = true;
        self.sync_funds_target();
    }

    /// Modo real: reconstruye las filas del panel desde la cuenta de VERDAD
    /// (posiciones del clearinghouseState + órdenes de frontendOpenOrders).
    /// El SL/TP de cada posición se deriva de sus triggers reduce-only.
    fn sync_exec_rows(&mut self) {
        let Some(t) = &self.trade else {
            return;
        };
        // solo la cuenta de trading: los watchers también emiten snapshots
        // de otras direcciones (watch-only, sesión WC ajena)
        let Some(funds) = self.funds.as_ref().filter(|f| f.addr == t.master_fmt) else {
            return;
        };
        let orders = self.live_orders.as_deref().unwrap_or(&[]);
        self.exec.positions = funds
            .positions
            .iter()
            .map(|p| {
                let long = p.szi >= 0.0;
                let trig = |quiero_sl: bool| {
                    orders
                        .iter()
                        .find(|o| {
                            o.coin == p.coin
                                && o.is_close_trigger()
                                && o.is_buy != long
                                && if quiero_sl { o.is_sl() } else { o.is_tp() }
                        })
                        .map(|o| o.px)
                };
                exec::MockPos {
                    coin: p.coin.clone(),
                    szi: p.szi,
                    entry: p.entry_px.unwrap_or(0.0),
                    lev: p.leverage,
                    sl: trig(true),
                    tp: trig(false),
                    liq: p.liq_px,
                    demo: false,
                }
            })
            .collect();
        self.exec.orders = orders
            .iter()
            .map(|o| exec::MockOrd {
                coin: o.coin.clone(),
                side: if o.is_buy {
                    exec::Side::Long
                } else {
                    exec::Side::Short
                },
                kind: if o.is_sl() {
                    exec::OrdKind::Sl
                } else if o.is_tp() {
                    exec::OrdKind::Tp
                } else {
                    exec::OrdKind::Limit
                },
                px: o.px,
                sz: o.sz,
                oid: Some(o.oid),
                demo: false,
            })
            .collect();
        self.exec.clamp_focus();
    }

    /// Chain CAIP-2 implícita de la red del TUI (para observar la cuenta de
    /// trading sin sesión WC: el saldo on-chain/spot necesitan una chain).
    fn net_chain(&self) -> String {
        if self.net_label == "testnet" {
            "eip155:421614".into()
        } else {
            "eip155:42161".into()
        }
    }

    /// Sincroniza la cuenta maestra observada con la sesión WC: al conectar se
    /// leen su clearinghouseState y su USDC on-chain sin que el usuario
    /// escriba nada; al caer la sesión se deja de mostrar todo (nunca saldos
    /// de una sesión muerta). Con el trading real armado, sin sesión WC se
    /// observa la cuenta de TRADING (la maestra del agent) — el panel real
    /// necesita posiciones y margen sin depender del móvil.
    fn sync_funds_target(&mut self) {
        let session = match &self.wc {
            WcStatus::Connected(s) => s
                .address
                .parse::<Address>()
                .ok()
                .map(|a| (a, s.chain.clone())),
            _ => self
                .trade
                .as_ref()
                .map(|t| (t.master, self.net_chain())),
        };
        let target = session.as_ref().map(|(a, _)| *a);
        if target != self.funds_target {
            self.funds_target = target;
            self.funds = None;
            self.funds_at = None;
            // cambia la cuenta maestra: los flujos de depósito/retiro/agent/
            // transferencia y el saldo spot eran de otra
            self.deposit_ui = None;
            self.deposit = None;
            self.withdraw_ui = None;
            self.withdraw = None;
            self.agent_ui = None;
            self.agent = None;
            self.spot = None;
            self.spot_at = None;
            self.account_mode = None;
            self.transfer_ui = None;
            self.transfer = None;
            self.push_wallet_targets();
        }
        if session != self.usdc_target {
            self.usdc_target = session.clone();
            self.usdc = None;
            self.usdc_at = None;
            let _ = self.usdc_tx.send(session);
        }
    }

    fn start_input(&mut self) {
        self.input_mode = true;
        self.input_err = None;
        // Siempre en blanco: no pre-rellenar con la última dirección observada
        // (evita reobservar la anterior por un Enter accidental).
        self.input_buf = String::new();
    }

    /// Inserta texto pegado (bracketed paste) en el campo de dirección: filtra
    /// a caracteres válidos de una dirección (0x + hex) y respeta el tope de 42.
    pub fn handle_input_paste(&mut self, data: &str) {
        if !self.input_mode {
            return;
        }
        for c in data.trim().chars() {
            if self.input_buf.len() >= 42 {
                break;
            }
            if c.is_ascii_hexdigit() || c == 'x' || c == 'X' {
                self.input_buf.push(c);
            }
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = false;
                self.input_err = None;
            }
            KeyCode::Enter => {
                let s = self.input_buf.trim();
                match s.parse::<Address>() {
                    Ok(a) => {
                        self.wallet_addr = Some(format!("{a}"));
                        self.wallet = None;
                        self.wallet_at = None;
                        self.wallet_fills = Vec::new();
                        self.wallet_fills_at = None;
                        self.wallet_sel = 0;
                        self.wallet_pos_modal = None;
                        self.wallet_target = Some(a);
                        self.push_wallet_targets();
                        self.input_mode = false;
                        self.input_err = None;
                    }
                    Err(_) => {
                        self.input_err =
                            Some("dirección inválida: formato 0x + 40 hex".to_string());
                    }
                }
            }
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) if c.is_ascii_hexdigit() || c == 'x' || c == 'X' => {
                if self.input_buf.len() < 42 {
                    self.input_buf.push(c);
                }
            }
            _ => {}
        }
    }

    // ── depósito real al bridge (Pieza 2, Vista 8) ─────────────────────────

    /// Ruta del depósito + saldo USDC on-chain ya leído. None = no se puede
    /// depositar (sin sesión, chain sin bridge verificado, o saldo sin leer).
    pub fn deposit_route(&self) -> Option<(crate::data::DepositRoute, f64)> {
        let chain = match &self.wc {
            WcStatus::Connected(s) => s.chain.as_str(),
            _ => return None,
        };
        let route = crate::data::deposit_route(chain)?;
        let bal = self.usdc.flatten()?;
        Some((route, bal))
    }

    fn deposit_open(&mut self) {
        // con una firma ya en vuelo no se abre otro flujo encima
        if matches!(self.deposit, Some(DepositStatus::AwaitingWallet { .. })) {
            return;
        }
        if self.deposit_route().is_some() {
            self.deposit_ui = Some(DepositUi::Amount {
                buf: String::new(),
                err: None,
            });
        }
    }

    /// Teclado con el modal de depósito abierto: captura TODO (los dígitos de
    /// la cantidad son también atajos globales de vista).
    fn handle_deposit_key(&mut self, key: KeyEvent) {
        match self.deposit_ui.clone() {
            Some(DepositUi::Amount { .. }) => match key.code {
                KeyCode::Esc => self.deposit_ui = None,
                KeyCode::Enter => self.deposit_validate(),
                KeyCode::Backspace => {
                    if let Some(DepositUi::Amount { buf, err }) = &mut self.deposit_ui {
                        buf.pop();
                        *err = None;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                    if let Some(DepositUi::Amount { buf, err }) = &mut self.deposit_ui {
                        if buf.len() < 13 && (c != '.' || !buf.contains('.')) {
                            buf.push(c);
                            *err = None;
                        }
                    }
                }
                _ => {}
            },
            Some(DepositUi::Confirm { .. }) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.deposit_confirm_yes(),
                KeyCode::Char('n') | KeyCode::Esc => self.deposit_ui = None,
                _ => {}
            },
            None => {}
        }
    }

    /// Enter sobre la cantidad: valida contra el mínimo del bridge y el saldo
    /// on-chain, y pasa al resumen de confirmación.
    fn deposit_validate(&mut self) {
        let Some(DepositUi::Amount { buf, .. }) = self.deposit_ui.clone() else {
            return;
        };
        let Some((_, bal)) = self.deposit_route() else {
            self.deposit_ui = None;
            return;
        };
        let outcome = match walletconnect::usdc_units(&buf) {
            None => Err("cantidad inválida: número con hasta 6 decimales".to_string()),
            Some(units) if units < crate::data::MIN_DEPOSIT_UNITS => {
                Err("mínimo 5 USDC — por debajo el bridge NO acredita y SE PIERDE".to_string())
            }
            Some(units) if units > (bal * 1e6).floor() as u128 => {
                Err(format!("saldo insuficiente: hay {bal:.2} USDC on-chain"))
            }
            Some(units) => Ok(units),
        };
        match outcome {
            Ok(units) => {
                self.deposit_ui = Some(DepositUi::Confirm {
                    usdc: units as f64 / 1e6,
                    units,
                });
            }
            Err(e) => {
                if let Some(DepositUi::Amount { err, .. }) = &mut self.deposit_ui {
                    *err = Some(e);
                }
            }
        }
    }

    /// Confirmación final (`y`): envía la petición de firma al gestor WC.
    /// A partir de aquí quien decide es el usuario en MetaMask.
    fn deposit_confirm_yes(&mut self) {
        let Some(DepositUi::Confirm { usdc, units }) = self.deposit_ui.clone() else {
            return;
        };
        let Some((route, _)) = self.deposit_route() else {
            self.deposit_ui = None;
            return;
        };
        let _ = self.wc_tx.send(WcCmd::Deposit(DepositReq {
            usdc,
            units,
            token: route.usdc.to_string(),
            bridge: route.bridge.to_string(),
            rpc: route.rpc,
        }));
        // feedback inmediato; el gestor emitirá las fases reales
        self.deposit = Some(DepositStatus::AwaitingWallet { usdc });
        self.deposit_ui = None;
    }

    /// Botón "sí" del modal vía ratón: equivale a Enter (cantidad) o `y`.
    fn deposit_modal_yes(&mut self) {
        match &self.deposit_ui {
            Some(DepositUi::Amount { .. }) => self.deposit_validate(),
            Some(DepositUi::Confirm { .. }) => self.deposit_confirm_yes(),
            None => {}
        }
    }

    // ── retiro real de Hyperliquid (paso 5, Vista 8) ───────────────────────

    /// ¿La cuenta maestra está en Unified Account Mode? Mientras el modo no
    /// se haya leído (None) se asume estándar: un fallo del endpoint nunca
    /// debe deshabilitar funcionalidad por sí solo.
    pub fn is_unified(&self) -> bool {
        self.account_mode == Some(AccountMode::Unified)
    }

    /// Margen/saldo disponible para operar y retirar en perps, según el modo
    /// de cuenta: unificada → spotClearinghouseState (fuente de verdad única;
    /// el clearinghouseState de perps NO es significativo y diría 0),
    /// estándar → withdrawable del clearinghouseState de perps, como siempre.
    /// None = el saldo relevante aún no se leyó.
    pub fn perps_avail(&self) -> Option<f64> {
        if self.is_unified() {
            let s = self.spot.as_ref()?;
            // el disponible tras mantenimiento es la cifra exacta; si la API
            // no lo trae, total − hold es el mejor proxy honesto
            Some(
                s.usdc_avail
                    .unwrap_or_else(|| (s.usdc_total - s.usdc_hold).max(0.0)),
            )
        } else {
            Some(self.funds.as_ref()?.withdrawable)
        }
    }

    /// Ruta del retiro + disponible real según el modo de cuenta (ver
    /// `perps_avail`). None = no se puede retirar (sin sesión, chain sin
    /// ruta, o saldo relevante aún sin leer). La MISMA función sirve mainnet
    /// y testnet: la chain de la sesión (que deriva de --testnet) elige el
    /// endpoint.
    pub fn withdraw_route(&self) -> Option<(crate::data::WithdrawRoute, f64)> {
        let chain = match &self.wc {
            WcStatus::Connected(s) => s.chain.as_str(),
            _ => return None,
        };
        let route = crate::data::withdraw_route(chain)?;
        let avail = self.perps_avail()?;
        Some((route, avail))
    }

    fn withdraw_open(&mut self) {
        // con una firma ya en vuelo no se abre otro flujo encima
        if matches!(self.withdraw, Some(WithdrawStatus::AwaitingWallet { .. })) {
            return;
        }
        if self.withdraw_route().is_some() {
            self.withdraw_ui = Some(WithdrawUi::Amount {
                buf: String::new(),
                err: None,
            });
        }
    }

    /// Teclado con el modal de retiro abierto: captura TODO, como el depósito.
    fn handle_withdraw_key(&mut self, key: KeyEvent) {
        match self.withdraw_ui.clone() {
            Some(WithdrawUi::Amount { .. }) => match key.code {
                KeyCode::Esc => self.withdraw_ui = None,
                KeyCode::Enter => self.withdraw_validate(),
                KeyCode::Backspace => {
                    if let Some(WithdrawUi::Amount { buf, err }) = &mut self.withdraw_ui {
                        buf.pop();
                        *err = None;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                    if let Some(WithdrawUi::Amount { buf, err }) = &mut self.withdraw_ui {
                        if buf.len() < 13 && (c != '.' || !buf.contains('.')) {
                            buf.push(c);
                            *err = None;
                        }
                    }
                }
                _ => {}
            },
            Some(WithdrawUi::Confirm { .. }) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.withdraw_confirm_yes(),
                KeyCode::Char('n') | KeyCode::Esc => self.withdraw_ui = None,
                _ => {}
            },
            None => {}
        }
    }

    /// Enter sobre la cantidad: valida contra la comisión ($1, descontada de
    /// lo pedido) y contra el withdrawable real, y pasa a la confirmación.
    fn withdraw_validate(&mut self) {
        let Some(WithdrawUi::Amount { buf, .. }) = self.withdraw_ui.clone() else {
            return;
        };
        let Some((_, avail)) = self.withdraw_route() else {
            self.withdraw_ui = None;
            return;
        };
        let outcome = match walletconnect::usdc_units(&buf) {
            None => Err("cantidad inválida: número con hasta 6 decimales".to_string()),
            Some(units) if units <= crate::data::WITHDRAW_FEE_UNITS => {
                Err("la comisión es 1 USDC — retira más de 1 para recibir algo".to_string())
            }
            Some(units) if units > (avail * 1e6).floor() as u128 => {
                Err(format!("excede el retirable: hay {avail:.2} USDC withdrawable"))
            }
            Some(units) => Ok(units),
        };
        match outcome {
            Ok(units) => {
                self.withdraw_ui = Some(WithdrawUi::Confirm {
                    usdc: units as f64 / 1e6,
                    units,
                });
            }
            Err(e) => {
                if let Some(WithdrawUi::Amount { err, .. }) = &mut self.withdraw_ui {
                    *err = Some(e);
                }
            }
        }
    }

    /// Confirmación final (`y`): pide la firma EIP-712 al gestor WC. El
    /// destino es SIEMPRE la propia cuenta maestra de la sesión — no hay
    /// input de dirección que equivocarse.
    fn withdraw_confirm_yes(&mut self) {
        let Some(WithdrawUi::Confirm { usdc, units }) = self.withdraw_ui.clone() else {
            return;
        };
        // destino SIEMPRE checksummed EIP-55 (funds_addr = la dirección de la
        // sesión parseada y re-formateada), el mismo string que se firma,
        // se POSTea y se muestra en el resumen
        let (Some((route, _)), Some(dest)) = (self.withdraw_route(), self.funds_addr()) else {
            self.withdraw_ui = None;
            return;
        };
        let _ = self.wc_tx.send(WcCmd::Withdraw(WithdrawReq {
            usdc,
            units,
            amount: walletconnect::fmt_usdc(units),
            destination: dest,
            api: route.api.to_string(),
            hl_chain: route.hl_chain,
            chain_id: route.chain_id,
            rpc: route.rpc,
            token: route.usdc.to_string(),
        }));
        // feedback inmediato; el gestor emitirá las fases reales
        self.withdraw = Some(WithdrawStatus::AwaitingWallet { usdc });
        self.withdraw_ui = None;
    }

    /// Botón "sí" del modal de retiro vía ratón: Enter (cantidad) o `y`.
    fn withdraw_modal_yes(&mut self) {
        match &self.withdraw_ui {
            Some(WithdrawUi::Amount { .. }) => self.withdraw_validate(),
            Some(WithdrawUi::Confirm { .. }) => self.withdraw_confirm_yes(),
            None => {}
        }
    }

    // ── transferencia interna spot⇄perps (Vista 8) ─────────────────────────

    /// Ruta de la transferencia: solo exige sesión con chain mapeada (reusa
    /// la del retiro: mismo endpoint y dominio de firma). La validación de
    /// saldo llega después, según el sentido elegido.
    pub fn transfer_route(&self) -> Option<crate::data::WithdrawRoute> {
        let chain = match &self.wc {
            WcStatus::Connected(s) => s.chain.as_str(),
            _ => return None,
        };
        crate::data::withdraw_route(chain)
    }

    /// Disponible del lado ORIGEN según el sentido: spot = total − hold,
    /// perps = withdrawable. None = ese saldo aún no se leyó.
    fn transfer_avail(&self, to_perp: bool) -> Option<f64> {
        if to_perp {
            self.spot
                .as_ref()
                .map(|s| (s.usdc_total - s.usdc_hold).max(0.0))
        } else {
            self.funds.as_ref().map(|f| f.withdrawable)
        }
    }

    fn transfer_open(&mut self) {
        // con una firma ya en vuelo no se abre otro flujo encima
        if matches!(self.transfer, Some(TransferStatus::AwaitingWallet { .. })) {
            return;
        }
        // cuenta unificada: usdClassTransfer está deshabilitado por
        // Hyperliquid ("action disabled when unified account is active") —
        // mensaje claro aquí en vez de dejar llegar ese error crudo
        if self.is_unified() {
            self.transfer = Some(TransferStatus::Failed {
                error: crate::i18n::t().fu_xfer_unified_na.into(),
            });
            return;
        }
        if self.transfer_route().is_some() {
            // sentido por defecto: spot → perps (el caso típico — el faucet
            // de testnet y las ventas spot dejan el USDC en spot)
            self.transfer_ui = Some(TransferUi::Amount {
                to_perp: true,
                buf: String::new(),
                err: None,
            });
        }
    }

    /// Alterna el sentido en el paso de cantidad (Tab/←→ o click).
    fn transfer_toggle_dir(&mut self) {
        if let Some(TransferUi::Amount { to_perp, err, .. }) = &mut self.transfer_ui {
            *to_perp = !*to_perp;
            *err = None;
        }
    }

    /// Teclado con el modal de transferencia abierto: captura TODO.
    fn handle_transfer_key(&mut self, key: KeyEvent) {
        match self.transfer_ui.clone() {
            Some(TransferUi::Amount { .. }) => match key.code {
                KeyCode::Esc => self.transfer_ui = None,
                KeyCode::Enter => self.transfer_validate(),
                KeyCode::Tab
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Char('h')
                | KeyCode::Char('l') => self.transfer_toggle_dir(),
                KeyCode::Backspace => {
                    if let Some(TransferUi::Amount { buf, err, .. }) = &mut self.transfer_ui {
                        buf.pop();
                        *err = None;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                    if let Some(TransferUi::Amount { buf, err, .. }) = &mut self.transfer_ui {
                        if buf.len() < 13 && (c != '.' || !buf.contains('.')) {
                            buf.push(c);
                            *err = None;
                        }
                    }
                }
                _ => {}
            },
            Some(TransferUi::Confirm { .. }) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.transfer_confirm_yes(),
                KeyCode::Char('n') | KeyCode::Esc => self.transfer_ui = None,
                _ => {}
            },
            None => {}
        }
    }

    /// Enter sobre la cantidad: valida contra el disponible del lado origen
    /// (honesto si ese saldo aún no se leyó) y pasa a la confirmación.
    fn transfer_validate(&mut self) {
        let Some(TransferUi::Amount { to_perp, buf, .. }) = self.transfer_ui.clone() else {
            return;
        };
        if self.transfer_route().is_none() {
            self.transfer_ui = None;
            return;
        }
        let avail = self.transfer_avail(to_perp);
        let outcome = match walletconnect::usdc_units(&buf) {
            None => Err("cantidad inválida: número con hasta 6 decimales".to_string()),
            Some(0) => Err("la cantidad debe ser mayor que 0".to_string()),
            Some(units) => match avail {
                None => Err(format!(
                    "saldo {} aún sin leer — espera unos segundos",
                    if to_perp { "spot" } else { "de perps" }
                )),
                Some(a) if units > (a * 1e6).floor() as u128 => Err(format!(
                    "excede el disponible del lado origen: {a:.2} USDC"
                )),
                Some(_) => Ok(units),
            },
        };
        match outcome {
            Ok(units) => {
                self.transfer_ui = Some(TransferUi::Confirm {
                    to_perp,
                    usdc: units as f64 / 1e6,
                    units,
                });
            }
            Err(e) => {
                if let Some(TransferUi::Amount { err, .. }) = &mut self.transfer_ui {
                    *err = Some(e);
                }
            }
        }
    }

    /// Confirmación final (`y`): pide la firma EIP-712 al gestor WC. No hay
    /// dirección de destino que equivocarse: el dinero no sale de la cuenta,
    /// solo cambia de lado (spot⇄perps) dentro de Hyperliquid.
    fn transfer_confirm_yes(&mut self) {
        let Some(TransferUi::Confirm {
            to_perp,
            usdc,
            units,
        }) = self.transfer_ui.clone()
        else {
            return;
        };
        let (Some(route), Some(master)) = (self.transfer_route(), self.funds_addr()) else {
            self.transfer_ui = None;
            return;
        };
        let _ = self.wc_tx.send(WcCmd::ClassTransfer(TransferReq {
            usdc,
            units,
            amount: walletconnect::fmt_usdc(units),
            to_perp,
            master,
            api: route.api.to_string(),
            hl_chain: route.hl_chain,
            chain_id: route.chain_id,
        }));
        // feedback inmediato; el gestor emitirá las fases reales
        self.transfer = Some(TransferStatus::AwaitingWallet { usdc, to_perp });
        self.transfer_ui = None;
    }

    /// Botón "sí" del modal de transferencia vía ratón: Enter o `y`.
    fn transfer_modal_yes(&mut self) {
        match &self.transfer_ui {
            Some(TransferUi::Amount { .. }) => self.transfer_validate(),
            Some(TransferUi::Confirm { .. }) => self.transfer_confirm_yes(),
            None => {}
        }
    }

    // ── autorización de agent wallet (paso 6, Vista 8) ─────────────────────

    /// `a`: genera la clave nueva YA (para poder mostrar su dirección en el
    /// resumen) y abre la confirmación. La clave no toca el disco hasta que
    /// la maestra firme; cancelar la descarta sin rastro.
    fn agent_open(&mut self) {
        // con una firma ya en vuelo no se abre otro flujo encima
        if matches!(self.agent, Some(AgentStatus::AwaitingWallet { .. })) {
            return;
        }
        // misma puerta que el retiro: sesión activa con ruta conocida y
        // clearinghouseState leído (sin cuenta en Hyperliquid no hay agent)
        let Some((route, _)) = self.withdraw_route() else {
            return;
        };
        let fresh = crate::wallet::agent::generate();
        self.agent_ui = Some(AgentUi {
            agent_addr: fresh.address,
            replaces: crate::wallet::agent::existing_agent(route.hl_chain),
            priv_hex: fresh.priv_hex,
        });
    }

    /// Teclado con el modal del agent abierto: captura TODO, como los demás.
    fn handle_agent_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => self.agent_confirm_yes(),
            KeyCode::Char('n') | KeyCode::Esc => self.agent_ui = None,
            _ => {}
        }
    }

    /// Confirmación (`y`): pide la firma EIP-712 de la maestra al gestor WC.
    fn agent_confirm_yes(&mut self) {
        let Some(ui) = self.agent_ui.clone() else {
            return;
        };
        let (Some((route, _)), Some(master)) = (self.withdraw_route(), self.funds_addr()) else {
            self.agent_ui = None;
            return;
        };
        let _ = self.wc_tx.send(WcCmd::ApproveAgent(AgentReq {
            agent_address: ui.agent_addr.clone(),
            agent_priv: ui.priv_hex,
            master,
            api: route.api.to_string(),
            hl_chain: route.hl_chain,
            chain_id: route.chain_id,
        }));
        // feedback inmediato; el gestor emitirá las fases reales
        self.agent = Some(AgentStatus::AwaitingWallet {
            agent: ui.agent_addr,
        });
        self.agent_ui = None;
    }

    pub fn handle_mouse(&mut self, me: MouseEvent) {
        match me.kind {
            // panel de ejecución (Vista 8): click sobre el hitmap del frame
            MouseEventKind::Down(MouseButton::Left) if self.view == View::Funds => {
                self.mouse_pos = Some((me.column, me.row));
                self.exec_click(me.column, me.row);
            }
            // Vista 7: click sobre una whale → selecciona la fila y abre el modal
            // con su dirección completa. Si ya hay modal abierto, un click lo cierra.
            MouseEventKind::Down(MouseButton::Left) if self.view == View::Whales => {
                self.mouse_pos = Some((me.column, me.row));
                if self.whale_modal.is_some() {
                    self.whale_modal = None;
                } else if let Some(a) = self.whale_rows_area {
                    if me.column >= a.x
                        && me.column < a.x + a.width
                        && me.row >= a.y
                        && me.row < a.y + a.height
                    {
                        let idx = self.whales_state.offset() + (me.row - a.y) as usize;
                        if idx < self.whale_rows_len() {
                            self.whale_sel = idx;
                            self.open_whale_modal();
                        }
                    }
                }
            }
            // Vista 9: click sobre una posición abierta → abre su modal de
            // detalle; si el modal ya está abierto, un click lo cierra.
            MouseEventKind::Down(MouseButton::Left) if self.view == View::Wallet => {
                self.mouse_pos = Some((me.column, me.row));
                if self.wallet_pos_modal.is_some() {
                    self.wallet_pos_modal = None;
                } else if let Some(a) = self.wallet_rows_area {
                    if me.column >= a.x
                        && me.column < a.x + a.width
                        && me.row >= a.y
                        && me.row < a.y + a.height
                    {
                        let idx = self.wallet_state.offset() + (me.row - a.y) as usize;
                        if idx < self.wallet_pos_len() {
                            self.wallet_sel = idx;
                            self.open_wallet_pos_modal();
                        }
                    }
                }
            }
            // arrastre del slider de apalancamiento
            MouseEventKind::Drag(MouseButton::Left) if self.exec.lev_drag => {
                self.mouse_pos = Some((me.column, me.row));
                self.exec_slider_at(me.column);
            }
            MouseEventKind::Up(_) => self.exec.lev_drag = false,
            MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Down(_) => {
                self.mouse_pos = Some((me.column, me.row));
            }
            // capturar el ratón desactiva el scroll nativo del terminal:
            // la rueda desplaza las tablas con selección
            MouseEventKind::ScrollDown if self.search.active => self.move_search_sel(3),
            MouseEventKind::ScrollUp if self.search.active => self.move_search_sel(-3),
            MouseEventKind::ScrollDown => match self.view {
                View::Ranking => self.move_sel(3),
                View::Whales => self.move_whale_sel(3),
                View::Flow => self.move_flow_sel(3),
                View::Wallet => self.move_wallet_sel(3),
                View::Funds if !self.exec.captures() => self.exec_move_focus(1),
                _ => {}
            },
            MouseEventKind::ScrollUp => match self.view {
                View::Ranking => self.move_sel(-3),
                View::Whales => self.move_whale_sel(-3),
                View::Flow => self.move_flow_sel(-3),
                View::Wallet => self.move_wallet_sel(-3),
                View::Funds if !self.exec.captures() => self.exec_move_focus(-1),
                _ => {}
            },
            _ => {}
        }
    }

    // ── panel de ejecución (Vista 8, maqueta) ──────────────────────────────

    /// Entra a la Vista 8. El formulario opera sobre el par seleccionado
    /// global (mismo selector que las demás vistas), así que asegura uno.
    fn enter_funds(&mut self) {
        self.view = View::Funds;
        if self.selected_coin.is_none() {
            self.selected_coin = self.sorted_coins().get(self.sel).cloned();
        }
        if self.selected_coin.is_some() {
            self.sync_selected();
            self.request_extra(false);
        }
    }

    fn exec_max_lev(&self) -> u32 {
        self.selected_pair()
            .map(|p| p.meta.max_leverage as u32)
            .unwrap_or(50)
            .max(1)
    }

    fn exec_move_focus(&mut self, delta: i64) {
        self.exec.editing = false;
        self.exec.focus = exec::step_focus(
            self.exec.focus,
            delta,
            self.exec.typ,
            self.exec.positions.len(),
            self.exec.orders.len(),
        );
    }

    fn exec_lev_step(&mut self, delta: i64) {
        let max = self.exec_max_lev() as i64;
        self.exec.lev = (self.exec.lev as i64 + delta).clamp(1, max) as u32;
    }

    /// ←→ sobre el campo enfocado: par, lado, leverage, tipo o unidad.
    fn exec_adjust(&mut self, delta: i64) {
        match self.exec.focus {
            Focus::Pair => self.step_pair(delta),
            Focus::Side => self.exec.side = self.exec.side.flip(),
            Focus::Lev => self.exec_lev_step(delta),
            Focus::OrdType => {
                self.exec.typ = self.exec.typ.flip();
                self.exec.clamp_focus();
            }
            Focus::Size => self.exec.unit = self.exec.unit.flip(),
            _ => {}
        }
    }

    /// Enter sobre el campo/fila enfocado.
    fn exec_activate(&mut self) {
        match self.exec.focus {
            Focus::Pair => self.search.open(),
            Focus::Side => self.exec.side = self.exec.side.flip(),
            Focus::OrdType => {
                self.exec.typ = self.exec.typ.flip();
                self.exec.clamp_focus();
            }
            Focus::Lev => self.exec.lev_edit = Some(self.exec.lev.to_string()),
            Focus::LimitPx | Focus::Size | Focus::Sl | Focus::Tp => self.exec.editing = true,
            Focus::Submit => self.exec_submit(),
            Focus::Pos(i) => self.exec_open_sltp(i),
            Focus::Ord(i) => self.exec_cancel(i),
        }
    }

    /// Valida el formulario y abre el modal de confirmación con el resumen.
    /// En modo REAL valida además el margen disponible (perps_avail, la
    /// fuente correcta también en cuenta unificada) ANTES de permitir enviar.
    fn exec_submit(&mut self) {
        let Some(p) = self.selected_pair() else {
            self.exec.err = Some("sin par seleccionado todavía".into());
            return;
        };
        let (coin, mid, maxl) = (p.meta.name.clone(), p.mid, p.meta.max_leverage);
        match self.exec.draft(&coin, mid, maxl) {
            Ok(d) => {
                if self.exec.real {
                    // el SDK no agrupa TP/SL con una límite descansando (sin
                    // posición aún, un trigger reduce-only no tiene qué
                    // cerrar) — honestidad antes que colocar algo roto
                    if d.typ == exec::OrdType::Limit && (d.sl.is_some() || d.tp.is_some()) {
                        self.exec.err = Some(
                            "orden límite real: deja SL/TP vacíos y ponlos con e \
                             cuando la posición exista (tras el fill)"
                                .into(),
                        );
                        return;
                    }
                    let need = d.sz_usd / d.lev.max(1) as f64;
                    match self.perps_avail() {
                        None => {
                            self.exec.err =
                                Some("margen aún sin leer — espera unos segundos".into());
                            return;
                        }
                        Some(a) if need > a => {
                            self.exec.err = Some(format!(
                                "margen insuficiente: requiere ${need:.2}, disponible ${a:.2}"
                            ));
                            return;
                        }
                        Some(_) => {}
                    }
                }
                self.exec.err = None;
                self.exec_open_confirm(Confirm::Order(d));
            }
            Err(e) => self.exec.err = Some(e),
        }
    }

    fn exec_open_sltp(&mut self, i: usize) {
        let Some(p) = self.exec.positions.get(i) else {
            return;
        };
        let fmtv = |v: Option<f64>| v.map(fmt_px).unwrap_or_default();
        self.exec.sltp = Some(SlTpEdit {
            pos: i,
            sl: fmtv(p.sl),
            tp: fmtv(p.tp),
            on_tp: false,
            err: None,
        });
    }

    /// Abre el modal de confirmación. En mainnet real activa además la frase
    /// reforzada (escribir CONFIRMO): aquí hay dinero de verdad en juego, un
    /// `y` o un click accidental no deben bastar.
    fn exec_open_confirm(&mut self, c: Confirm) {
        self.exec.confirm_phrase =
            (self.exec.real && self.net_label == "mainnet").then(String::new);
        self.exec.confirm = Some(c);
    }

    fn exec_confirm_yes(&mut self) {
        // guarda de la frase mainnet: cubre también el click en el botón
        if let Some(typed) = &self.exec.confirm_phrase {
            if typed != exec::MAINNET_PHRASE {
                self.exec.err = Some(format!(
                    "mainnet: escribe {} y pulsa Enter para ejecutar",
                    exec::MAINNET_PHRASE
                ));
                return;
            }
        }
        self.exec.confirm_phrase = None;
        if let Some(c) = self.exec.confirm.take() {
            if self.exec.real {
                self.exec_confirm_real(c);
            } else {
                match c {
                    Confirm::Order(d) => self.exec.place(&d),
                    Confirm::Close(i) => self.exec.close_pos(i),
                }
            }
        }
    }

    /// Confirmación en modo REAL: el comando viaja al trader (firma con la
    /// agent key) y las filas NO se tocan aquí — se refrescan con la verdad
    /// del exchange tras la acción.
    fn exec_confirm_real(&mut self, c: Confirm) {
        let Some(t) = &self.trade else {
            return;
        };
        match c {
            Confirm::Order(d) => {
                let (sz_dec, mid) = self
                    .pairs
                    .get(&d.coin)
                    .map(|p| (p.meta.sz_decimals, p.mid))
                    .unwrap_or((4, 0.0));
                let _ = t.tx.send(TraderCmd::Open {
                    coin: d.coin.clone(),
                    is_buy: d.side.is_long(),
                    lev: d.lev,
                    limit_px: (d.typ == exec::OrdType::Limit).then_some(d.entry),
                    sz: d.sz_asset,
                    sz_decimals: sz_dec,
                    mid,
                    sl: d.sl,
                    tp: d.tp,
                });
                self.exec.err = None;
                self.exec.status = Some(format!(
                    "enviando orden REAL {} {} al exchange…",
                    d.side.label(),
                    d.coin
                ));
            }
            Confirm::Close(i) => {
                let Some(p) = self.exec.positions.get(i) else {
                    return;
                };
                let (sz_dec, mid) = self
                    .pairs
                    .get(&p.coin)
                    .map(|x| (x.meta.sz_decimals, x.mid))
                    .unwrap_or((4, 0.0));
                let _ = t.tx.send(TraderCmd::Close {
                    coin: p.coin.clone(),
                    szi: p.szi,
                    sz_decimals: sz_dec,
                    mid,
                });
                self.exec.err = None;
                self.exec.status = Some(format!("cerrando {} a mercado (REAL)…", p.coin));
            }
        }
    }

    /// Cancela la orden enfocada: real (por oid, vía trader) o maqueta.
    fn exec_cancel(&mut self, i: usize) {
        if !self.exec.real {
            self.exec.cancel_ord(i);
            return;
        }
        let Some(t) = &self.trade else {
            return;
        };
        let Some(o) = self.exec.orders.get(i) else {
            return;
        };
        let Some(oid) = o.oid else {
            return;
        };
        let _ = t.tx.send(TraderCmd::Cancel {
            coin: o.coin.clone(),
            oid,
        });
        self.exec.err = None;
        self.exec.status = Some(format!("cancelando orden {oid} (REAL)…"));
    }

    /// Enter del modal SL/TP: parsea contra la entrada de la posición y, si
    /// todo es coherente, aplica y sincroniza las órdenes trigger.
    fn exec_sltp_commit(&mut self) {
        let Some(m) = self.exec.sltp.clone() else {
            return;
        };
        let Some(p) = self.exec.positions.get(m.pos) else {
            self.exec.sltp = None;
            return;
        };
        let (entry, long) = (p.entry, p.is_long());
        let parse = |s: &str, is_sl: bool| -> Result<Option<f64>, String> {
            let v = exec::parse_trigger(s, entry, long, is_sl)?;
            if let Some(px) = v {
                if let Some(e) = exec::trigger_side_err(px, entry, long, is_sl) {
                    return Err(e.into());
                }
            }
            Ok(v)
        };
        match (parse(&m.sl, true), parse(&m.tp, false)) {
            (Ok(sl), Ok(tp)) => {
                if self.exec.real {
                    self.exec_sltp_real(m.pos, sl, tp);
                } else {
                    self.exec.apply_sltp(m.pos, sl, tp);
                }
                self.exec.sltp = None;
            }
            (Err(e), _) | (_, Err(e)) => {
                if let Some(mm) = &mut self.exec.sltp {
                    mm.err = Some(e);
                }
            }
        }
    }

    /// SL/TP en modo REAL: cancela los triggers actuales de la posición y
    /// coloca los nuevos (reduce-only) vía el trader. Hyperliquid opera en
    /// modo one-way (una posición por par), así que "los triggers de este
    /// par al lado de cierre" identifica sin ambigüedad los de la posición.
    fn exec_sltp_real(&mut self, pos: usize, sl: Option<f64>, tp: Option<f64>) {
        let Some(t) = &self.trade else {
            return;
        };
        let Some(p) = self.exec.positions.get(pos) else {
            return;
        };
        let close_side = if p.is_long() {
            exec::Side::Short
        } else {
            exec::Side::Long
        };
        let cancel_oids: Vec<u64> = self
            .exec
            .orders
            .iter()
            .filter(|o| {
                o.coin == p.coin
                    && o.side == close_side
                    && matches!(o.kind, exec::OrdKind::Sl | exec::OrdKind::Tp)
            })
            .filter_map(|o| o.oid)
            .collect();
        let sz_dec = self
            .pairs
            .get(&p.coin)
            .map(|x| x.meta.sz_decimals)
            .unwrap_or(4);
        let _ = t.tx.send(TraderCmd::SetTriggers {
            coin: p.coin.clone(),
            szi: p.szi,
            sz_decimals: sz_dec,
            cancel_oids,
            sl,
            tp,
        });
        self.exec.err = None;
        self.exec.status = Some(format!("actualizando SL/TP de {} (REAL)…", p.coin));
    }

    /// Teclas de la Vista 8 fuera de edición/modales: navegación del panel
    /// de ejecución + conexión WalletConnect de la cuenta maestra.
    fn handle_funds_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('c') => {
                // feedback inmediato; el gestor emitirá los estados reales
                self.wc = WcStatus::Connecting;
                let _ = self.wc_tx.send(WcCmd::Connect);
            }
            KeyCode::Char('d') => {
                let _ = self.wc_tx.send(WcCmd::Disconnect);
            }
            // depósito real al bridge (solo con sesión, mainnet y saldo leído)
            KeyCode::Char('p') => self.deposit_open(),
            // retiro real (con sesión y withdrawable leído; mainnet o testnet)
            KeyCode::Char('w') => self.withdraw_open(),
            // autorización de agent wallet (firma única de la maestra)
            KeyCode::Char('a') => self.agent_open(),
            // transferencia interna spot⇄perps (gasless, sin comisión)
            KeyCode::Char('t') => self.transfer_open(),
            KeyCode::Esc => self.view = View::Ranking,
            KeyCode::Down | KeyCode::Char('j') => self.exec_move_focus(1),
            KeyCode::Up | KeyCode::Char('k') => self.exec_move_focus(-1),
            KeyCode::Left | KeyCode::Char('h') => self.exec_adjust(-1),
            KeyCode::Right | KeyCode::Char('l') => self.exec_adjust(1),
            KeyCode::Char('+') => self.exec_lev_step(1),
            KeyCode::Char('-') => self.exec_lev_step(-1),
            KeyCode::Char('/') => self.search.open(),
            KeyCode::Enter => self.exec_activate(),
            // x cierra la posición (con confirmación) o cancela la orden
            KeyCode::Char('x') => match self.exec.focus {
                Focus::Pos(i) => self.exec_open_confirm(Confirm::Close(i)),
                Focus::Ord(i) => self.exec_cancel(i),
                _ => {}
            },
            KeyCode::Char('e') => {
                if let Focus::Pos(i) = self.exec.focus {
                    self.exec_open_sltp(i);
                }
            }
            _ => {}
        }
    }

    /// Teclado con el panel capturando: modales o edición de un input.
    fn handle_exec_capture(&mut self, key: KeyEvent) {
        // modal de confirmación (orden nueva o cierre de posición)
        if self.exec.confirm.is_some() {
            // mainnet real: hay que teclear la frase (CONFIRMO) y Enter — el
            // atajo `y` queda deshabilitado a propósito, y `n` es una letra
            // más del buffer (solo Esc cancela), para que ninguna pulsación
            // suelta ejecute ni cierre por accidente con dinero real
            if let Some(buf) = &mut self.exec.confirm_phrase {
                match key.code {
                    KeyCode::Esc => {
                        self.exec.confirm = None;
                        self.exec.confirm_phrase = None;
                        self.exec.status = Some("acción cancelada".into());
                    }
                    KeyCode::Enter => self.exec_confirm_yes(),
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(ch) => {
                        if buf.len() < exec::MAINNET_PHRASE.len() {
                            buf.push(ch.to_ascii_uppercase());
                        }
                    }
                    _ => {}
                }
                return;
            }
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.exec_confirm_yes(),
                KeyCode::Char('n') | KeyCode::Esc => {
                    self.exec.confirm = None;
                    self.exec.status = Some("acción cancelada".into());
                }
                _ => {}
            }
            return;
        }
        // modal SL/TP de una posición abierta
        if self.exec.sltp.is_some() {
            self.handle_sltp_key(key);
            return;
        }
        // edición numérica del apalancamiento
        if let Some(buf) = &mut self.exec.lev_edit {
            match key.code {
                KeyCode::Esc => self.exec.lev_edit = None,
                KeyCode::Enter => {
                    if let Ok(v) = buf.parse::<i64>() {
                        let max = self.exec_max_lev() as i64;
                        self.exec.lev = v.clamp(1, max) as u32;
                    }
                    self.exec.lev_edit = None;
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if c.is_ascii_digit() && buf.len() < 3 => buf.push(c),
                _ => {}
            }
            return;
        }
        // edición de texto del campo enfocado (LimitPx/Size/SL/TP)
        match key.code {
            KeyCode::Esc | KeyCode::Enter => self.exec.editing = false,
            KeyCode::Down => self.exec_move_focus(1),
            KeyCode::Up => self.exec_move_focus(-1),
            KeyCode::Backspace => {
                if let Some(s) = self.exec.focused_input_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() || c == '.' || c == '%' => {
                if let Some(s) = self.exec.focused_input_mut() {
                    if s.len() < 12 {
                        s.push(c);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_sltp_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Enter {
            self.exec_sltp_commit();
            return;
        }
        let Some(m) = &mut self.exec.sltp else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.exec.sltp = None,
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => m.on_tp = !m.on_tp,
            KeyCode::Backspace => {
                let b = if m.on_tp { &mut m.tp } else { &mut m.sl };
                b.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() || c == '.' || c == '%' => {
                let b = if m.on_tp { &mut m.tp } else { &mut m.sl };
                if b.len() < 12 {
                    b.push(c);
                }
            }
            _ => {}
        }
    }

    /// Click izquierdo en la Vista 8: resuelve contra el hitmap del frame.
    fn exec_click(&mut self, x: u16, y: u16) {
        let hit = self
            .exec
            .hits
            .iter()
            .find(|(r, _)| r.contains(Position::new(x, y)))
            .map(|(r, h)| (*r, *h));
        let Some((r, h)) = hit else {
            return;
        };
        match h {
            Hit::Focus(fo) => {
                self.exec.editing = false;
                self.exec.focus = fo;
            }
            Hit::Edit(fo) => {
                // dentro del modal SL/TP, los mismos targets cambian de campo
                if let Some(m) = &mut self.exec.sltp {
                    m.on_tp = fo == Focus::Tp;
                } else {
                    self.exec.focus = fo;
                    self.exec.editing = true;
                }
            }
            Hit::SetSide(s) => {
                self.exec.focus = Focus::Side;
                self.exec.editing = false;
                self.exec.side = s;
            }
            Hit::SetType(t) => {
                self.exec.focus = Focus::OrdType;
                self.exec.editing = false;
                self.exec.typ = t;
                self.exec.clamp_focus();
            }
            Hit::SetUnit(u) => {
                self.exec.focus = Focus::Size;
                self.exec.unit = u;
            }
            Hit::PairStep(d) => self.step_pair(d),
            Hit::LevStep(d) => {
                self.exec.focus = Focus::Lev;
                self.exec_lev_step(d);
            }
            Hit::LevSlider => {
                self.exec.focus = Focus::Lev;
                self.exec.lev_drag = true;
                let max = self.exec_max_lev();
                self.exec.lev = exec::slider_lev(x.saturating_sub(r.x), r.width, max);
            }
            Hit::Submit => {
                self.exec.focus = Focus::Submit;
                self.exec.editing = false;
                self.exec_submit();
            }
            Hit::ConfirmYes => {
                if self.deposit_ui.is_some() {
                    self.deposit_modal_yes();
                } else if self.withdraw_ui.is_some() {
                    self.withdraw_modal_yes();
                } else if self.agent_ui.is_some() {
                    self.agent_confirm_yes();
                } else if self.transfer_ui.is_some() {
                    self.transfer_modal_yes();
                } else if self.exec.sltp.is_some() {
                    self.exec_sltp_commit();
                } else {
                    self.exec_confirm_yes();
                }
            }
            Hit::ConfirmNo => {
                if self.deposit_ui.is_some() {
                    self.deposit_ui = None;
                } else if self.withdraw_ui.is_some() {
                    self.withdraw_ui = None;
                } else if self.agent_ui.is_some() {
                    self.agent_ui = None;
                } else if self.transfer_ui.is_some() {
                    self.transfer_ui = None;
                } else if self.exec.sltp.is_some() {
                    self.exec.sltp = None;
                } else {
                    self.exec.confirm = None;
                    self.exec.confirm_phrase = None;
                }
            }
            // click en la línea de sentido del modal de transferencia
            Hit::XferDir => self.transfer_toggle_dir(),
            Hit::ClosePos => {
                if let Focus::Pos(i) = self.exec.focus {
                    self.exec_open_confirm(Confirm::Close(i));
                }
            }
            Hit::EditSlTp => {
                if let Focus::Pos(i) = self.exec.focus {
                    self.exec_open_sltp(i);
                }
            }
            Hit::CancelOrd => {
                if let Focus::Ord(i) = self.exec.focus {
                    self.exec_cancel(i);
                }
            }
        }
    }

    /// Arrastre sobre el slider de apalancamiento.
    fn exec_slider_at(&mut self, x: u16) {
        let slider = self
            .exec
            .hits
            .iter()
            .find(|(_, h)| *h == Hit::LevSlider)
            .map(|(r, _)| *r);
        if let Some(r) = slider {
            let max = self.exec_max_lev();
            self.exec.lev = exec::slider_lev(x.saturating_sub(r.x), r.width, max);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.input_mode {
            self.handle_input_key(key);
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        if self.search.active {
            self.handle_search_key(key);
            return;
        }
        // modales de depósito/retiro reales: por delante de todo lo demás
        if self.view == View::Funds && self.deposit_ui.is_some() {
            self.handle_deposit_key(key);
            return;
        }
        if self.view == View::Funds && self.withdraw_ui.is_some() {
            self.handle_withdraw_key(key);
            return;
        }
        if self.view == View::Funds && self.agent_ui.is_some() {
            self.handle_agent_key(key);
            return;
        }
        if self.view == View::Funds && self.transfer_ui.is_some() {
            self.handle_transfer_key(key);
            return;
        }
        // edición/modales del panel de ejecución: capturan TODO el teclado
        // (los dígitos de los inputs son también atajos globales de vista)
        if self.view == View::Funds && self.exec.captures() {
            self.handle_exec_capture(key);
            return;
        }
        match key.code {
            KeyCode::Char('?') => {
                self.show_help = true;
                return;
            }
            KeyCode::Char('L') => {
                crate::i18n::toggle_lang();
                return;
            }
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('1') => {
                self.view = View::Ranking;
                return;
            }
            KeyCode::Char('2') => {
                self.goto_pair_view(View::Pair);
                return;
            }
            KeyCode::Char('3') => {
                self.goto_pair_view(View::WhaleRsi);
                return;
            }
            KeyCode::Char('4') => {
                self.view = View::Heatmap;
                return;
            }
            KeyCode::Char('5') => {
                self.goto_pair_view(View::Liq);
                return;
            }
            KeyCode::Char('6') => {
                self.enter_flow();
                return;
            }
            KeyCode::Char('7') => {
                self.view = View::Whales;
                return;
            }
            KeyCode::Char('8') => {
                self.enter_funds();
                return;
            }
            KeyCode::Char('9') => {
                self.view = View::Wallet;
                return;
            }
            KeyCode::Tab => {
                self.cycle_view();
                return;
            }
            _ => {}
        }
        match self.view {
            View::Ranking => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
                KeyCode::PageDown => self.move_sel(15),
                KeyCode::PageUp => self.move_sel(-15),
                KeyCode::Home | KeyCode::Char('g') => self.sel = 0,
                KeyCode::End | KeyCode::Char('G') => self.sel = self.pairs.len().saturating_sub(1),
                KeyCode::Enter => self.enter_pair_from_sel(),
                KeyCode::Char('/') => self.search.open(),
                KeyCode::Char('s') => {
                    // mantener el par seleccionado al cambiar de orden
                    let coin = self.sorted_coins().get(self.sel).cloned();
                    self.sort = self.sort.next();
                    self.sort_desc = self.sort.default_desc();
                    if let Some(c) = coin {
                        if let Some(i) = self.sorted_coins().iter().position(|x| *x == c) {
                            self.sel = i;
                        }
                    }
                }
                KeyCode::Char('r') => {
                    let coin = self.sorted_coins().get(self.sel).cloned();
                    self.sort_desc = !self.sort_desc;
                    if let Some(c) = coin {
                        if let Some(i) = self.sorted_coins().iter().position(|x| *x == c) {
                            self.sel = i;
                        }
                    }
                }
                _ => {}
            },
            View::Pair | View::WhaleRsi => match key.code {
                KeyCode::Esc | KeyCode::Backspace => self.view = View::Ranking,
                KeyCode::Left | KeyCode::Char('h') => self.step_pair(-1),
                KeyCode::Right | KeyCode::Char('l') => self.step_pair(1),
                KeyCode::Char('i') => self.cycle_interval(),
                KeyCode::Char('u') => self.request_extra(true),
                _ => {}
            },
            View::Heatmap => match key.code {
                KeyCode::Char('m') => self.heat_metric = self.heat_metric.next(),
                KeyCode::Esc => self.view = View::Ranking,
                _ => {}
            },
            View::Whales if self.whale_modal.is_some() => match key.code {
                KeyCode::Char('c') => self.copy_whale_addr(),
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.whale_modal = None,
                _ => {}
            },
            View::Whales => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.move_whale_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_whale_sel(-1),
                KeyCode::PageDown => self.move_whale_sel(15),
                KeyCode::PageUp => self.move_whale_sel(-15),
                KeyCode::Enter => self.open_whale_modal(),
                KeyCode::Esc => self.view = View::Ranking,
                _ => {}
            },
            View::Wallet if self.wallet_pos_modal.is_some() => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.wallet_pos_modal = None,
                _ => {}
            },
            View::Wallet => match key.code {
                KeyCode::Char('e') | KeyCode::Char('a') => self.start_input(),
                // sin dirección observada aún, Enter equivale a introducir una
                KeyCode::Enter if self.wallet_addr.is_none() => self.start_input(),
                KeyCode::Down | KeyCode::Char('j') => self.move_wallet_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_wallet_sel(-1),
                KeyCode::Enter => self.open_wallet_pos_modal(),
                KeyCode::Esc => self.view = View::Ranking,
                _ => {}
            },
            View::Liq => match key.code {
                KeyCode::Esc | KeyCode::Backspace => self.view = View::Ranking,
                KeyCode::Left | KeyCode::Char('h') => self.step_pair(-1),
                KeyCode::Right | KeyCode::Char('l') => self.step_pair(1),
                KeyCode::Char('i') => self.cycle_interval(),
                KeyCode::Char('r') => {
                    self.liq_range_idx = (self.liq_range_idx + 1) % LIQ_RANGES.len()
                }
                KeyCode::Char('u') => self.request_extra(true),
                _ => {}
            },
            View::Funds => self.handle_funds_key(key),
            View::Flow => match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.move_flow_sel(1),
                KeyCode::Up | KeyCode::Char('k') => self.move_flow_sel(-1),
                KeyCode::PageDown => self.move_flow_sel(15),
                KeyCode::PageUp => self.move_flow_sel(-15),
                KeyCode::Home | KeyCode::Char('g') => self.flow_sel = 0,
                KeyCode::End | KeyCode::Char('G') => {
                    self.flow_sel = self.pairs.len().saturating_sub(1)
                }
                KeyCode::Enter => self.flow_select(),
                KeyCode::Char('/') => self.search.open(),
                KeyCode::Char('w') => self.flow_win = self.flow_win.next(),
                KeyCode::Char('s') => {
                    // mismo gesto que en Ranking: s cicla el orden, el cursor
                    // sigue al par que estaba seleccionado
                    let coin = self.flow_coins().get(self.flow_sel).cloned();
                    self.flow_sort = self.flow_sort.next();
                    self.flow_desc = true;
                    self.keep_flow_cursor(coin);
                }
                KeyCode::Char('r') => {
                    let coin = self.flow_coins().get(self.flow_sel).cloned();
                    self.flow_desc = !self.flow_desc;
                    self.keep_flow_cursor(coin);
                }
                KeyCode::Char('u') => self.request_extra(true),
                KeyCode::Esc => self.view = View::Ranking,
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::walletconnect::WcSession;

    fn candle(t_close: u64) -> CandlePoint {
        CandlePoint {
            t_close,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
        }
    }

    /// El delta por vela agrega los minutos dentro de la ventana de cada vela y
    /// marca `None` las velas anteriores al arranque del tracking (warmup).
    #[test]
    fn delta_per_candle_buckets_and_warmup() {
        let mut d = DeltaState::new("BTC".into());
        // ms de un minuto cualquiera alineado; los minutos avanzan de 60_000
        let m = |k: u64| 1_000 * 60_000 + k * 60_000;
        d.push(300.0, 100.0, m(0) + 5); // min 0: neto +200
        d.push(50.0, 200.0, m(1) + 5); // min 1: neto -150
        d.push(500.0, 100.0, m(2) + 5); // min 2: neto +400

        // velas de 1m: cierre = apertura + 60s - 1ms
        let iv = 60_000;
        let cl = |k: u64| m(k) + iv - 1;
        // una vela previa al tracking sale None; las tres con datos suman su min
        let cands = [candle(m(0) - 1), candle(cl(0)), candle(cl(1)), candle(cl(2))];
        let out = d.per_candle(&cands, iv);
        assert_eq!(out[0], None);
        assert_eq!(out[1], Some(200.0));
        assert_eq!(out[2], Some(-150.0));
        assert_eq!(out[3], Some(400.0));

        // una vela de 5m que empieza un minuto antes del tracking → None
        let iv5 = 5 * 60_000;
        let c5 = candle(m(4) - 1); // apertura = m(0)-60_000 < primer min
        assert_eq!(d.per_candle(&[c5], iv5)[0], None);

        // vela de 5m con apertura = primer min rastreado (min 0..4) suma 0..2
        let c5b = candle(m(0) + iv5 - 1); // apertura = m(0), cierre en min 4
        assert_eq!(d.per_candle(&[c5b], iv5)[0], Some(450.0));
    }

    const MASTER: &str = "0x000000000000000000000000000000000000dead";
    const WATCH: &str = "0x000000000000000000000000000000000000beef";

    type UsdcRx = watch::Receiver<Option<(Address, String)>>;
    type WcRx = mpsc::UnboundedReceiver<WcCmd>;

    fn test_app() -> (App, watch::Receiver<Vec<Address>>, UsdcRx, WcRx) {
        // sin tty en tests: forzar protocolo para que Gfx no interrogue stdin
        std::env::set_var("CHART_PROTO", "halfblocks");
        let (extra_tx, _extra) = mpsc::channel(8);
        let (wallet_tx, wallet_rx) = watch::channel(Vec::new());
        let (usdc_tx, usdc_rx) = watch::channel(None);
        let (coin_tx, _coin) = watch::channel(None);
        let (wc_tx, wc_rx) = mpsc::unbounded_channel();
        let app = App::new(extra_tx, wallet_tx, usdc_tx, coin_tx, wc_tx, "test", Gfx::new());
        (app, wallet_rx, usdc_rx, wc_rx)
    }

    fn connect_on(app: &mut App, addr: &str, chain: &str) {
        app.apply_msg(DataMsg::Wc(WcStatus::Connected(WcSession {
            address: addr.to_string(),
            chain: chain.into(),
            peer: None,
            since: Instant::now(),
            session_topic: "topic".into(),
        })));
    }

    fn connect(app: &mut App, addr: &str) {
        connect_on(app, addr, "eip155:42161");
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::from(code));
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Dirección watch-only por el mismo camino que el usuario (overlay `e`).
    fn set_watch(app: &mut App, addr: &str) {
        app.start_input();
        for c in addr.chars() {
            app.handle_input_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_input_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.input_err.is_none(), "dirección de test inválida");
    }

    fn snap(addr_fmt: &str, value: f64) -> AccountSnapshot {
        AccountSnapshot {
            addr: addr_fmt.to_string(),
            account_value: value,
            withdrawable: value,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: Vec::new(),
        }
    }

    /// Al conectar WC se observa la cuenta maestra junto a la watch-only y
    /// cada snapshot va a su hueco; al caer la sesión, se retira y limpia.
    #[test]
    fn fondos_sigue_a_la_sesion_wc() {
        let (mut app, wallet_rx, usdc_rx, _wc) = test_app();
        set_watch(&mut app, WATCH);
        connect(&mut app, MASTER);
        assert_eq!(wallet_rx.borrow().len(), 2);
        // el watcher de USDC recibe (dirección, chain) de la sesión
        assert_eq!(
            usdc_rx.borrow().as_ref().map(|(_, c)| c.clone()),
            Some("eip155:42161".to_string())
        );

        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 123.0)));
        assert_eq!(app.funds.as_ref().map(|w| w.account_value), Some(123.0));
        assert!(app.wallet.is_none(), "el hueco watch-only no debe tocarse");

        app.apply_msg(DataMsg::UsdcBalance {
            addr: master_fmt.clone(),
            usdc: Some(42.5),
        });
        assert_eq!(app.usdc, Some(Some(42.5)));

        // snapshot de otra dirección no debe pisar el saldo de la maestra
        app.apply_msg(DataMsg::UsdcBalance {
            addr: "0xotra".into(),
            usdc: Some(9.9),
        });
        assert_eq!(app.usdc, Some(Some(42.5)));

        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.funds.is_none() && app.funds_at.is_none());
        assert!(app.usdc.is_none() && app.usdc_at.is_none());
        assert_eq!(wallet_rx.borrow().len(), 1);
        assert!(usdc_rx.borrow().is_none());
    }

    /// Watch-only y maestra iguales: un solo target y la misma respuesta
    /// alimenta ambos huecos.
    #[test]
    fn misma_direccion_un_solo_target() {
        let (mut app, wallet_rx, _usdc_rx, _wc) = test_app();
        set_watch(&mut app, MASTER);
        connect(&mut app, MASTER);
        assert_eq!(wallet_rx.borrow().len(), 1);

        let fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::WalletState(snap(&fmt, 7.0)));
        assert!(app.funds.is_some() && app.wallet.is_some());
    }

    /// Flujo completo del depósito real: `p` → cantidad → validaciones de
    /// mínimo y saldo → resumen → `y` envía WcCmd::Deposit con las unidades
    /// exactas y la ruta verificada. Los dígitos NO cambian de vista.
    #[test]
    fn deposito_valida_y_envia() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect(&mut app, MASTER);
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::UsdcBalance {
            addr: master_fmt,
            usdc: Some(20.0),
        });
        app.view = View::Funds;

        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.deposit_ui, Some(DepositUi::Amount { .. })));

        // por debajo del mínimo del bridge: bloqueado (se perdería)
        type_str(&mut app, "3");
        assert_eq!(app.view, View::Funds, "el dígito no debe cambiar de vista");
        press(&mut app, KeyCode::Enter);
        match &app.deposit_ui {
            Some(DepositUi::Amount { err, .. }) => {
                assert!(err.is_some(), "3 USDC debe fallar (mínimo 5)")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // más que el saldo on-chain: bloqueado
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, "25");
        press(&mut app, KeyCode::Enter);
        match &app.deposit_ui {
            Some(DepositUi::Amount { err, .. }) => {
                assert!(err.is_some(), "25 USDC con saldo 20 debe fallar")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // 7.5 USDC pasa al resumen y `y` lo envía al gestor WC
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, "7.5");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.deposit_ui,
            Some(DepositUi::Confirm { units: 7_500_000, .. })
        ));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.deposit_ui.is_none());
        assert!(matches!(
            app.deposit,
            Some(DepositStatus::AwaitingWallet { .. })
        ));
        match wc_rx.try_recv().expect("debe salir un WcCmd::Deposit") {
            WcCmd::Deposit(req) => {
                assert_eq!(req.units, 7_500_000);
                assert_eq!(req.bridge, "0x2df1c51e09aecf9cacb7bc98cb1742757f163df7");
                assert_eq!(req.token, "0xaf88d065e77c8cc2239327c5edb3a432268e5831");
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // al caer/cambiar la sesión, el estado del depósito no sobrevive
        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.deposit.is_none() && app.deposit_ui.is_none());
    }

    /// Flujo completo del retiro real contra TESTNET: `w` → cantidad →
    /// validaciones de comisión y withdrawable → resumen → `y` envía
    /// WcCmd::Withdraw con destino = la propia maestra y ruta de testnet.
    #[test]
    fn retiro_valida_y_envia_testnet() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect_on(&mut app, MASTER, "eip155:421614");
        let master_fmt = app.funds_addr().unwrap();
        // withdrawable real leído del clearinghouseState de testnet
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 1000.0)));
        app.view = View::Funds;

        press(&mut app, KeyCode::Char('w'));
        assert!(matches!(app.withdraw_ui, Some(WithdrawUi::Amount { .. })));

        // no cubre ni la comisión de $1: no llegaría nada
        type_str(&mut app, "1");
        assert_eq!(app.view, View::Funds, "el dígito no debe cambiar de vista");
        press(&mut app, KeyCode::Enter);
        match &app.withdraw_ui {
            Some(WithdrawUi::Amount { err, .. }) => {
                assert!(err.is_some(), "1 USDC debe fallar (== comisión)")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // más que el withdrawable: bloqueado
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, "1001");
        press(&mut app, KeyCode::Enter);
        match &app.withdraw_ui {
            Some(WithdrawUi::Amount { err, .. }) => {
                assert!(err.is_some(), "1001 con withdrawable 1000 debe fallar")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // 10 USDC pasa al resumen y `y` lo envía al gestor WC
        for _ in 0..4 {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "10");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.withdraw_ui,
            Some(WithdrawUi::Confirm { units: 10_000_000, .. })
        ));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.withdraw_ui.is_none());
        assert!(matches!(
            app.withdraw,
            Some(WithdrawStatus::AwaitingWallet { .. })
        ));
        match wc_rx.try_recv().expect("debe salir un WcCmd::Withdraw") {
            WcCmd::Withdraw(req) => {
                assert_eq!(req.units, 10_000_000);
                assert_eq!(req.amount, "10");
                assert_eq!(req.destination, master_fmt, "destino = la propia maestra");
                assert_eq!(req.api, "https://api.hyperliquid-testnet.xyz");
                assert_eq!(req.hl_chain, "Testnet");
                assert_eq!(req.chain_id, 421_614);
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // al caer/cambiar la sesión, el estado del retiro no sobrevive
        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.withdraw.is_none() && app.withdraw_ui.is_none());
    }

    /// La MISMA función en mainnet elige el endpoint de mainnet; y sin
    /// withdrawable leído (sin snapshot) la tecla `w` no abre nada.
    #[test]
    fn retiro_ruta_mainnet_y_sin_saldo() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect(&mut app, MASTER);
        app.view = View::Funds;

        // sin clearinghouseState aún: no hay ruta, el modal no se abre
        press(&mut app, KeyCode::Char('w'));
        assert!(app.withdraw_ui.is_none(), "sin withdrawable no se abre");

        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 50.0)));
        press(&mut app, KeyCode::Char('w'));
        type_str(&mut app, "10");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('y'));
        match wc_rx.try_recv().expect("debe salir un WcCmd::Withdraw") {
            WcCmd::Withdraw(req) => {
                assert_eq!(req.api, "https://api.hyperliquid.xyz");
                assert_eq!(req.hl_chain, "Mainnet");
                assert_eq!(req.chain_id, 42_161);
            }
            other => panic!("comando inesperado: {other:?}"),
        }
    }

    /// Flujo completo de la autorización del agent contra TESTNET: `a` genera
    /// la clave y abre el resumen, `y` envía WcCmd::ApproveAgent con la clave
    /// privada que corresponde a la dirección mostrada; Esc descarta sin
    /// enviar nada (y la clave descartada jamás tocó el disco).
    #[test]
    fn agent_genera_confirma_y_envia_testnet() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect_on(&mut app, MASTER, "eip155:421614");
        app.view = View::Funds;

        // sin clearinghouseState aún: sin cuenta leída no se abre
        press(&mut app, KeyCode::Char('a'));
        assert!(app.agent_ui.is_none(), "sin snapshot no se abre");

        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 999.0)));

        press(&mut app, KeyCode::Char('a'));
        let ui1 = app.agent_ui.clone().expect("modal abierto");
        assert!(ui1.agent_addr.starts_with("0x") && ui1.agent_addr.len() == 42);

        // Esc descarta sin enviar nada
        press(&mut app, KeyCode::Esc);
        assert!(app.agent_ui.is_none());
        assert!(wc_rx.try_recv().is_err(), "no debe salir ningún comando");

        // cada intento genera una clave nueva
        press(&mut app, KeyCode::Char('a'));
        let ui2 = app.agent_ui.clone().unwrap();
        assert_ne!(ui2.agent_addr, ui1.agent_addr);

        press(&mut app, KeyCode::Char('y'));
        assert!(app.agent_ui.is_none());
        assert!(matches!(app.agent, Some(AgentStatus::AwaitingWallet { .. })));
        match wc_rx.try_recv().expect("debe salir un WcCmd::ApproveAgent") {
            WcCmd::ApproveAgent(req) => {
                assert_eq!(req.agent_address, ui2.agent_addr);
                assert_eq!(req.master, master_fmt);
                assert_eq!(req.api, "https://api.hyperliquid-testnet.xyz");
                assert_eq!(req.hl_chain, "Testnet");
                assert_eq!(req.chain_id, 421_614);
                // la clave privada del comando ES la de la dirección mostrada
                let bytes = alloy_primitives::hex::decode(&req.agent_priv).unwrap();
                let sk = k256::ecdsa::SigningKey::from_slice(&bytes).unwrap();
                let pk = sk.verifying_key().to_encoded_point(false);
                let hash = alloy_primitives::keccak256(&pk.as_bytes()[1..]);
                let derived = Address::from_slice(&hash[12..]);
                assert_eq!(format!("{derived}"), ui2.agent_addr);
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // al caer/cambiar la sesión, el estado del agent no sobrevive
        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.agent.is_none() && app.agent_ui.is_none());
    }

    /// Flujo completo de la transferencia interna contra TESTNET: `t` abre en
    /// spot→perps, valida contra el saldo spot (honesto si aún no se leyó),
    /// Tab alterna el sentido, y `y` envía WcCmd::ClassTransfer exacto.
    #[test]
    fn transferencia_valida_y_envia_testnet() {
        use crate::data::types::SpotSnapshot;

        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect_on(&mut app, MASTER, "eip155:421614");
        app.view = View::Funds;
        let master_fmt = app.funds_addr().unwrap();

        // se abre sin saldos leídos (la ruta solo pide sesión con chain)…
        press(&mut app, KeyCode::Char('t'));
        assert!(matches!(
            app.transfer_ui,
            Some(TransferUi::Amount { to_perp: true, .. })
        ));
        // …pero validar sin el saldo spot leído da error honesto, no 0
        type_str(&mut app, "10");
        press(&mut app, KeyCode::Enter);
        match &app.transfer_ui {
            Some(TransferUi::Amount { err, .. }) => {
                assert!(err.is_some(), "sin saldo spot leído debe fallar")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // llega el saldo spot (999 total, 1.5 en órdenes → 997.5 disponibles)
        app.apply_msg(DataMsg::SpotState(SpotSnapshot {
            addr: master_fmt.clone(),
            usdc_total: 999.0,
            usdc_hold: 1.5,
            usdc_avail: None,
            others: Vec::new(),
        }));
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, "998");
        press(&mut app, KeyCode::Enter);
        match &app.transfer_ui {
            Some(TransferUi::Amount { err, .. }) => {
                assert!(err.is_some(), "998 con 997.5 disponibles debe fallar")
            }
            other => panic!("esperaba Amount con error, hay {other:?}"),
        }

        // 10 USDC spot→perps pasa al resumen y `y` lo envía al gestor WC
        for _ in 0..3 {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "10");
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.transfer_ui,
            Some(TransferUi::Confirm {
                to_perp: true,
                units: 10_000_000,
                ..
            })
        ));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.transfer_ui.is_none());
        assert!(matches!(
            app.transfer,
            Some(TransferStatus::AwaitingWallet { to_perp: true, .. })
        ));
        match wc_rx.try_recv().expect("debe salir un WcCmd::ClassTransfer") {
            WcCmd::ClassTransfer(req) => {
                assert_eq!(req.units, 10_000_000);
                assert_eq!(req.amount, "10");
                assert!(req.to_perp);
                assert_eq!(req.master, master_fmt);
                assert_eq!(req.api, "https://api.hyperliquid-testnet.xyz");
                assert_eq!(req.hl_chain, "Testnet");
                assert_eq!(req.chain_id, 421_614);
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // al caer/cambiar la sesión, ni el estado ni el saldo spot sobreviven
        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.transfer.is_none() && app.transfer_ui.is_none());
        assert!(app.spot.is_none() && app.spot_at.is_none());
    }

    /// El sentido perps→spot valida contra el withdrawable de perps, y Tab
    /// alterna entre ambos sin perder la cantidad tecleada.
    #[test]
    fn transferencia_sentido_perps_a_spot() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect_on(&mut app, MASTER, "eip155:421614");
        app.view = View::Funds;
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 50.0)));

        press(&mut app, KeyCode::Char('t'));
        type_str(&mut app, "20");
        // Tab: spot→perps pasa a perps→spot, la cantidad se conserva
        press(&mut app, KeyCode::Tab);
        match &app.transfer_ui {
            Some(TransferUi::Amount { to_perp, buf, .. }) => {
                assert!(!to_perp, "Tab debe alternar el sentido");
                assert_eq!(buf, "20");
            }
            other => panic!("esperaba Amount, hay {other:?}"),
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('y'));
        match wc_rx.try_recv().expect("debe salir un WcCmd::ClassTransfer") {
            WcCmd::ClassTransfer(req) => {
                assert!(!req.to_perp);
                assert_eq!(req.amount, "20");
            }
            other => panic!("comando inesperado: {other:?}"),
        }
    }

    /// El saldo spot de otra dirección no pisa el de la maestra conectada.
    #[test]
    fn spot_enruta_por_direccion() {
        use crate::data::types::SpotSnapshot;

        let (mut app, _w, _u, _wc) = test_app();
        connect(&mut app, MASTER);
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::SpotState(SpotSnapshot {
            addr: "0xotra".into(),
            usdc_total: 5.0,
            usdc_hold: 0.0,
            usdc_avail: None,
            others: Vec::new(),
        }));
        assert!(app.spot.is_none(), "spot de otra dirección no debe entrar");
        app.apply_msg(DataMsg::SpotState(SpotSnapshot {
            addr: master_fmt,
            usdc_total: 999.0,
            usdc_hold: 0.0,
            usdc_avail: None,
            others: Vec::new(),
        }));
        assert_eq!(app.spot.as_ref().map(|s| s.usdc_total), Some(999.0));
    }

    /// Esc en el modal cancela sin enviar nada.
    #[test]
    fn deposito_esc_no_envia_nada() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect(&mut app, MASTER);
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::UsdcBalance {
            addr: master_fmt,
            usdc: Some(20.0),
        });
        app.view = View::Funds;
        press(&mut app, KeyCode::Char('p'));
        type_str(&mut app, "7.5");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc); // en el resumen: cancela
        assert!(app.deposit_ui.is_none());
        assert!(wc_rx.try_recv().is_err(), "no debe salir ningún comando");
    }

    /// Sin ruta verificada (chain distinta de mainnet Arbitrum) `p` no abre
    /// el modal: no existe depósito hacia bridges sin verificar.
    #[test]
    fn deposito_solo_mainnet() {
        let (mut app, _w, _u, _wc) = test_app();
        connect_on(&mut app, MASTER, "eip155:421614");
        let fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::UsdcBalance {
            addr: fmt,
            usdc: Some(50.0),
        });
        app.view = View::Funds;
        press(&mut app, KeyCode::Char('p'));
        assert!(app.deposit_ui.is_none());
    }

    /// Fixture de spot con disponible tras mantenimiento, como lo emite el
    /// watcher para una cuenta unificada real.
    fn spot_snap(addr_fmt: &str, total: f64, avail: Option<f64>) -> SpotSnapshot {
        SpotSnapshot {
            addr: addr_fmt.to_string(),
            usdc_total: total,
            usdc_hold: 0.0,
            usdc_avail: avail,
            others: Vec::new(),
        }
    }

    /// Cuenta UNIFICADA (el caso real del usuario en mainnet Y testnet,
    /// verificado 2026-07-20): el margen disponible sale del saldo spot — el
    /// clearinghouseState de perps dice 0 y NO es significativo — y la tecla
    /// `t` no abre el modal: deja un mensaje claro en vez del error crudo
    /// "action disabled when unified account is active" de Hyperliquid.
    #[test]
    fn cuenta_unificada_margen_de_spot_y_sin_transferencia() {
        let (mut app, _w, _u, mut wc_rx) = test_app();
        connect(&mut app, MASTER);
        app.view = View::Funds;
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::AccountMode {
            addr: master_fmt.clone(),
            mode: AccountMode::Unified,
        });
        // perps a 0 (lo que devuelve de verdad una cuenta unificada) + spot
        // con 5.000708 disponibles tras mantenimiento
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 0.0)));
        app.apply_msg(DataMsg::SpotState(spot_snap(
            &master_fmt,
            5.000708,
            Some(5.000708),
        )));

        assert!(app.is_unified());
        // fuente de verdad del margen: spot, no el 0 de perps
        assert_eq!(app.perps_avail(), Some(5.000708));
        // el retiro valida contra ese mismo disponible (con 0 de perps el
        // usuario no podría retirar sus 5 USDC reales)
        let (_, avail) = app.withdraw_route().expect("ruta de retiro");
        assert_eq!(avail, 5.000708);

        // `t` no abre modal ni envía nada: mensaje claro en su tira
        press(&mut app, KeyCode::Char('t'));
        assert!(app.transfer_ui.is_none(), "t no debe abrir modal");
        assert!(wc_rx.try_recv().is_err(), "no debe salir ningún comando");
        match &app.transfer {
            Some(TransferStatus::Failed { error }) => {
                assert!(error.contains("UNIFIED"), "mensaje poco claro: {error}")
            }
            other => panic!("esperaba el mensaje de no-aplica, hay {other:?}"),
        }

        // sin el campo de disponible, el proxy honesto es total − hold
        app.apply_msg(DataMsg::SpotState(spot_snap(&master_fmt, 7.0, None)));
        assert_eq!(app.perps_avail(), Some(7.0));

        // al caer la sesión el modo no sobrevive (nunca modo de otra cuenta)
        app.apply_msg(DataMsg::Wc(WcStatus::Idle));
        assert!(app.account_mode.is_none());
    }

    /// Cuenta ESTÁNDAR ("default"): todo el comportamiento clásico intacto —
    /// margen del withdrawable de perps y `t` abre el modal de transferencia.
    #[test]
    fn cuenta_estandar_conserva_comportamiento() {
        let (mut app, _w, _u, _wc) = test_app();
        connect(&mut app, MASTER);
        app.view = View::Funds;
        let master_fmt = app.funds_addr().unwrap();
        app.apply_msg(DataMsg::AccountMode {
            addr: master_fmt.clone(),
            mode: AccountMode::Standard("default".into()),
        });
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 50.0)));
        app.apply_msg(DataMsg::SpotState(spot_snap(&master_fmt, 999.0, None)));

        assert!(!app.is_unified());
        assert_eq!(app.perps_avail(), Some(50.0));
        press(&mut app, KeyCode::Char('t'));
        assert!(
            matches!(app.transfer_ui, Some(TransferUi::Amount { .. })),
            "en cuenta estándar t debe abrir el modal"
        );
    }

    /// El modo de otra dirección no pisa el de la maestra conectada (mismo
    /// enrutado por dirección que el saldo spot).
    #[test]
    fn modo_de_cuenta_enruta_por_direccion() {
        let (mut app, _w, _u, _wc) = test_app();
        connect(&mut app, MASTER);
        app.apply_msg(DataMsg::AccountMode {
            addr: "0xotra".into(),
            mode: AccountMode::Unified,
        });
        assert!(app.account_mode.is_none(), "modo de otra dirección no entra");
    }

    // ── panel de ejecución REAL (paso 7) ───────────────────────────────────

    /// Arma el trading real y devuelve el receptor de comandos del trader.
    fn arm(app: &mut App) -> mpsc::UnboundedReceiver<TraderCmd> {
        let (tx, rx) = mpsc::unbounded_channel();
        app.arm_trading(MASTER.parse().unwrap(), "0xAGENTEagenteAGENTE".into(), tx);
        rx
    }

    /// Siembra BTC con mid 100k (szDecimals 5, lev máx 40) y lo selecciona.
    fn pair_btc(app: &mut App) {
        let meta = PairMeta {
            name: "BTC".into(),
            sz_decimals: 5,
            max_leverage: 40,
        };
        let snap = CtxSnapshot {
            t: Instant::now(),
            t_ms: 0,
            mark_px: 100_000.0,
            mid_px: Some(100_000.0),
            oracle_px: 100_000.0,
            funding: 0.0,
            open_interest: 0.0,
            premium: None,
            day_ntl_vlm: 0.0,
            prev_day_px: 100_000.0,
        };
        app.apply_msg(DataMsg::Ctxs(vec![(meta, snap)]));
        app.selected_coin = Some("BTC".into());
    }

    fn pos_btc(szi: f64) -> crate::data::types::PosInfo {
        crate::data::types::PosInfo {
            coin: "BTC".into(),
            szi,
            entry_px: Some(100_000.0),
            position_value: szi.abs() * 100_000.0,
            unrealized_pnl: 0.0,
            roe: 0.0,
            leverage: 10,
            is_cross: false,
            liq_px: Some(90_500.0),
            since_open_funding: 0.0,
        }
    }

    /// Modo REAL: las filas del panel son la cuenta de verdad — posición del
    /// clearinghouseState con su liq REAL y SL derivado de su trigger, y las
    /// órdenes con oid. Cancelar/cerrar/SL-TP envían el comando exacto al
    /// trader en vez de tocar las listas.
    #[test]
    fn panel_real_sincroniza_filas_y_enruta_comandos() {
        let (mut app, wallet_rx, _u, _wc) = test_app();
        let mut trader_rx = arm(&mut app);
        pair_btc(&mut app);
        // sin sesión WC, la cuenta de trading ya se observa
        assert_eq!(wallet_rx.borrow().len(), 1);
        let master_fmt = app.trade.as_ref().unwrap().master_fmt.clone();

        let mut snap = snap(&master_fmt, 500.0);
        snap.positions = vec![pos_btc(0.01)];
        app.apply_msg(DataMsg::WalletState(snap));
        app.apply_msg(DataMsg::OpenOrders {
            addr: master_fmt.clone(),
            orders: vec![
                LiveOrd {
                    coin: "BTC".into(),
                    is_buy: false, // cierre de un long: vende
                    kind: "Stop Market".into(),
                    px: 95_000.0,
                    sz: 0.01,
                    oid: 11,
                    reduce_only: true,
                    is_trigger: true,
                },
                LiveOrd {
                    coin: "BTC".into(),
                    is_buy: true,
                    kind: "Limit".into(),
                    px: 80_000.0,
                    sz: 0.02,
                    oid: 22,
                    reduce_only: false,
                    is_trigger: false,
                },
            ],
        });

        // filas reales: posición con liq de la API y SL del trigger
        assert_eq!(app.exec.positions.len(), 1);
        let p = &app.exec.positions[0];
        assert_eq!(p.liq, Some(90_500.0), "liq REAL de la API, no estimada");
        assert_eq!(p.sl, Some(95_000.0), "SL derivado del trigger reduce-only");
        assert_eq!(p.tp, None);
        assert!(!p.demo);
        assert_eq!(app.exec.orders.len(), 2);
        assert_eq!(app.exec.orders[0].oid, Some(11));

        // cancelar la límite (fila 1) → Cancel con su oid, sin tocar listas
        app.view = View::Funds;
        app.exec.focus = Focus::Ord(1);
        press(&mut app, KeyCode::Char('x'));
        match trader_rx.try_recv().expect("debe salir Cancel") {
            TraderCmd::Cancel { coin, oid } => {
                assert_eq!(coin, "BTC");
                assert_eq!(oid, 22);
            }
            other => panic!("comando inesperado: {other:?}"),
        }
        assert_eq!(app.exec.orders.len(), 2, "la fila la quita el refresh, no el click");

        // cerrar la posición → confirmación explícita → Close con szi y mid
        app.exec.focus = Focus::Pos(0);
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(app.exec.confirm, Some(Confirm::Close(0))));
        press(&mut app, KeyCode::Char('y'));
        match trader_rx.try_recv().expect("debe salir Close") {
            TraderCmd::Close {
                coin,
                szi,
                sz_decimals,
                mid,
            } => {
                assert_eq!(coin, "BTC");
                assert_eq!(szi, 0.01);
                assert_eq!(sz_decimals, 5);
                assert_eq!(mid, 100_000.0);
            }
            other => panic!("comando inesperado: {other:?}"),
        }
        assert_eq!(app.exec.positions.len(), 1, "la posición la quita el refresh");

        // SL/TP nuevos → SetTriggers cancelando el trigger anterior (oid 11)
        app.exec_sltp_real(0, Some(94_000.0), Some(120_000.0));
        match trader_rx.try_recv().expect("debe salir SetTriggers") {
            TraderCmd::SetTriggers {
                coin,
                szi,
                cancel_oids,
                sl,
                tp,
                ..
            } => {
                assert_eq!(coin, "BTC");
                assert_eq!(szi, 0.01);
                assert_eq!(cancel_oids, vec![11]);
                assert_eq!(sl, Some(94_000.0));
                assert_eq!(tp, Some(120_000.0));
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // los eventos del trader aterrizan en la línea de estado
        app.apply_msg(DataMsg::Exec(ExecEvent::Failed("Order must have minimum value of $10.".into())));
        assert!(app.exec.err.as_deref().unwrap().contains("minimum value"));
    }

    /// Modo REAL: el margen disponible (perps_avail) bloquea el envío ANTES
    /// de la confirmación, el mínimo de $10 sigue vigente, y una límite con
    /// SL/TP se rechaza con instrucción clara (el SDK no los agrupa).
    #[test]
    fn panel_real_valida_margen_y_minimos() {
        let (mut app, _w, _u, _wc) = test_app();
        let mut trader_rx = arm(&mut app);
        pair_btc(&mut app);
        let master_fmt = app.trade.as_ref().unwrap().master_fmt.clone();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 50.0)));

        // $1000 a 5× = $200 de margen > $50 disponibles → ni confirma
        app.exec.size = "1000".into();
        app.exec.lev = 5;
        app.exec_submit();
        assert!(app.exec.confirm.is_none());
        assert!(app.exec.err.as_deref().unwrap().contains("margen insuficiente"));

        // por debajo de $10 notional: rechazo del draft (regla real)
        app.exec.size = "5".into();
        app.exec_submit();
        assert!(app.exec.err.as_deref().unwrap().contains("$10"));

        // $200 a 5× = $40 ≤ $50 → confirma; `y` envía Open a mercado con SL/TP
        app.exec.size = "200".into();
        app.exec.sl = "2%".into();
        app.exec.tp = "5%".into();
        app.exec_submit();
        assert!(app.exec.confirm.is_some(), "err: {:?}", app.exec.err);
        app.view = View::Funds;
        press(&mut app, KeyCode::Char('y'));
        match trader_rx.try_recv().expect("debe salir Open") {
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
                assert_eq!(coin, "BTC");
                assert!(is_buy);
                assert_eq!(lev, 5);
                assert!(limit_px.is_none(), "mercado: sin precio límite");
                assert!((sz - 0.002).abs() < 1e-12);
                assert_eq!(sz_decimals, 5);
                assert_eq!(mid, 100_000.0);
                assert!((sl.unwrap() - 98_000.0).abs() < 1e-6);
                assert!((tp.unwrap() - 105_000.0).abs() < 1e-6);
            }
            other => panic!("comando inesperado: {other:?}"),
        }

        // límite + SL/TP: rechazo con instrucción (ponerlos tras el fill)
        app.exec.typ = exec::OrdType::Limit;
        app.exec.limit_px = "90000".into();
        app.exec_submit();
        assert!(app.exec.confirm.is_none());
        assert!(app.exec.err.as_deref().unwrap().contains("tras el fill"));

        // sin la restricción (SL/TP vacíos) la límite sí pasa a confirmación
        app.exec.sl.clear();
        app.exec.tp.clear();
        app.exec_submit();
        assert!(app.exec.confirm.is_some());
        press(&mut app, KeyCode::Char('y'));
        match trader_rx.try_recv().expect("debe salir Open límite") {
            TraderCmd::Open { limit_px, .. } => assert_eq!(limit_px, Some(90_000.0)),
            other => panic!("comando inesperado: {other:?}"),
        }

        // en maqueta (sin armar) nada de esto aplica: no hay validación de
        // margen — cubierto por los tests existentes de la maqueta
    }

    /// Paso 7.5 — fricción reforzada de MAINNET: con dinero real, ni `y` ni
    /// Enter ni el click en el botón ejecutan; solo teclear CONFIRMO + Enter.
    /// Esc sigue cancelando. En testnet/maqueta la frase no se activa.
    #[test]
    fn mainnet_exige_frase_confirmo() {
        let (mut app, _w, _u, _wc) = test_app();
        let mut trader_rx = arm(&mut app);
        app.net_label = "mainnet";
        pair_btc(&mut app);
        let master_fmt = app.trade.as_ref().unwrap().master_fmt.clone();
        app.apply_msg(DataMsg::WalletState(snap(&master_fmt, 50.0)));

        app.exec.size = "200".into();
        app.exec.lev = 5;
        app.exec_submit();
        assert!(app.exec.confirm.is_some(), "err: {:?}", app.exec.err);
        assert!(app.exec.confirm_phrase.is_some(), "mainnet activa la frase");
        app.view = View::Funds;

        // `y`, Enter con la frase incompleta y el click del botón NO ejecutan
        press(&mut app, KeyCode::Char('y'));
        press(&mut app, KeyCode::Enter);
        app.exec_confirm_yes(); // camino del click en ConfirmYes
        assert!(trader_rx.try_recv().is_err(), "no debe salir ningún comando");
        assert!(app.exec.confirm.is_some(), "el modal sigue abierto");

        // teclear la frase (minúsculas: se normaliza) + Enter → Open real.
        // La `y` de antes quedó en el buffer: limpiar con Backspace primero.
        press(&mut app, KeyCode::Backspace);
        for ch in "confirmo".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            trader_rx.try_recv(),
            Ok(TraderCmd::Open { .. })
        ));
        assert!(app.exec.confirm.is_none());
        assert!(app.exec.confirm_phrase.is_none());

        // Esc cancela sin ejecutar
        app.exec_submit();
        assert!(app.exec.confirm.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(app.exec.confirm.is_none());
        assert!(app.exec.confirm_phrase.is_none());
        assert!(trader_rx.try_recv().is_err());
    }
}
