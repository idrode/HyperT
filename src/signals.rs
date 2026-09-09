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

/// Periodo por defecto del TRIX — el del Pine Script fuente (18, no 15).
pub const TRIX_PERIOD: usize = 18;

/// TRIX, fórmula EXACTA del Pine fuente (v6):
/// `10000 * ta.change(ta.ema(ta.ema(ta.ema(math.log(close), len), len), len))`
/// — triple EMA sobre el LOGARITMO NATURAL del cierre (no el precio crudo), y
/// `ta.change` con longitud 1 = diferencia simple (no porcentual: el log ya
/// aporta el efecto porcentual), escalada ×10000. Cada EMA arranca con seed
/// SMA sobre su primera ventana completa (semántica de `smoothed`, la misma
/// del RSI/MA ya portados). NaN durante el warmup. Oscila alrededor de 0:
/// positivo = momentum alcista.
pub fn trix_series(closes: &[f64], period: usize) -> Vec<f64> {
    let alpha = 2.0 / (period as f64 + 1.0);
    let logc: Vec<f64> = closes
        .iter()
        .map(|c| if *c > 0.0 { c.ln() } else { f64::NAN })
        .collect();
    let e1 = smoothed(&logc, period, alpha);
    let e2 = smoothed(&e1, period, alpha);
    let e3 = smoothed(&e2, period, alpha);
    let n = closes.len();
    let mut out = vec![f64::NAN; n];
    for i in 1..n {
        if e3[i].is_finite() && e3[i - 1].is_finite() {
            out[i] = 10000.0 * (e3[i] - e3[i - 1]);
        }
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

// ── Divergencia precio/RSI ──────────────────────────────────────────────────
//
// Pieza pedida por la especificación del consenso ponderado (Vista 6): señal
// INDEPENDIENTE de la divergencia CVD/precio que ya vive en `flow.rs` — aquella
// compara volumen acumulado contra precio, esta compara el RSI contra precio.
// Ambas deben poder coexistir en el score.
//
// Método clásico (el mismo del indicador de divergencias de TradingView): se
// buscan pivotes en el OSCILADOR, no en el precio, y el precio se lee en esas
// mismas velas. Un pivote solo se confirma cuando han cerrado `right` velas
// después, así que las últimas `right` velas nunca pueden tener un pivote —
// honestidad de datos: no se adivina un pivote a medio formar.

/// Velas a cada lado que debe superar un pivote para confirmarse.
pub const DIV_LEFT: usize = 5;
pub const DIV_RIGHT: usize = 5;
/// Separación admitida entre los dos pivotes comparados, en velas. Demasiado
/// juntos = ruido; demasiado lejos = ya no es la misma estructura de mercado.
pub const DIV_MIN_SPAN: usize = 5;
pub const DIV_MAX_SPAN: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DivKind {
    /// Precio hace mínimo más BAJO y el RSI mínimo más ALTO → sesgo alcista.
    Bullish,
    /// Precio hace máximo más ALTO y el RSI máximo más BAJO → sesgo bajista.
    Bearish,
}

/// Divergencia confirmada entre dos pivotes del RSI (índices en las series de
/// entrada, `from` < `to`). Los valores son los del RSI en esos pivotes: son
/// los que traza la línea del panel del oscilador.
#[derive(Debug, Clone, Copy)]
pub struct Divergence {
    pub from: usize,
    pub to: usize,
    pub from_rsi: f64,
    pub to_rsi: f64,
    pub kind: DivKind,
}

/// Índices de pivotes de una serie: máximos locales si `high`, mínimos si no.
/// Un pivote i debe ser estrictamente mejor que las `left` velas anteriores y
/// al menos tan bueno como las `right` posteriores (empates a la derecha no
/// invalidan, criterio de Pine `ta.pivothigh`/`ta.pivotlow`). NaN nunca es
/// pivote ni participa en la comparación: un warmup a medias no inventa uno.
pub fn pivots(vals: &[f64], left: usize, right: usize, high: bool) -> Vec<usize> {
    let mut out = Vec::new();
    if vals.len() <= left + right {
        return out;
    }
    let better = |a: f64, b: f64| if high { a > b } else { a < b };
    let not_worse = |a: f64, b: f64| if high { a >= b } else { a <= b };
    for i in left..vals.len() - right {
        let v = vals[i];
        if !v.is_finite() {
            continue;
        }
        let l_ok = (i - left..i).all(|j| vals[j].is_finite() && better(v, vals[j]));
        let r_ok = (i + 1..=i + right).all(|j| vals[j].is_finite() && not_worse(v, vals[j]));
        if l_ok && r_ok {
            out.push(i);
        }
    }
    out
}

/// Divergencias precio/RSI sobre pivotes CONSECUTIVOS del RSI, separados entre
/// `DIV_MIN_SPAN` y `DIV_MAX_SPAN` velas. `closes` y `rsi` deben estar
/// alineados 1:1 (misma longitud); si no lo están, devuelve vacío en vez de
/// comparar velas desalineadas.
pub fn rsi_divergences(closes: &[f64], rsi: &[f64]) -> Vec<Divergence> {
    let mut out = Vec::new();
    if closes.len() != rsi.len() {
        return out;
    }
    for (high, kind) in [(false, DivKind::Bullish), (true, DivKind::Bearish)] {
        let pv = pivots(rsi, DIV_LEFT, DIV_RIGHT, high);
        for w in pv.windows(2) {
            let (a, b) = (w[0], w[1]);
            let span = b - a;
            if !(DIV_MIN_SPAN..=DIV_MAX_SPAN).contains(&span) {
                continue;
            }
            let (pa, pb) = (closes[a], closes[b]);
            if !pa.is_finite() || !pb.is_finite() {
                continue;
            }
            let hit = match kind {
                // precio mínimo más bajo, RSI mínimo más alto
                DivKind::Bullish => pb < pa && rsi[b] > rsi[a],
                // precio máximo más alto, RSI máximo más bajo
                DivKind::Bearish => pb > pa && rsi[b] < rsi[a],
            };
            if hit {
                out.push(Divergence {
                    from: a,
                    to: b,
                    from_rsi: rsi[a],
                    to_rsi: rsi[b],
                    kind,
                });
            }
        }
    }
    out.sort_by_key(|d| d.to);
    out
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

    /// Caso numérico CONOCIDO en forma cerrada: si el cierre crece
    /// exponencialmente (`close = 100·e^(0.01·i)`), `log(close)` es una recta
    /// de pendiente 0.01. Una EMA con seed SMA sobre una recta es EXACTAMENTE
    /// la recta desplazada por su lag ((p−1)/2 · pendiente): el seed cae en el
    /// punto medio de la ventana y la recursión preserva el desfase (se
    /// comprueba algebraicamente: (1−α)/α = (p−1)/2). El triple EMA sigue
    /// siendo una recta con pendiente 0.01, así que la diferencia simple por
    /// vela es 0.01 y TRIX = 10000 × 0.01 = 100 exacto — verificable a mano
    /// sin depender de una implementación de referencia.
    #[test]
    fn trix_recta_exponencial_valor_exacto() {
        let closes: Vec<f64> = (0..120).map(|i| 100.0 * (0.01 * i as f64).exp()).collect();
        let t = trix_series(&closes, 18);
        for v in &t[60..] {
            assert!((v - 100.0).abs() < 1e-8, "TRIX debería ser 100 exacto: {v}");
        }
        // precio constante → log constante → triple EMA constante → TRIX 0
        let flat = vec![250.0; 120];
        let tf = trix_series(&flat, 18);
        assert!(tf[119].abs() < 1e-12);
    }

    /// Caso pequeño calculado A MANO paso a paso (periodo 2, α=2/3, seed SMA):
    /// closes [1,2,3,4,5] → log [0, .693147, 1.098612, 1.386294, 1.609438].
    /// e1: seed(1)=.346574, e1(2)=.847298, e1(3)=1.206629, e1(4)=1.475168.
    /// e2: seed(2)=.596936, e2(3)=1.003398, e2(4)=1.317912.
    /// e3: seed(3)=.800167, e3(4)=1.145330.
    /// TRIX(4) = 10000·(e3(4)−e3(3)) = 3450.6121…
    #[test]
    fn trix_caso_a_mano() {
        let closes = [1.0, 2.0, 3.0, 4.0, 5.0];
        let t = trix_series(&closes, 2);
        assert!(t[..4].iter().all(|v| v.is_nan()), "warmup honesto");
        let ln = |x: f64| x.ln();
        let a = 2.0 / 3.0;
        let e1_1 = (ln(1.0) + ln(2.0)) / 2.0;
        let e1_2 = a * ln(3.0) + (1.0 - a) * e1_1;
        let e1_3 = a * ln(4.0) + (1.0 - a) * e1_2;
        let e1_4 = a * ln(5.0) + (1.0 - a) * e1_3;
        let e2_2 = (e1_1 + e1_2) / 2.0;
        let e2_3 = a * e1_3 + (1.0 - a) * e2_2;
        let e2_4 = a * e1_4 + (1.0 - a) * e2_3;
        let e3_3 = (e2_2 + e2_3) / 2.0;
        let e3_4 = a * e2_4 + (1.0 - a) * e3_3;
        let want = 10000.0 * (e3_4 - e3_3);
        assert!((t[4] - want).abs() < 1e-9);
        assert!((want - 3450.6121).abs() < 1e-3, "referencia a mano: {want}");
    }

    /// Warmup del TRIX(18): 3 EMAs encadenadas con seed SMA + 1 vela de la
    /// diferencia → primer valor finito en el índice 3·(p−1)+1.
    #[test]
    fn trix_warmup_alignment() {
        let closes: Vec<f64> = (0..120).map(|i| 100.0 + (i as f64 * 0.3).sin()).collect();
        let t = trix_series(&closes, 18);
        let first = 3 * 17 + 1;
        assert!(t[..first].iter().all(|v| v.is_nan()));
        assert!(t[first..].iter().all(|v| v.is_finite()));
        // precios no positivos no revientan: NaN honesto
        assert!(trix_series(&[0.0; 40], 18).iter().all(|v| v.is_nan()));
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

#[cfg(test)]
mod div_tests {
    use super::*;

    /// Serie con un mínimo claro en el índice 6 y nada más: solo ese índice
    /// puede ser pivote (los 5 primeros y los 5 últimos nunca se confirman).
    #[test]
    fn pivote_bajo_confirmado_solo_con_ambos_lados() {
        let v = [9.0, 8.0, 7.0, 6.0, 5.0, 4.0, 1.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        assert_eq!(pivots(&v, 5, 5, false), vec![6]);
        // la misma serie no tiene ningún máximo local confirmable
        assert!(pivots(&v, 5, 5, true).is_empty());
    }

    #[test]
    fn pivote_sin_velas_a_la_derecha_no_se_confirma() {
        // mínimo en el último índice: faltan las 5 velas de confirmación
        let v = [9.0, 8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0];
        assert!(pivots(&v, 5, 5, false).is_empty());
    }

    #[test]
    fn nan_de_warmup_nunca_es_pivote() {
        let mut v = vec![f64::NAN; 6];
        v.extend([1.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        // el índice 6 vale 1.0 pero sus 5 velas izquierdas son NaN
        assert!(pivots(&v, 5, 5, false).is_empty());
    }

    /// Caso construido a mano: dos valles de RSI (idx 6 y 18). El precio hace
    /// mínimo MÁS BAJO en el segundo, el RSI mínimo MÁS ALTO → alcista.
    #[test]
    fn divergencia_alcista_precio_baja_rsi_sube() {
        let mut rsi = vec![50.0; 25];
        let mut closes = vec![100.0; 25];
        for (i, v) in [(6usize, 20.0), (18, 30.0)] {
            rsi[i] = v;
        }
        closes[6] = 90.0;
        closes[18] = 85.0;
        let d = rsi_divergences(&closes, &rsi);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DivKind::Bullish);
        assert_eq!((d[0].from, d[0].to), (6, 18));
        assert_eq!((d[0].from_rsi, d[0].to_rsi), (20.0, 30.0));
    }

    #[test]
    fn divergencia_bajista_precio_sube_rsi_baja() {
        let mut rsi = vec![50.0; 25];
        let mut closes = vec![100.0; 25];
        rsi[6] = 80.0;
        rsi[18] = 70.0;
        closes[6] = 110.0;
        closes[18] = 115.0;
        let d = rsi_divergences(&closes, &rsi);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].kind, DivKind::Bearish);
        assert_eq!((d[0].from, d[0].to), (6, 18));
    }

    #[test]
    fn confirmacion_no_es_divergencia() {
        // precio y RSI se mueven en el MISMO sentido: no hay divergencia
        let mut rsi = vec![50.0; 25];
        let mut closes = vec![100.0; 25];
        rsi[6] = 30.0;
        rsi[18] = 20.0;
        closes[6] = 90.0;
        closes[18] = 85.0;
        assert!(rsi_divergences(&closes, &rsi).is_empty());
    }

    #[test]
    fn pivotes_demasiado_lejos_no_se_comparan() {
        let n = DIV_MAX_SPAN + 30;
        let mut rsi = vec![50.0; n];
        let mut closes = vec![100.0; n];
        let (a, b) = (6, 6 + DIV_MAX_SPAN + 1);
        rsi[a] = 20.0;
        rsi[b] = 30.0;
        closes[a] = 90.0;
        closes[b] = 85.0;
        assert!(rsi_divergences(&closes, &rsi).is_empty());
    }

    #[test]
    fn series_desalineadas_no_producen_señal() {
        let rsi = vec![50.0; 25];
        let closes = vec![100.0; 24];
        assert!(rsi_divergences(&closes, &rsi).is_empty());
    }
}
