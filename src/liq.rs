//! Estimación del mapa de liquidaciones. IMPORTANTE: Hyperliquid no expone un feed
//! exacto de niveles de liquidación de mercado completo (el endpoint `liquidatable`
//! devuelve [] — verificado). Esto es una APROXIMACIÓN estilo Coinglass: distribuye el
//! OI actual entre precios de entrada recientes (ponderados por volumen de vela) y
//! tramos de apalancamiento típicos. Los únicos niveles exactos son los de las whales
//! trackeadas (clearinghouseState.liquidationPx), que se agregan aparte.

use crate::data::types::CandlePoint;

#[derive(Debug, Clone, Copy)]
pub struct LiqBucket {
    /// Precio central del bucket.
    pub px: f64,
    /// Notional estimado de longs que se liquidarían en este precio.
    pub long_est: f64,
    /// Notional estimado de shorts que se liquidarían en este precio.
    pub short_est: f64,
    /// Notional real de whales trackeadas con liq px en este bucket.
    pub whale_ntl: f64,
}

/// Tramos de apalancamiento asumidos y su peso en el OI (supuesto, no dato real).
const TIERS: [(f64, f64); 4] = [(5.0, 0.30), (10.0, 0.35), (20.0, 0.25), (40.0, 0.10)];
/// liq ≈ entry × (1 ∓ 0.9/lev): el 0.9 aproxima el margen de mantenimiento.
const MAINT_FACTOR: f64 = 0.9;

pub fn estimate(
    candles: &[CandlePoint],
    oi_notional: f64,
    mark: f64,
    whale_liqs: &[(f64, f64)],
    n_buckets: usize,
    range_pct: f64,
) -> Vec<LiqBucket> {
    if n_buckets == 0 || mark <= 0.0 || range_pct <= 0.0 {
        return Vec::new();
    }
    let lo = mark * (1.0 - range_pct);
    let hi = mark * (1.0 + range_pct);
    let step = (hi - lo) / n_buckets as f64;
    let mut buckets: Vec<LiqBucket> = (0..n_buckets)
        .map(|i| LiqBucket {
            px: lo + step * (i as f64 + 0.5),
            long_est: 0.0,
            short_est: 0.0,
            whale_ntl: 0.0,
        })
        .collect();
    let idx_of = |px: f64| -> Option<usize> {
        if px < lo || px >= hi {
            return None;
        }
        Some((((px - lo) / step) as usize).min(n_buckets - 1))
    };

    // en un perp OI long == OI short: cada lado recibe el OI completo
    let total_vol: f64 = candles.iter().map(|c| c.volume * c.close).sum();
    if total_vol > 0.0 && oi_notional > 0.0 {
        for c in candles {
            if c.close <= 0.0 {
                continue;
            }
            let w = (c.volume * c.close) / total_vol;
            for (lev, share) in TIERS {
                let liq_long = c.close * (1.0 - MAINT_FACTOR / lev);
                let liq_short = c.close * (1.0 + MAINT_FACTOR / lev);
                if let Some(i) = idx_of(liq_long) {
                    buckets[i].long_est += oi_notional * w * share;
                }
                if let Some(i) = idx_of(liq_short) {
                    buckets[i].short_est += oi_notional * w * share;
                }
            }
        }
    }
    for (px, ntl) in whale_liqs {
        if let Some(i) = idx_of(*px) {
            buckets[i].whale_ntl += ntl;
        }
    }
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candle(close: f64, volume: f64) -> CandlePoint {
        CandlePoint {
            t_close: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume,
        }
    }

    #[test]
    fn longs_below_shorts_above() {
        let candles = vec![candle(100.0, 10.0)];
        let b = estimate(&candles, 1_000_000.0, 100.0, &[], 40, 0.30);
        let long_below: f64 = b.iter().filter(|x| x.px < 100.0).map(|x| x.long_est).sum();
        let long_above: f64 = b.iter().filter(|x| x.px > 100.0).map(|x| x.long_est).sum();
        let short_above: f64 = b.iter().filter(|x| x.px > 100.0).map(|x| x.short_est).sum();
        assert!(long_below > 0.0);
        assert_eq!(long_above, 0.0);
        assert!(short_above > 0.0);
        // todo el OI de cada lado cae dentro del rango ±30% con entradas en mark
        assert!((long_below - 1_000_000.0).abs() < 1.0);
        assert!((short_above - 1_000_000.0).abs() < 1.0);
    }

    #[test]
    fn whale_bucket() {
        let b = estimate(&[], 0.0, 100.0, &[(95.0, 5000.0)], 10, 0.10);
        let total: f64 = b.iter().map(|x| x.whale_ntl).sum();
        assert_eq!(total, 5000.0);
        // 95 está en la mitad inferior del rango [90,110)
        let hit = b.iter().find(|x| x.whale_ntl > 0.0).unwrap();
        assert!(hit.px < 100.0);
    }

    #[test]
    fn empty_without_data() {
        assert!(estimate(&[], 0.0, 0.0, &[], 10, 0.1).is_empty());
        let b = estimate(&[], 0.0, 100.0, &[], 10, 0.1);
        assert!(b.iter().all(|x| x.long_est == 0.0 && x.short_est == 0.0));
    }
}
