//! Overlay del buscador incremental de par (tecla `/`, Vistas 1 y 6):
//! caja pequeña anclada abajo-izquierda estilo línea de comandos de nvim,
//! la tabla de detrás se filtra en vivo mientras se escribe.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App) {
    let a = f.area();
    let h = 3u16;
    if a.height <= h + 1 {
        return;
    }
    let w = a.width.min(48);
    let area = Rect::new(a.x + 1, a.y + a.height - h - 1, w, h);
    f.render_widget(Clear, area);

    let n = app.search_results().len();
    let line = Line::from(vec![
        Span::styled(
            "/",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.search.query.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Span::styled("▏", Style::new().fg(Color::Cyan)),
        Span::styled(
            format!(
                "  {n} {}",
                if n == 1 {
                    crate::i18n::t().search_matches_one
                } else {
                    crate::i18n::t().search_matches_many
                }
            ),
            Style::new().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(
        Paragraph::new(line).block(
            Block::bordered()
                .title(crate::i18n::t().search_box_title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}
