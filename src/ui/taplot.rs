//! Utilidades de hover compartidas por el gráfico de velas (Vista 2) y los
//! paneles de indicadores (Vistas 2 y 3). El trazo de los indicadores ya no se
//! dibuja aquí: lo rasteriza `oscimg` (plotters + ratatui-image); el mapeo
//! columna de celda → vela sigue siendo este, idéntico al del dibujo manual.

use ratatui::prelude::*;

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
