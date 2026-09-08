//! Drop shadow de terminal, estilo Turbo Vision / Norton Commander.
//!
//! Se dibuja ANTES del panel real, en un rectángulo desplazado abajo-derecha:
//! el panel tapa después casi toda la sombra, que solo asoma por el borde
//! inferior y el derecho. El desplazamiento es asimétrico (2 columnas, 1 fila)
//! porque la celda de terminal es más alta que ancha, así la sombra se ve
//! cuadrada en pantalla.
//!
//! Es una utilidad genérica: la usan todos los overlays flotantes del
//! proyecto (modales de las Vistas 2/3/7/8/9, ayuda, buscador y el widget
//! rápido de la Vista 1), no solo uno.

use ratatui::layout::Rect;
use ratatui::Frame;

use super::theme;

/// Desplazamiento de la sombra respecto al panel.
pub const DX: u16 = 2;
pub const DY: u16 = 1;

/// Pinta la sombra de `area`. Llamar justo ANTES de dibujar el panel (y antes
/// del `Clear` del overlay, que borra la parte que el panel va a tapar).
pub fn draw(f: &mut Frame, area: Rect) {
    let full = f.area();
    let shifted = Rect::new(
        area.x.saturating_add(DX),
        area.y.saturating_add(DY),
        area.width,
        area.height,
    )
    .intersection(full);
    if shifted.width == 0 || shifted.height == 0 {
        return;
    }
    let col = theme::shadow_bg();
    let buf = f.buffer_mut();
    for y in shifted.top()..shifted.bottom() {
        for x in shifted.left()..shifted.right() {
            // dentro del propio panel no hace falta ensuciar: lo repinta él
            if x < area.right() && y < area.bottom() && x >= area.x && y >= area.y {
                continue;
            }
            let cell = &mut buf[(x, y)];
            // el contenido de debajo se apaga a la sombra, no se borra a un
            // bloque negro: se conserva el símbolo y se oscurece el fondo
            cell.set_bg(col);
            cell.set_fg(col);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    use super::super::theme;

    /// La sombra cae abajo-derecha, respeta el hueco del propio panel y sigue
    /// al tema activo (nunca un negro fijo).
    #[test]
    fn la_sombra_cae_abajo_a_la_derecha_y_sigue_al_tema() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let area = Rect::new(4, 3, 10, 5);
        let mut anteriores = Vec::new();
        for th in [theme::Theme::Dark, theme::Theme::Light] {
            theme::set_theme(th);
            let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
            term.draw(|f| super::draw(f, area)).unwrap();
            let buf = term.backend().buffer().clone();
            let bg = |x: u16, y: u16| buf.cell((x, y)).unwrap().bg;

            // esquina inferior-derecha: sombreada
            assert_eq!(bg(area.right(), area.bottom()), theme::shadow_bg());
            assert_eq!(bg(area.right() + 1, area.y + 1), theme::shadow_bg());
            // el interior del panel se deja intacto (lo pinta el panel)
            assert_eq!(bg(area.x, area.y), ratatui::style::Color::Reset);
            // arriba y a la izquierda del panel, nada
            assert_eq!(
                bg(area.x, area.y.saturating_sub(1)),
                ratatui::style::Color::Reset
            );
            assert_eq!(
                bg(area.x.saturating_sub(1), area.y),
                ratatui::style::Color::Reset
            );
            // la fila justo bajo el panel arranca desplazada DX columnas
            assert_eq!(bg(area.x, area.bottom()), ratatui::style::Color::Reset);
            assert_eq!(bg(area.x + super::DX, area.bottom()), theme::shadow_bg());
            anteriores.push(theme::shadow_bg());
        }
        assert_ne!(anteriores[0], anteriores[1], "la sombra cambia con el tema");
        theme::set_theme(theme::Theme::Dark);
    }

    /// Una sombra que se sale de la pantalla se recorta, no revienta.
    #[test]
    fn la_sombra_se_recorta_en_el_borde() {
        let mut term = Terminal::new(TestBackend::new(20, 10)).unwrap();
        term.draw(|f| super::draw(f, Rect::new(15, 8, 5, 2)))
            .unwrap();
    }
}
