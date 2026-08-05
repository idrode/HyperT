//! Señales puras de la Vista 6 (Flujo de Dinero / Posicionamiento): percentil
//! de funding, actividad de la rotación de OI, asimetría de liquidaciones,
//! divergencia CVD/precio y score compuesto de sobreextensión. Todo contrario:
//! mide "hacia dónde va cargado el barco", no da señales de entrada.

use crate::liq::LiqBucket;

/// Muestras mínimas para que el percentil de funding signifique algo
/// (72 = 3 días de funding horario; el fetch apunta a ~30d).
pub const MIN_PCTL_SAMPLES: usize = 72;
/// Percentil de funding considerado extremo (>p95 crowd long, <p5 crowd short).
pub const FUNDING_PCTL_EXTREME: f64 = 95.0;
/// Premium medio sostenido considerado presión agresiva de un lado (bps).
pub const PREMIUM_EXTREME_BPS: f64 = 5.0;
/// % long de whales fuera de este rango = whales claramente cargadas a un lado.
pub const WHALE_LONG_EXTREME: f64 = 65.0;
/// Combustible de liquidación de un lado ≥ este múltiplo del otro = asimetría.
pub const LIQ_RATIO_EXTREME: f64 = 1.5;
/// El mismo umbral extremo expresado en el eje normalizado de `liq_asym`
/// (ratio r equivale a (r−1)/(r+1) en ese eje).
pub const LIQ_ASYM_EXTREME: f64 = (LIQ_RATIO_EXTREME - 1.0) / (LIQ_RATIO_EXTREME + 1.0);
/// |Δprecio| bajo este umbral (%) se considera plano para la divergencia CVD.
pub const CVD_PX_FLAT_PCT: f64 = 0.15;
/// Ratio de volumen en ventana sobre su media rolling: alto = convicción.
const VOL_RATIO_HI: f64 = 1.3;
/// Ratio de volumen bajo = acumulación silenciosa.
const VOL_RATIO_LO: f64 = 0.7;
/// |ΔOI %| bajo esto es ruido, no rotación.
const OI_MIN_PCT: f64 = 0.5;

/// Percentil (0-100) de `v` contra su propio histórico, con midrank para los
/// empates. None si no hay historia suficiente para que sea significativo.
pub fn percentile_rank(hist: &[f64], v: f64) -> Option<f64> {
    if hist.len() < MIN_PCTL_SAMPLES {
        return None;
    }
    let (mut less, mut equal) = (0usize, 0usize);
    for &h in hist {
        if h < v {
            less += 1;
        } else if h == v {
            equal += 1;
        }
    }
    Some((less as f64 + equal as f64 * 0.5) / hist.len() as f64 * 100.0)
}

/// Volumen negociado estimado en la ventana a partir del volumen rolling 24h:
/// vol_w ≈ vol24_ahora − vol24_antes + (tramo que rodó fuera, asumido uniforme).
/// `w_frac` = ventana / 24h (p. ej. 1h → 1/24). Aproximación honesta: la API
/// no da volumen por ventana, solo el rolling de 24h.
pub fn window_vol_est(vol24_now: f64, vol24_then: f64, w_frac: f64) -> f64 {
    (vol24_now - vol24_then + vol24_then * w_frac).max(0.0)
}

/// Carácter de la rotación de OI en una ventana. La dirección la da el signo
/// del ΔOI; esto solo califica CÓMO se movió (con cuánto volumen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// OI sube con volumen claramente alto: posicionamiento con convicción.
    Conviccion,
    /// OI sube con volumen claramente bajo: acumulación silenciosa.
    Silenciosa,
    /// OI sube sin ratio de volumen concluyente.
    Normal,
    /// OI baja: posiciones cerrándose, sea cual sea el volumen.
    Salida,
    /// ΔOI dentro del deadband: nada que calificar.
    Flat,
}

impl Activity {
    pub fn label(&self) -> &'static str {
        match self {
            Activity::Conviccion => crate::i18n::t().act_conviccion,
            Activity::Silenciosa => crate::i18n::t().act_silenciosa,
            Activity::Normal => crate::i18n::t().act_normal,
            Activity::Salida => crate::i18n::t().act_salida,
            Activity::Flat => "—",
        }
    }
}

/// Clasifica la rotación: `oi_delta_pct` en %, `vol_ratio` = volumen estimado
/// de la ventana / su volumen típico (rolling 24h prorrateado), None si aún
/// no hay historia de volumen.
pub fn classify_activity(oi_delta_pct: f64, vol_ratio: Option<f64>) -> Activity {
    if oi_delta_pct.abs() < OI_MIN_PCT {
        return Activity::Flat;
    }
    if oi_delta_pct < 0.0 {
        return Activity::Salida;
    }
    match vol_ratio {
        Some(r) if r >= VOL_RATIO_HI => Activity::Conviccion,
        Some(r) if r <= VOL_RATIO_LO => Activity::Silenciosa,
        _ => Activity::Normal,
    }
}

