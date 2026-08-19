use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::i18n::{self, HelpRow};

pub fn draw(f: &mut Frame) {
    // alto pedido = todas las filas + bordes (`centered` lo recorta al alto real
    // del terminal); con 48 fijas la ayuda se cortaba por abajo y las secciones
    // de Fondos/Flujo no llegaban a verse nunca.
    let h = i18n::help_rows().len() as u16 + 2;
    let area = centered(72, h, f.area());
    f.render_widget(Clear, area);

    let key = |k: &str, desc: &str| {
        Line::from(vec![
            Span::styled(
                format!("  {k:<12}"),
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(desc.to_string()),
        ])
    };
    let section = |s: &str| {
        Line::from(Span::styled(
            s.to_string(),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
    };
    let lines: Vec<Line> = i18n::help_rows()
        .iter()
        .map(|row| match row {
            HelpRow::Section(s) => section(s),
            HelpRow::Key(k, d) => key(k, d),
            HelpRow::Blank => Line::raw(""),
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(i18n::t().help_title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

fn centered(w: u16, h: u16, r: Rect) -> Rect {
    let w = w.min(r.width);
    let h = h.min(r.height);
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}
