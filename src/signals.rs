//! Señales puras: clasificación de flujo OI/precio y TA de confirmación (RSI, ADX/DMI).
//! Prioridad del proyecto: datos nativos de Hyperliquid primero; RSI/ADX solo confirmación.

/// Régimen de flujo según el delta de OI vs. el delta de precio en una ventana.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Regime {
    /// OI sube + precio sube: entran longs agresivos.
    LongBuild,
    /// OI sube + precio baja: entran shorts agresivos.
    ShortBuild,
    /// OI baja + precio sube: shorts cerrando (short squeeze/covering).
    ShortCover,
    /// OI baja + precio baja: longs cerrando (capitulación/toma de beneficio).
    LongUnwind,
    /// Sin movimiento significativo.
    Flat,
}

impl Regime {
    pub fn label(&self) -> &'static str {
        match self {
            Regime::LongBuild => crate::i18n::t().reg_long_build,
            Regime::ShortBuild => crate::i18n::t().reg_short_build,
            Regime::ShortCover => crate::i18n::t().reg_short_cover,
            Regime::LongUnwind => crate::i18n::t().reg_long_unwind,
            Regime::Flat => "—",
        }
    }
}

/// Deadbands: por debajo de esto el movimiento se considera ruido.
const PX_MIN_PCT: f64 = 0.05;
const OI_MIN_PCT: f64 = 0.10;

pub fn classify(px_delta_pct: f64, oi_delta_pct: f64) -> Regime {
    if px_delta_pct.abs() < PX_MIN_PCT || oi_delta_pct.abs() < OI_MIN_PCT {
        return Regime::Flat;
    }
    match (oi_delta_pct > 0.0, px_delta_pct > 0.0) {
        (true, true) => Regime::LongBuild,
        (true, false) => Regime::ShortBuild,
        (false, true) => Regime::ShortCover,
        (false, false) => Regime::LongUnwind,
    }
}

