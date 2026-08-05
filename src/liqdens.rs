//! Densidad de liquidación por OI-delta — port de `reference/liq.pine`
//! (indicador "LqEMABB" de TradingView; solo la señal de liquidaciones, las
//! EMAs/Bollinger del final del script son extras ajenos a ella).
//!
//! Igual que el mapa de `src/liq.rs`, esto es una ESTIMACIÓN estadística, no
//! posiciones reales: velas con apertura neta de posiciones (ΔOI > 0 anómalo
//! frente a su SMA) proyectan niveles de liquidación hipotéticos a tiers de
//! apalancamiento fijos alrededor del ohlc4 de la vela.
//!
//! Mapeo clase → tiers, verificado contra el script literal (NO es simétrico):
//!   h3 (|ΔOI| ≥ 3.0×SMA):        5x ±20%, 10x ±10%, 25x ±4%, 50x ±2%, 100x ±1%
//!   h2 (2.0×SMA ≤ |ΔOI| < 3.0×): 10x ±10%, 25x ±4%, 50x ±2%, 100x ±1%
//!   h1 (1.2×SMA ≤ |ΔOI| < 2.0×): 25x ±4%, 50x ±2%, 100x ±1%
//! (cuanto mayor el spike de OI, más tiers de apalancamiento bajo se asumen
//! implicados; ambos lados del pivote siempre.)
//!
//! Desviaciones deliberadas respecto al Pine, documentadas:
//! - El ΔOI de la primera vela se omite (Pine usa `nz(OI[1])`, que en el
//!   primer bar del chart daría ΔOI = OI entero; en TradingView ese artefacto
//!   queda miles de barras atrás, aquí caería dentro de la ventana).
//! - El rango del histograma usa las últimas min(120, n) velas; el Pine con
//!   menos de 120 barras de chart devolvería na y no pintaría nada.
//!
//! Se conserva a propósito el `>=`/`<=` del binning original: un nivel que cae
//! exactamente en la frontera entre dos bins cuenta en ambos.

/// Ventana de la SMA del |ΔOI| (input "MA Length" del script).
pub const MA_LENGTH: usize = 60;
/// Nº mínimo de velas con OI para clasificar: 60 deltas + la vela inicial.
pub const MIN_BARS: usize = MA_LENGTH + 1;
/// Umbrales de clasificación (inputs h1/h2/h3 del script).
const H1: f64 = 1.2;
const H2: f64 = 2.0;
const H3: f64 = 3.0;
/// Velas hacia atrás para el rango del histograma ("Number of bars to lookback").
const LOOKBACK: usize = 120;
/// Nº de bins del histograma ("Number of histograms").
pub const N_BINS: usize = 120;
/// Cap FIFO de niveles por clase (numOfLines del script, vía f_append).
const MAX_LINES_PER_CLASS: usize = 500;

/// Distancia al pivote por tier = 1/leverage. Ver mapeo en el doc del módulo.
const TIERS_H3: &[f64] = &[0.20, 0.10, 0.04, 0.02, 0.01];
const TIERS_H2: &[f64] = &[0.10, 0.04, 0.02, 0.01];
const TIERS_H1: &[f64] = &[0.04, 0.02, 0.01];