/// Combustible de liquidación (abajo, arriba) del mark a partir de los buckets
/// estimados: longs se liquidan por debajo, shorts por encima; el notional de
/// whales cuenta en el lado donde esté su precio de liquidación. Los buckets
/// deben venir ya acotados al rango deseado (±3% para la señal de asimetría).
pub fn liq_fuel(buckets: &[LiqBucket], mark: f64) -> Option<(f64, f64)> {
    if buckets.is_empty() || mark <= 0.0 {
        return None;
    }
    let (mut below, mut above) = (0.0, 0.0);
    for b in buckets {
        if b.px < mark {
            below += b.long_est + b.whale_ntl;
        } else {
            above += b.short_est + b.whale_ntl;
        }
    }
    Some((below, above))
}

/// Asimetría normalizada del combustible: (abajo − arriba) / total, en
/// [−1, +1]. Positivo = más combustible ABAJO = camino de menor resistencia
/// bajista; negativo el espejo alcista. None sin combustible a ningún lado:
/// sin datos no hay sesgo, nunca un 0 falso que ordene como neutral.
pub fn liq_asym(below: f64, above: f64) -> Option<f64> {
    let total = below + above;
    if total <= 0.0 {
        return None;
    }
    Some((below - above) / total)
}

/// Clave de orden por confluencia: el score compuesto y la asimetría de
/// combustible apuntando al MISMO lado. Positivo = confluencia bajista,
/// negativo = alcista; la magnitud la domina el neto del score y la asimetría
/// desempata. Some(0) = hay datos pero no coinciden o ninguno es direccional;
/// None = falta el combustible o el score no tiene componentes — esos pares
/// van al final del ranking, nunca ordenados como si fueran cero.
pub fn confluence(s: Score, asym: Option<f64>) -> Option<f64> {
    let a = asym?;
    if s.avail == 0 {
        return None;
    }
    let net = s.bear as f64 - s.bull as f64;
    let fuel_dir = if a >= LIQ_ASYM_EXTREME {
        1.0
    } else if a <= -LIQ_ASYM_EXTREME {
        -1.0
    } else {
        0.0
    };
    if net == 0.0 || fuel_dir == 0.0 || (net > 0.0) != (fuel_dir > 0.0) {
        return Some(0.0);
    }
    Some((net.abs() + a.abs()) * net.signum())
}

/// Divergencia CVD/precio en una ventana. Solo lee algo cuando el precio está
/// plano: agresión de un lado que NO mueve el precio = el otro lado la absorbe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CvdSignal {
    /// CVD sube y el precio no: compras agresivas absorbidas (techo, bajista).
    AbsorcionCompras,
    /// CVD baja y el precio no: ventas agresivas absorbidas (suelo, alcista).
    AbsorcionVentas,
    Neutro,
}

impl CvdSignal {
    pub fn label(&self) -> &'static str {
        match self {
            CvdSignal::AbsorcionCompras => crate::i18n::t().cvd_absorb_buy,
            CvdSignal::AbsorcionVentas => crate::i18n::t().cvd_absorb_sell,
            CvdSignal::Neutro => crate::i18n::t().cvd_neutro,
        }
    }
}

/// `cvd_delta_ntl` = Δ del CVD en la ventana (USD, con signo); `px_chg_pct` =
/// Δ precio en la misma ventana; `min_ntl` = umbral de significancia del CVD
/// (p. ej. una fracción del volumen estimado de la ventana).
pub fn cvd_divergence(cvd_delta_ntl: f64, px_chg_pct: f64, min_ntl: f64) -> CvdSignal {
    if px_chg_pct.abs() > CVD_PX_FLAT_PCT || min_ntl <= 0.0 {
        return CvdSignal::Neutro;
    }
    if cvd_delta_ntl >= min_ntl {
        CvdSignal::AbsorcionCompras
    } else if cvd_delta_ntl <= -min_ntl {
        CvdSignal::AbsorcionVentas
    } else {
        CvdSignal::Neutro
    }
}

