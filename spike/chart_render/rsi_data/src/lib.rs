//! Datos RSI de prueba para el spike de renderizado — determinista, sin red.

/// Serie de cierres sintética: tramos de tendencia + ruido LCG, diseñada para
/// que el RSI(14) recorra sobrecompra (>70), zona neutral y sobreventa (<30)
/// como lo haría un par real en swing.
pub fn demo_closes() -> Vec<f64> {
    let mut seed: u64 = 0x5EED_CAFE;
    let mut noise = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as f64 / (1u64 << 31) as f64) - 1.0 // [-1, 1)
    };
    // (nº velas, deriva por vela en %)
    let segments: &[(usize, f64)] = &[
        (40, 0.30),  // subida sostenida → RSI a sobrecompra
        (25, -0.05), // distribución lateral
        (35, -0.45), // caída fuerte → RSI a sobreventa
        (30, 0.02),  // suelo / chop
        (45, 0.35),  // recuperación con impulso
        (25, -0.20), // retroceso final
    ];
    let mut px = 100.0f64;
    let mut out = Vec::with_capacity(200);
    for &(n, drift) in segments {
        for _ in 0..n {
            px *= 1.0 + (drift + noise() * 0.6) / 100.0;
            out.push(px);
        }
    }
    out
}

/// RSI de Wilder. Devuelve la serie alineada al final (len = closes.len() - period).
pub fn rsi(closes: &[f64], period: usize) -> Vec<f64> {
    assert!(closes.len() > period);
    let mut gains = 0.0;
    let mut losses = 0.0;
    for w in closes.windows(2).take(period) {
        let d = w[1] - w[0];
        if d >= 0.0 { gains += d } else { losses -= d }
    }
    let mut avg_g = gains / period as f64;
    let mut avg_l = losses / period as f64;
    let mut out = Vec::with_capacity(closes.len() - period);
    out.push(100.0 - 100.0 / (1.0 + avg_g / avg_l.max(1e-12)));
    for w in closes.windows(2).skip(period) {
        let d = w[1] - w[0];
        avg_g = (avg_g * (period - 1) as f64 + d.max(0.0)) / period as f64;
        avg_l = (avg_l * (period - 1) as f64 + (-d).max(0.0)) / period as f64;
        out.push(100.0 - 100.0 / (1.0 + avg_g / avg_l.max(1e-12)));
    }
    out
}

/// SMA alineada al final (len = xs.len() - n + 1).
pub fn sma(xs: &[f64], n: usize) -> Vec<f64> {
    xs.windows(n).map(|w| w.iter().sum::<f64>() / n as f64).collect()
}

/// (RSI(14), MA(14) del RSI) recortadas a la misma longitud, listas para pintar.
pub fn demo_rsi_ma() -> (Vec<f64>, Vec<f64>) {
    let closes = demo_closes();
    let r = rsi(&closes, 14);
    let m = sma(&r, 14);
    let r = r[r.len() - m.len()..].to_vec();
    (r, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsi_en_rango_y_con_extremos() {
        let (r, m) = demo_rsi_ma();
        assert_eq!(r.len(), m.len());
        assert!(r.len() > 120);
        assert!(r.iter().all(|v| (0.0..=100.0).contains(v)));
        assert!(r.iter().any(|v| *v > 70.0), "debe tocar sobrecompra");
        assert!(r.iter().any(|v| *v < 30.0), "debe tocar sobreventa");
    }
}