/// Vela con su open interest al cierre, en orden cronológico.
#[derive(Debug, Clone, Copy)]
pub struct LiqBar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// OI al cierre de la vela, en unidades base.
    pub oi: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct DensBin {
    pub low: f64,
    pub high: f64,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct Density {
    /// Histograma de N_BINS entre range_low y range_high.
    pub bins: Vec<DensBin>,
    /// Precios de los niveles vivos (equivale al histogramData del script;
    /// incluye niveles fuera del rango del histograma, que no se binean).
    pub alive_levels: Vec<f64>,
    /// Velas que clasificaron h1/h2/h3 (con nivel proyectado).
    pub classified: usize,
    pub range_low: f64,
    pub range_high: f64,
}

/// Calcula la densidad de liquidación sobre velas con OI al cierre.
/// None si no hay velas suficientes para la SMA o los precios son degenerados.
pub fn density(bars: &[LiqBar]) -> Option<Density> {
    let n = bars.len();
    if n < MIN_BARS {
        return None;
    }

    // prefix[k] = Σ |ΔOI| de las velas 1..=k (el delta de la vela 0 no existe)
    let mut prefix = vec![0.0_f64; n];
    for i in 1..n {
        prefix[i] = prefix[i - 1] + (bars[i].oi - bars[i - 1].oi).abs();
    }

    // niveles proyectados por clase: (vela de nacimiento, precio)
    let mut lines: [std::collections::VecDeque<(usize, f64)>; 3] = Default::default();
    let mut classified = 0usize;
    for i in MA_LENGTH..n {
        let d = bars[i].oi - bars[i - 1].oi;
        if d <= 0.0 {
            continue;
        }
        // SMA incluye la vela actual, como ta.sma en Pine
        let ma = (prefix[i] - prefix[i - MA_LENGTH]) / MA_LENGTH as f64;
        let tiers = if d >= ma * H3 {
            TIERS_H3
        } else if d >= ma * H2 {
            TIERS_H2
        } else if d >= ma * H1 {
            TIERS_H1
        } else {
            continue;
        };
        classified += 1;
        let arr = &mut lines[tiers.len() - 3];
        let b = &bars[i];
        let pivot = (b.open + b.close + b.high + b.low) / 4.0;
        // orden de inserción del script: por tier, primero bajo y luego sobre
        // el pivote — relevante solo para el cap FIFO
        for pct in tiers {
            for y in [pivot * (1.0 - pct), pivot * (1.0 + pct)] {
                if arr.len() == MAX_LINES_PER_CLASS {
                    arr.pop_front();
                }
                arr.push_back((i, y));
            }
        }
    }

    // rango del histograma sobre las últimas LOOKBACK velas
    let tail = &bars[n.saturating_sub(LOOKBACK)..];
    let local_high = tail
        .iter()
        .map(|b| b.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let local_low = tail.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    if !(local_low > 0.0 && local_high.is_finite()) {
        return None;
    }
    let ratio = local_high / local_low;
    let range_high = local_high * (1.0 + ratio / 10.0);
    let range_low = local_low * (1.0 - ratio / 10.0);
    let height = (range_high - range_low) / N_BINS as f64;

    let mut bins: Vec<DensBin> = (0..N_BINS)
        .map(|j| DensBin {
            low: range_low + height * j as f64,
            high: range_low + height * (j + 1) as f64,
            count: 0,
        })
        .collect();

    // un nivel vive mientras ninguna vela desde su nacimiento (incluida la
    // que lo crea) lo cruce estrictamente — high > y > low
    let mut alive_levels = Vec::new();
    for arr in &lines {
        for &(birth, y) in arr {
            if bars[birth..].iter().any(|b| b.high > y && b.low < y) {
                continue;
            }
            alive_levels.push(y);
            for bin in &mut bins {
                if y >= bin.low && y <= bin.high {
                    bin.count += 1;
                }
            }
        }
    }

    Some(Density {
        bins,
        alive_levels,
        classified,
        range_low,
        range_high,
    })
}

/// Bins con densidad, ordenados por densidad desc (desempate: más cerca del
/// mark), para la salida "top N niveles" del panel.
pub fn top_bins(d: &Density, mark: f64, n: usize) -> Vec<DensBin> {
    let mut v: Vec<DensBin> = d.bins.iter().filter(|b| b.count > 0).copied().collect();
    v.sort_by(|a, b| {
        let da = ((a.low + a.high) / 2.0 - mark).abs();
        let db = ((b.low + b.high) / 2.0 - mark).abs();
        b.count
            .cmp(&a.count)
            .then_with(|| da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal))
    });
    v.truncate(n);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vela plana (sin rango) que nunca cruza niveles ajenos a su precio.
    fn flat(px: f64, oi: f64) -> LiqBar {
        LiqBar {
            open: px,
            high: px,
            low: px,
            close: px,
            oi,
        }
    }

    /// 61 velas planas a `px` con ΔOI alternando ±1 → SMA(|ΔOI|) ≈ 1.
    fn baseline(px: f64) -> Vec<LiqBar> {
        (0..=MA_LENGTH)
            .map(|i| flat(px, 100.0 + (i % 2) as f64))
            .collect()
    }

    fn has(levels: &[f64], v: f64) -> bool {
        levels.iter().any(|y| (y - v).abs() < 1e-9)
    }

    #[test]
    fn warmup_minimo() {
        let bars = baseline(100.0);
        assert!(density(&bars[..MA_LENGTH]).is_none());
        assert!(density(&bars).is_some());
    }

    #[test]
    fn h3_proyecta_cinco_tiers() {
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 5.0)); // ΔOI 5 ≥ 3×SMA(≈1.07) → h3
        let d = density(&bars).unwrap();
        assert_eq!(d.classified, 1);
        assert_eq!(d.alive_levels.len(), 10);
        for v in [
            80.0, 120.0, 90.0, 110.0, 96.0, 104.0, 98.0, 102.0, 99.0, 101.0,
        ] {
            assert!(has(&d.alive_levels, v), "falta nivel {v}");
        }
        // 80/120 quedan fuera del rango del histograma ([90,110] con velas
        // planas) pero sí cuentan como niveles vivos, como en el script
        assert!(d.range_low >= 89.9 && d.range_high <= 110.1);
    }

    #[test]
    fn h2_sin_5x_y_h1_sin_10x() {
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 2.2)); // 2×SMA ≤ 2.2 < 3×SMA → h2
        let d = density(&bars).unwrap();
        assert_eq!(d.alive_levels.len(), 8);
        assert!(!has(&d.alive_levels, 80.0) && !has(&d.alive_levels, 120.0));
        assert!(has(&d.alive_levels, 90.0) && has(&d.alive_levels, 110.0));

        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 1.5)); // 1.2×SMA ≤ 1.5 < 2×SMA → h1
        let d = density(&bars).unwrap();
        assert_eq!(d.alive_levels.len(), 6);
        assert!(!has(&d.alive_levels, 90.0) && !has(&d.alive_levels, 110.0));
        assert!(has(&d.alive_levels, 96.0) && has(&d.alive_levels, 104.0));
    }

    #[test]
    fn sin_clasificacion_bajo_umbral_o_delta_negativo() {
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 0.5)); // < 1.2×SMA
        assert_eq!(density(&bars).unwrap().classified, 0);

        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi - 50.0)); // cierre de posiciones, no abre
        assert_eq!(density(&bars).unwrap().classified, 0);
    }

    #[test]
    fn vela_posterior_mata_niveles_cruzados() {
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 1.5)); // h1: 96/104/98/102/99/101
        bars.push(LiqBar {
            open: 100.0,
            high: 103.0,
            low: 97.0,
            close: 100.0,
            oi: last_oi, // ΔOI negativo: no clasifica
        });
        let d = density(&bars).unwrap();
        // 103 > y > 97 mata 98/99/101/102; el cruce es estricto, así que
        // 96 (low 97 no es < 96) y 104 (high 103 no es > 104) sobreviven
        assert_eq!(d.alive_levels.len(), 2);
        assert!(has(&d.alive_levels, 96.0) && has(&d.alive_levels, 104.0));
    }

    #[test]
    fn la_vela_de_nacimiento_tambien_mata() {
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(LiqBar {
            open: 100.0,
            high: 110.0,
            low: 90.0,
            close: 100.0,      // ohlc4 = 100, rango ±10% estricto
            oi: last_oi + 5.0, // h3
        });
        let d = density(&bars).unwrap();
        // mueren los niveles dentro de (90,110); 90 y 110 exactos sobreviven
        // (cruce estricto), igual que 80 y 120
        assert_eq!(d.alive_levels.len(), 4);
        for v in [80.0, 90.0, 110.0, 120.0] {
            assert!(has(&d.alive_levels, v), "falta nivel {v}");
        }
    }

    #[test]
    fn histograma_acumula_coincidencias() {
        // dos velas h3 con el mismo pivote → niveles duplicados → bins con 2
        let mut bars = baseline(100.0);
        let last_oi = bars.last().unwrap().oi;
        bars.push(flat(100.0, last_oi + 5.0));
        bars.push(flat(100.0, last_oi + 10.0)); // otro ΔOI +5 → h3 de nuevo
        let d = density(&bars).unwrap();
        assert_eq!(d.classified, 2);
        assert_eq!(d.alive_levels.len(), 20);
        let top = top_bins(&d, 100.0, 4);
        assert!(!top.is_empty());
        assert!(
            top[0].count >= 2,
            "el bin más denso debe acumular duplicados"
        );
        // todo bin con cuenta cae dentro del rango
        for b in &top {
            assert!(b.low >= d.range_low && b.high <= d.range_high + 1e-9);
        }
    }
}