/// Entradas del score compuesto; None = componente sin datos todavía (se
/// muestra honesto como no disponible, no cuenta en el total).
#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreInputs {
    /// Percentil 0-100 del funding actual contra su histórico.
    pub funding_pctile: Option<f64>,
    /// Premium medio sostenido de la ventana, en bps.
    pub premium_mean_bps: Option<f64>,
    /// % long del notional de whales en el par (0-100).
    pub whale_pct_long: Option<f64>,
    /// Combustible de liquidación (abajo, arriba) a ±3%.
    pub liq_fuel: Option<(f64, f64)>,
    pub cvd: Option<CvdSignal>,
}

/// Extremos que apuntan en cada dirección de precio. `bear` = evidencia de
/// camino abajo (crowd long sobrecargado, whales cortas, combustible abajo…),
/// `bull` el espejo. `avail` = componentes con datos: el score se lee "n de m".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Score {
    pub bear: u8,
    pub bull: u8,
    pub avail: u8,
}

pub fn score(inp: &ScoreInputs) -> Score {
    let mut s = Score::default();
    let mut add = |flag: Option<bool>| {
        s.avail += 1;
        match flag {
            Some(true) => s.bear += 1,
            Some(false) => s.bull += 1,
            None => {}
        }
    };
    if let Some(p) = inp.funding_pctile {
        // funding extremo alto = masa long pagando = contrario bajista
        add(if p >= FUNDING_PCTL_EXTREME {
            Some(true)
        } else if p <= 100.0 - FUNDING_PCTL_EXTREME {
            Some(false)
        } else {
            None
        });
    }
    if let Some(bps) = inp.premium_mean_bps {
        add(if bps >= PREMIUM_EXTREME_BPS {
            Some(true)
        } else if bps <= -PREMIUM_EXTREME_BPS {
            Some(false)
        } else {
            None
        });
    }
    if let Some(l) = inp.whale_pct_long {
        // whales netas cortas = dinero informado apunta abajo
        add(if l <= 100.0 - WHALE_LONG_EXTREME {
            Some(true)
        } else if l >= WHALE_LONG_EXTREME {
            Some(false)
        } else {
            None
        });
    }
    if let Some((below, above)) = inp.liq_fuel {
        add(if below > 0.0 && below >= above * LIQ_RATIO_EXTREME {
            Some(true)
        } else if above > 0.0 && above >= below * LIQ_RATIO_EXTREME {
            Some(false)
        } else {
            None
        });
    }
    if let Some(c) = inp.cvd {
        add(match c {
            CvdSignal::AbsorcionCompras => Some(true),
            CvdSignal::AbsorcionVentas => Some(false),
            CvdSignal::Neutro => None,
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_needs_history() {
        let short = vec![0.0; MIN_PCTL_SAMPLES - 1];
        assert!(percentile_rank(&short, 1.0).is_none());
    }

    #[test]
    fn percentile_rank_midrank() {
        // 100 valores 0..=99: 49.5 deja la mitad por debajo; 99.5 los deja todos
        let hist: Vec<f64> = (0..100).map(|i| i as f64).collect();
        assert_eq!(percentile_rank(&hist, 49.5), Some(50.0));
        assert_eq!(percentile_rank(&hist, 99.5), Some(100.0));
        assert_eq!(percentile_rank(&hist, -1.0), Some(0.0));
        // empate exacto cuenta medio punto (midrank)
        assert_eq!(percentile_rank(&hist, 0.0), Some(0.5));
        let flat = vec![2.0; 100];
        assert_eq!(percentile_rank(&flat, 2.0), Some(50.0));
    }

    #[test]
    fn window_vol_estimate() {
        // sin cambio del rolling: la ventana negoció justo lo que rodó fuera
        assert_eq!(window_vol_est(2400.0, 2400.0, 1.0 / 24.0), 100.0);
        // rolling subió: la ventana negoció el alza más el tramo rodado
        assert_eq!(window_vol_est(2500.0, 2400.0, 1.0 / 24.0), 200.0);
        // nunca negativo aunque el rolling caiga a plomo
        assert_eq!(window_vol_est(100.0, 2400.0, 1.0 / 24.0), 0.0);
    }

    #[test]
    fn activity_matrix() {
        assert_eq!(classify_activity(0.2, Some(2.0)), Activity::Flat);
        assert_eq!(classify_activity(-3.0, Some(2.0)), Activity::Salida);
        assert_eq!(classify_activity(3.0, Some(1.5)), Activity::Conviccion);
        assert_eq!(classify_activity(3.0, Some(0.5)), Activity::Silenciosa);
        assert_eq!(classify_activity(3.0, Some(1.0)), Activity::Normal);
        assert_eq!(classify_activity(3.0, None), Activity::Normal);
    }

    fn bucket(px: f64, long: f64, short: f64, whale: f64) -> LiqBucket {
        LiqBucket {
            px,
            long_est: long,
            short_est: short,
            whale_ntl: whale,
        }
    }

    #[test]
    fn liq_fuel_sides() {
        let buckets = vec![
            bucket(97.0, 300.0, 0.0, 50.0),
            bucket(99.0, 200.0, 0.0, 0.0),
            bucket(101.0, 0.0, 100.0, 25.0),
        ];
        let (below, above) = liq_fuel(&buckets, 100.0).unwrap();
        assert_eq!(below, 550.0);
        assert_eq!(above, 125.0);
        assert!(liq_fuel(&[], 100.0).is_none());
        assert!(liq_fuel(&buckets, 0.0).is_none());
    }

    #[test]
    fn cvd_divergence_matrix() {
        // precio plano + CVD comprador significativo → compras absorbidas
        assert_eq!(
            cvd_divergence(1000.0, 0.05, 500.0),
            CvdSignal::AbsorcionCompras
        );
        assert_eq!(
            cvd_divergence(-1000.0, -0.05, 500.0),
            CvdSignal::AbsorcionVentas
        );
        // precio moviéndose: el CVD confirma, no diverge
        assert_eq!(cvd_divergence(1000.0, 1.0, 500.0), CvdSignal::Neutro);
        // CVD bajo el umbral de significancia
        assert_eq!(cvd_divergence(100.0, 0.0, 500.0), CvdSignal::Neutro);
        // sin umbral válido no hay lectura
        assert_eq!(cvd_divergence(1000.0, 0.0, 0.0), CvdSignal::Neutro);
    }

    #[test]
    fn liq_asym_axis() {
        assert_eq!(liq_asym(900.0, 100.0), Some(0.8));
        assert_eq!(liq_asym(100.0, 900.0), Some(-0.8));
        assert_eq!(liq_asym(500.0, 500.0), Some(0.0));
        assert!(liq_asym(0.0, 0.0).is_none());
    }

    #[test]
    fn confluence_same_side_only() {
        let bear = Score {
            bear: 3,
            bull: 1,
            avail: 5,
        };
        // score bajista + combustible abajo extremo → confluencia bajista
        assert_eq!(confluence(bear, Some(0.5)), Some(2.5));
        // combustible al lado contrario → datos sin confluencia
        assert_eq!(confluence(bear, Some(-0.5)), Some(0.0));
        // asimetría bajo el umbral extremo no es direccional
        assert_eq!(confluence(bear, Some(0.1)), Some(0.0));
        // sin combustible no hay clave: el par va al final, no como cero
        assert!(confluence(bear, None).is_none());
        let bull = Score {
            bear: 0,
            bull: 2,
            avail: 4,
        };
        assert_eq!(confluence(bull, Some(-0.5)), Some(-2.5));
        // score empatado no es direccional
        let flat = Score {
            bear: 1,
            bull: 1,
            avail: 5,
        };
        assert_eq!(confluence(flat, Some(0.9)), Some(0.0));
    }

    #[test]
    fn score_full_bear() {
        let s = score(&ScoreInputs {
            funding_pctile: Some(97.0),
            premium_mean_bps: Some(8.0),
            whale_pct_long: Some(30.0),
            liq_fuel: Some((900.0, 100.0)),
            cvd: Some(CvdSignal::AbsorcionCompras),
        });
        assert_eq!(
            s,
            Score {
                bear: 5,
                bull: 0,
                avail: 5
            }
        );
    }

    #[test]
    fn score_full_bull() {
        let s = score(&ScoreInputs {
            funding_pctile: Some(2.0),
            premium_mean_bps: Some(-8.0),
            whale_pct_long: Some(70.0),
            liq_fuel: Some((100.0, 900.0)),
            cvd: Some(CvdSignal::AbsorcionVentas),
        });
        assert_eq!(
            s,
            Score {
                bear: 0,
                bull: 5,
                avail: 5
            }
        );
    }

    #[test]
    fn score_partial_availability() {
        // solo 2 componentes con datos, uno extremo y otro neutral
        let s = score(&ScoreInputs {
            funding_pctile: Some(50.0),
            whale_pct_long: Some(20.0),
            ..ScoreInputs::default()
        });
        assert_eq!(
            s,
            Score {
                bear: 1,
                bull: 0,
                avail: 2
            }
        );
        // sin combustible a ningún lado: disponible pero sin bandera
        let s0 = score(&ScoreInputs {
            liq_fuel: Some((0.0, 0.0)),
            ..ScoreInputs::default()
        });
        assert_eq!(
            s0,
            Score {
                bear: 0,
                bull: 0,
                avail: 1
            }
        );
    }
}
