//! Utilidades de hover compartidas por el gráfico de velas (Vista 2) y los
//! paneles de indicadores (Vistas 2 y 3). El trazo de los indicadores ya no se
//! dibuja aquí: lo rasteriza `oscimg` (plotters + ratatui-image); el mapeo
//! columna de celda → vela sigue siendo este, idéntico al del dibujo manual.

use ratatui::prelude::*;

/// Reescala una serie centrada en 0 (TRIX) al eje 0-100 de los paneles de
/// osciladores: 0 → 50, y el mayor |valor| finito de TODA la serie → ±45
/// (queda dentro de la banda sin pisar los márgenes de las marcas ▲▼).
/// Escala por la serie completa, no por la ventana visible, para que el trazo
/// no "salte" al hacer scroll temporal. Devuelve también ese máximo, para que
/// el hover/eje puedan traducir de vuelta al valor real.
pub(super) fn scale_zero_centered(vals: &[f64]) -> (Vec<f64>, f64) {
    let max = vals
        .iter()
        .filter(|v| v.is_finite())
        .fold(0.0_f64, |m, v| m.max(v.abs()))
        .max(1e-12);
    let scaled = vals
        .iter()
        .map(|v| {
            if v.is_finite() {
                50.0 + 45.0 * v / max
            } else {
                f64::NAN
            }
        })
        .collect();
    (scaled, max)
}

#[cfg(test)]
mod tests {
    /// 0 → 50, ±máx → 50±45, NaN se preserva (warmup honesto).
    #[test]
    fn escala_centrada_en_cero() {
        let (s, max) = super::scale_zero_centered(&[f64::NAN, -4.0, 0.0, 2.0]);
        assert_eq!(max, 4.0);
        assert!(s[0].is_nan());
        assert_eq!(s[1], 5.0); // 50 − 45
        assert_eq!(s[2], 50.0);
        assert_eq!(s[3], 72.5); // 50 + 45·(2/4)
    }
}

/// Índice (dentro de la ventana visible) del punto/vela bajo el cursor.
/// `zone` es el área con borde del panel (o la unión de varios apilados:
/// misma columna = misma vela); el eje de `axis_w` a la derecha queda fuera.
pub(super) fn hover_idx(
    mouse: Option<(u16, u16)>,
    zone: Rect,
    axis_w: u16,
    cols_per_pt: u16,
    len: usize,
) -> Option<usize> {
    let (mx, my) = mouse?;
    let x0 = zone.x + 1;
    let w = zone.width.saturating_sub(2 + axis_w);
    if w == 0 || mx < x0 || mx >= x0 + w || my <= zone.y || my + 1 >= zone.bottom() {
        return None;
    }
    let max_vis = ((w / cols_per_pt) as usize).max(2);
    let i = ((mx - x0) / cols_per_pt) as usize;
    (i < len.min(max_vis)).then_some(i)
}

/// Como `hover_idx`, pero con los puntos repartidos uniformemente: `slots`
/// huecos de ventana sobre el ancho útil, sin nº entero de columnas por punto.
/// Es el mapeo de la Vista 3, cuya ventana visible (compartida con la Vista 2)
/// se escala al ancho de su panel más estrecho. Válidos los primeros `len`.
pub(super) fn hover_idx_scaled(
    mouse: Option<(u16, u16)>,
    zone: Rect,
    axis_w: u16,
    slots: usize,
    len: usize,
) -> Option<usize> {
    let (mx, my) = mouse?;
    let x0 = zone.x + 1;
    let w = zone.width.saturating_sub(2 + axis_w);
    if w == 0 || slots == 0 || mx < x0 || mx >= x0 + w || my <= zone.y || my + 1 >= zone.bottom() {
        return None;
    }
    let i = (mx - x0) as usize * slots / w as usize;
    (i < len.min(slots)).then_some(i)
}