/// RSI de Wilder como serie alineada 1:1 con `closes`; NaN durante el warmup.
/// Misma semántica que Pine: rma de subidas/bajadas con seed SMA.
pub fn rsi_series(closes: &[f64], period: usize) -> Vec<f64> {
    let n = closes.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period + 1 {
        return out;
    }
    let p = period as f64;
    let (mut avg_g, mut avg_l) = (0.0, 0.0);
    for i in 1..n {
        let d = closes[i] - closes[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        if i <= period {
            avg_g += g;
            avg_l += l;
            if i == period {
                avg_g /= p;
                avg_l /= p;
            }
        } else {
            avg_g = (avg_g * (p - 1.0) + g) / p;
            avg_l = (avg_l * (p - 1.0) + l) / p;
        }
        if i >= period {
            out[i] = if avg_l == 0.0 {
                100.0
            } else {
                100.0 - 100.0 / (1.0 + avg_g / avg_l)
            };
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
pub struct Dmi {
    pub adx: f64,
    pub plus_di: f64,
    pub minus_di: f64,
}

/// ADX/DMI como serie alineada 1:1 con las velas; campos NaN durante su warmup
/// (los DI están disponibles antes que el ADX, como en Pine). `di_len` suaviza
/// TR y ±DM; `adx_len` suaviza el DX.
pub fn dmi_series(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    di_len: usize,
    adx_len: usize,
) -> Vec<Dmi> {
    let n = highs.len();
    let nan = Dmi {
        adx: f64::NAN,
        plus_di: f64::NAN,
        minus_di: f64::NAN,
    };
    let mut out = vec![nan; n];
    if di_len == 0 || adx_len == 0 || n != lows.len() || n != closes.len() || n < di_len + 1 {
        return out;
    }
    let p = di_len as f64;
    let pa = adx_len as f64;
    let (mut s_tr, mut s_pdm, mut s_mdm) = (0.0f64, 0.0f64, 0.0f64);
    let (mut dx_sum, mut dx_count) = (0.0f64, 0usize);
    let mut adx = f64::NAN;
    for i in 1..n {
        let up = highs[i] - highs[i - 1];
        let down = lows[i - 1] - lows[i];
        let pdm = if up > down && up > 0.0 { up } else { 0.0 };
        let mdm = if down > up && down > 0.0 { down } else { 0.0 };
        let tr = (highs[i] - lows[i])
            .max((highs[i] - closes[i - 1]).abs())
            .max((lows[i] - closes[i - 1]).abs());
        if i <= di_len {
            s_tr += tr;
            s_pdm += pdm;
            s_mdm += mdm;
            if i < di_len {
                continue;
            }
        } else {
            s_tr = s_tr - s_tr / p + tr;
            s_pdm = s_pdm - s_pdm / p + pdm;
            s_mdm = s_mdm - s_mdm / p + mdm;
        }
        let (pdi, mdi) = if s_tr <= 0.0 {
            (0.0, 0.0)
        } else {
            (100.0 * s_pdm / s_tr, 100.0 * s_mdm / s_tr)
        };
        let dx = if pdi + mdi == 0.0 {
            0.0
        } else {
            100.0 * (pdi - mdi).abs() / (pdi + mdi)
        };
        if dx_count < adx_len {
            dx_sum += dx;
            dx_count += 1;
            if dx_count == adx_len {
                adx = dx_sum / pa;
            }
        } else {
            adx = (adx * (pa - 1.0) + dx) / pa;
        }
        out[i] = Dmi {
            adx,
            plus_di: pdi,
            minus_di: mdi,
        };
    }
    out
}

/// Aplica `f` a cada ventana completa de `period` valores; NaN mientras la
/// ventana no esté llena o contenga NaN (igual que Pine con fuentes aún na).
fn windowed(values: &[f64], period: usize, f: impl Fn(&[f64]) -> f64) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period {
        return out;
    }
    for i in (period - 1)..n {
        let w = &values[i + 1 - period..=i];
        if w.iter().all(|v| v.is_finite()) {
            out[i] = f(w);
        }
    }
    out
}

/// SMA por ventana completa (serie).
pub fn sma_series(values: &[f64], period: usize) -> Vec<f64> {
    windowed(values, period, |w| w.iter().sum::<f64>() / w.len() as f64)
}

/// Desviación estándar poblacional por ventana (como `ta.stdev` por defecto).
pub fn stdev_series(values: &[f64], period: usize) -> Vec<f64> {
    windowed(values, period, |w| {
        let m = w.iter().sum::<f64>() / w.len() as f64;
        (w.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / w.len() as f64).sqrt()
    })
}

/// EMA/RMA según `alpha`, con seed SMA sobre la primera ventana completa de
/// valores finitos (así arranca limpia sobre series con warmup NaN, como el RSI).
fn smoothed(values: &[f64], period: usize, alpha: f64) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 {
        return out;
    }
    let mut prev: Option<f64> = None;
    let mut run = 0usize;
    for i in 0..n {
        if !values[i].is_finite() {
            run = 0;
            prev = None;
            continue;
        }
        run += 1;
        if run < period {
            continue;
        }
        let v = match prev {
            Some(p) => alpha * values[i] + (1.0 - alpha) * p,
            None => values[i + 1 - period..=i].iter().sum::<f64>() / period as f64,
        };
        out[i] = v;
        prev = Some(v);
    }
    out
}

/// Tipos de MA del selector del Pine para la media del RSI. Solo Sma se
/// construye con los defaults; el resto es superficie configurable del port.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaKind {
    Sma,
    /// SMA + banda de Bollinger sobre el propio RSI.
    Bollinger,
    Ema,
    Rma,
    Wma,
    Vwma,
}

/// MA genérica sobre una serie; `volumes` solo se usa con Vwma.
pub fn ma_series(values: &[f64], period: usize, kind: MaKind, volumes: &[f64]) -> Vec<f64> {
    match kind {
        MaKind::Sma | MaKind::Bollinger => sma_series(values, period),
        MaKind::Ema => smoothed(values, period, 2.0 / (period as f64 + 1.0)),
        MaKind::Rma => smoothed(values, period, 1.0 / period as f64),
        MaKind::Wma => windowed(values, period, |w| {
            let den = (w.len() * (w.len() + 1)) as f64 / 2.0;
            w.iter()
                .enumerate()
                .map(|(i, v)| v * (i + 1) as f64)
                .sum::<f64>()
                / den
        }),
        MaKind::Vwma => {
            if volumes.len() != values.len() {
                return vec![f64::NAN; values.len()];
            }
            let vw: Vec<f64> = values.iter().zip(volumes).map(|(v, w)| v * w).collect();
            sma_series(&vw, period)
                .into_iter()
                .zip(sma_series(volumes, period))
                .map(|(a, b)| if b != 0.0 { a / b } else { f64::NAN })
                .collect()
        }
    }
}

// ═══ Panel Ballenas + RSI/ADX/DMI — port de reference/whales_rsi_adx_dmi.pine (v6) ═══
// TA puro sobre precio (Bollinger + RSI + DMI), sin OI ni datos on-chain: distinto
// del whale positioning por leaderboard. Busca reversión con ADX bajo, no continuación.

/// Parámetros del panel, espejo de los inputs del Pine (defaults idénticos).
#[derive(Debug, Clone)]
pub struct WhaleParams {
    pub rsi_len: usize,
    pub ma_kind: MaKind,
    pub ma_len: usize,
    /// Desviación de la BB sobre el RSI (solo con MaKind::Bollinger).
    pub rsi_bb_mult: f64,
    pub overbought: f64,
    pub oversold: f64,
    /// RSI Modificado (%B): Bollinger de precio propio, independiente del de ballenas.
    pub mod_len: usize,
    pub mod_mult: f64,
    pub di_len: usize,
    pub adx_len: usize,
    /// Bollinger de PRECIO de las condiciones de ballena.
    pub bb_len: usize,
    pub bb_mult: f64,
    /// Escala visual de la intensidad y tope de altura de columna (panel 0-100).
    pub whale_scale: f64,
    pub whale_cap: f64,
    // umbrales long (ballena comprando)
    pub rsi_max_long: f64,
    pub adx_max_long: f64,
    pub pdi_max_long: f64,
    pub mdi_min_long: f64,
    // umbrales short (ballena vendiendo)
    pub rsi_min_short: f64,
    pub adx_max_short: f64,
    pub pdi_min_short: f64,
    pub mdi_max_short: f64,
}

impl Default for WhaleParams {
    fn default() -> Self {
        Self {
            rsi_len: 14,
            ma_kind: MaKind::Sma,
            ma_len: 14,
            rsi_bb_mult: 2.0,
            overbought: 70.0,
            oversold: 30.0,
            mod_len: 20,
            mod_mult: 2.0,
            di_len: 14,
            adx_len: 14,
            bb_len: 20,
            bb_mult: 2.0,
            whale_scale: 10.0,
            whale_cap: 95.0,
            rsi_max_long: 40.0,
            adx_max_long: 28.0,
            pdi_max_long: 20.0,
            mdi_min_long: 23.0,
            rsi_min_short: 60.0,
            adx_max_short: 28.0,
            pdi_min_short: 23.0,
            mdi_max_short: 20.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhaleSide {
    Buy,
    Sell,
}

/// Disparo de ballena en una vela concreta.
#[derive(Debug, Clone, Copy)]
pub struct WhaleTrigger {
    /// Índice de la vela en las series de entrada.
    pub idx: usize,
    pub side: WhaleSide,
    /// % del cierre fuera de la banda (0 si cerró de vuelta dentro).
    pub dist_pct: f64,
    /// Altura de columna en el panel 0-100: max(dist, 0.1) × escala, capada.
    pub height: f64,
}

/// Series del panel whales+RSI, alineadas 1:1 con las velas (NaN = warmup).
pub struct WhalePanel {
    pub rsi: Vec<f64>,
    pub rsi_ma: Vec<f64>,
    /// (superior, inferior) de la BB sobre el RSI, solo con MaKind::Bollinger.
    pub rsi_bb: Option<(Vec<f64>, Vec<f64>)>,
    /// RSI Modificado: %B del Bollinger de precio reescalado a ~25-75.
    pub mod_rsi: Vec<f64>,
    pub dmi: Vec<Dmi>,
    /// Bollinger de PRECIO que evalúan las condiciones de ballena.
    pub bb_upper: Vec<f64>,
    pub bb_lower: Vec<f64>,
    /// Disparos en orden cronológico (idx ascendente).
    pub triggers: Vec<WhaleTrigger>,
}

impl WhalePanel {
    pub fn last_rsi(&self) -> Option<f64> {
        self.rsi.last().copied().filter(|v| v.is_finite())
    }

    pub fn last_dmi(&self) -> Option<Dmi> {
        self.dmi.last().copied().filter(|d| d.adx.is_finite())
    }
}

/// Los 5 filtros del Pine por lado (long y short son excluyentes: RSI<40 y
/// RSI>60 no coexisten). Devuelve el lado y la distancia % fuera de la banda.
#[allow(clippy::too_many_arguments)] // espejo literal de los inputs del Pine
fn whale_condition(
    high: f64,
    low: f64,
    close: f64,
    bb_upper: f64,
    bb_lower: f64,
    rsi: f64,
    d: &Dmi,
    p: &WhaleParams,
) -> Option<(WhaleSide, f64)> {
    if low <= bb_lower
        && rsi < p.rsi_max_long
        && d.plus_di < p.pdi_max_long
        && d.minus_di > p.mdi_min_long
        && d.adx < p.adx_max_long
    {
        let dist = (bb_lower - close).max(0.0) / bb_lower * 100.0;
        return Some((WhaleSide::Buy, dist));
    }
    if high >= bb_upper
        && rsi > p.rsi_min_short
        && d.plus_di > p.pdi_min_short
        && d.minus_di < p.mdi_max_short
        && d.adx < p.adx_max_short
    {
        let dist = (close - bb_upper).max(0.0) / bb_upper * 100.0;
        return Some((WhaleSide::Sell, dist));
    }
    None
}

/// Calcula todas las series del panel de una pasada. La confirmación RSI/ADX
/// de Vista 2 toma el último valor de estas mismas series — un único cálculo.
pub fn whale_panel(
    highs: &[f64],
    lows: &[f64],
    closes: &[f64],
    volumes: &[f64],
    p: &WhaleParams,
) -> WhalePanel {
    let n = closes.len();
    let rsi = rsi_series(closes, p.rsi_len);
    let rsi_ma = ma_series(&rsi, p.ma_len, p.ma_kind, volumes);
    let rsi_bb = (p.ma_kind == MaKind::Bollinger).then(|| {
        let sd = stdev_series(&rsi, p.ma_len);
        let up = rsi_ma
            .iter()
            .zip(&sd)
            .map(|(m, s)| m + p.rsi_bb_mult * s)
            .collect();
        let lo = rsi_ma
            .iter()
            .zip(&sd)
            .map(|(m, s)| m - p.rsi_bb_mult * s)
            .collect();
        (up, lo)
    });

    let basis = sma_series(closes, p.mod_len);
    let dev = stdev_series(closes, p.mod_len);
    let mod_rsi = (0..n)
        .map(|i| {
            let half = p.mod_mult * dev[i];
            let width = 2.0 * half;
            if width > 0.0 {
                // %B reescalado: b*0.5+25 → banda inferior=25, superior=75
                (closes[i] - (basis[i] - half)) / width * 100.0 * 0.5 + 25.0
            } else {
                f64::NAN
            }
        })
        .collect();

    let dmi = dmi_series(highs, lows, closes, p.di_len, p.adx_len);

    let bb_basis = sma_series(closes, p.bb_len);
    let bb_dev = stdev_series(closes, p.bb_len);
    let bb_upper: Vec<f64> = bb_basis
        .iter()
        .zip(&bb_dev)
        .map(|(b, d)| b + p.bb_mult * d)
        .collect();
    let bb_lower: Vec<f64> = bb_basis
        .iter()
        .zip(&bb_dev)
        .map(|(b, d)| b - p.bb_mult * d)
        .collect();

    let triggers = (0..n)
        .filter_map(|i| {
            if !(rsi[i].is_finite() && bb_upper[i].is_finite() && dmi[i].adx.is_finite()) {
                return None;
            }
            whale_condition(
                highs[i],
                lows[i],
                closes[i],
                bb_upper[i],
                bb_lower[i],
                rsi[i],
                &dmi[i],
                p,
            )
            .map(|(side, dist)| WhaleTrigger {
                idx: i,
                side,
                dist_pct: dist,
                height: (dist.max(0.1) * p.whale_scale).min(p.whale_cap),
            })
        })
        .collect();

    WhalePanel {
        rsi,
        rsi_ma,
        rsi_bb,
        mod_rsi,
        dmi,
        bb_upper,
        bb_lower,
        triggers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_matrix() {
        assert_eq!(classify(1.0, 1.0), Regime::LongBuild);
        assert_eq!(classify(-1.0, 1.0), Regime::ShortBuild);
        assert_eq!(classify(1.0, -1.0), Regime::ShortCover);
        assert_eq!(classify(-1.0, -1.0), Regime::LongUnwind);
        assert_eq!(classify(0.01, 5.0), Regime::Flat);
        assert_eq!(classify(5.0, 0.01), Regime::Flat);
    }

    #[test]
    fn rsi_extremes() {
        let up: Vec<f64> = (0..30).map(|i| 100.0 + i as f64).collect();
        assert_eq!(rsi_series(&up, 14)[29], 100.0);
        let down: Vec<f64> = (0..30).map(|i| 100.0 - i as f64).collect();
        assert!(rsi_series(&down, 14)[29] < 1.0);
        assert!(rsi_series(&up[..10], 14).iter().all(|v| v.is_nan()));
    }

    #[test]
    fn dmi_uptrend() {
        let n = 60;
        let highs: Vec<f64> = (0..n).map(|i| 101.0 + i as f64).collect();
        let lows: Vec<f64> = (0..n).map(|i| 99.0 + i as f64).collect();
        let closes: Vec<f64> = (0..n).map(|i| 100.5 + i as f64).collect();
        let d = dmi_series(&highs, &lows, &closes, 14, 14)[n - 1];
        assert!(d.plus_di > d.minus_di);
        assert!(d.adx > 25.0);
    }

    /// Serie sintética con vaivén para ejercitar los warmups.
    fn wavy(n: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let closes: Vec<f64> = (0..n)
            .map(|i| 100.0 + 8.0 * (i as f64 * 0.37).sin() + i as f64 * 0.05)
            .collect();
        let highs: Vec<f64> = closes
            .iter()
            .enumerate()
            .map(|(i, c)| c + 1.0 + 0.5 * (i as f64 * 0.7).sin().abs())
            .collect();
        let lows: Vec<f64> = closes
            .iter()
            .enumerate()
            .map(|(i, c)| c - 1.0 - 0.5 * (i as f64 * 0.5).cos().abs())
            .collect();
        (highs, lows, closes)
    }

    #[test]
    fn series_warmup_alignment() {
        let (highs, lows, closes) = wavy(80);
        let r = rsi_series(&closes, 14);
        assert!(r[..14].iter().all(|v| v.is_nan()));
        assert!(r[14..].iter().all(|v| v.is_finite()));
        let d = dmi_series(&highs, &lows, &closes, 14, 14);
        // DI disponible tras di_len deltas; ADX tras adx_len valores de DX más
        assert!(d[..14].iter().all(|x| x.plus_di.is_nan()));
        assert!(d[14..].iter().all(|x| x.plus_di.is_finite()));
        assert!(d[..27].iter().all(|x| x.adx.is_nan()));
        assert!(d[27..].iter().all(|x| x.adx.is_finite()));
        // la confirmación de Vista 2 lee el último valor de estas series
        let vols = vec![1.0; 80];
        let panel = whale_panel(&highs, &lows, &closes, &vols, &WhaleParams::default());
        assert_eq!(panel.last_rsi(), Some(r[79]));
        assert_eq!(panel.last_dmi().unwrap().adx, d[79].adx);
    }

    #[test]
    fn sma_stdev_windows() {
        let v = [1.0, 2.0, 3.0, 4.0];
        let s = sma_series(&v, 2);
        assert!(s[0].is_nan());
        assert_eq!(&s[1..], &[1.5, 2.5, 3.5]);
        let sd = stdev_series(&v, 2);
        assert!(sd[0].is_nan());
        // poblacional (biased), como ta.stdev
        assert_eq!(&sd[1..], &[0.5, 0.5, 0.5]);
        // una ventana con NaN no produce valor
        let with_nan = [f64::NAN, 2.0, 3.0];
        let s2 = sma_series(&with_nan, 2);
        assert!(s2[1].is_nan());
        assert_eq!(s2[2], 2.5);
    }

    #[test]
    fn ma_kinds() {
        let v = [2.0, 2.0, 2.0, 2.0, 2.0];
        let vol = [1.0; 5];
        for kind in [
            MaKind::Sma,
            MaKind::Ema,
            MaKind::Rma,
            MaKind::Wma,
            MaKind::Vwma,
        ] {
            let m = ma_series(&v, 3, kind, &vol);
            assert!((m[4] - 2.0).abs() < 1e-12, "{kind:?}");
        }
        let w = ma_series(&[1.0, 2.0, 3.0], 3, MaKind::Wma, &[]);
        assert!((w[2] - 14.0 / 6.0).abs() < 1e-12);
    }

    #[test]
    fn mod_rsi_scaling() {
        // closes 1..=5, BB(5, 2.0): basis 3, sd √2 → %B=85.355, escalado ≈ 67.678
        let closes = [1.0, 2.0, 3.0, 4.0, 5.0];
        let p = WhaleParams {
            mod_len: 5,
            ..WhaleParams::default()
        };
        let panel = whale_panel(&closes, &closes, &closes, &closes, &p);
        assert!(panel.mod_rsi[3].is_nan());
        assert!((panel.mod_rsi[4] - 67.67767).abs() < 1e-4);
    }

    #[test]
    fn whale_condition_gates() {
        let p = WhaleParams::default();
        // long: low toca banda inferior, RSI<40, +DI<20, -DI>23, ADX<28
        let d = Dmi {
            adx: 20.0,
            plus_di: 15.0,
            minus_di: 25.0,
        };
        let long = whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 35.0, &d, &p);
        assert!(matches!(long, Some((WhaleSide::Buy, _))));
        // cada filtro roto apaga la señal
        assert!(whale_condition(105.0, 101.0, 103.0, 120.0, 100.0, 35.0, &d, &p).is_none());
        assert!(whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 45.0, &d, &p).is_none());
        let mut x = d;
        x.plus_di = 21.0;
        assert!(whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 35.0, &x, &p).is_none());
        x = d;
        x.minus_di = 22.0;
        assert!(whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 35.0, &x, &p).is_none());
        x = d;
        x.adx = 29.0;
        assert!(whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 35.0, &x, &p).is_none());
        // short espejo: high toca banda superior, RSI>60, +DI>23, -DI<20, ADX<28
        let ds = Dmi {
            adx: 20.0,
            plus_di: 25.0,
            minus_di: 15.0,
        };
        let short = whale_condition(121.0, 110.0, 118.0, 120.0, 100.0, 65.0, &ds, &p);
        assert!(matches!(short, Some((WhaleSide::Sell, _))));
        // distancia: cierre 5% bajo la banda inferior
        let (_, dist) = whale_condition(105.0, 94.0, 95.0, 120.0, 100.0, 35.0, &d, &p).unwrap();
        assert!((dist - 5.0).abs() < 1e-12);
        // cierre de vuelta dentro de la banda → dist 0
        let (_, dist0) = whale_condition(105.0, 99.0, 103.0, 120.0, 100.0, 35.0, &d, &p).unwrap();
        assert_eq!(dist0, 0.0);
    }

    #[test]
    fn whale_intensity_min_and_cap() {
        let p = WhaleParams::default();
        // dist 0 → altura mínima 0.1×escala = 1; dist 20% → 200, capada a 95
        assert_eq!((0.0f64.max(0.1) * p.whale_scale).min(p.whale_cap), 1.0);
        assert_eq!((20.0f64.max(0.1) * p.whale_scale).min(p.whale_cap), 95.0);
    }
}
