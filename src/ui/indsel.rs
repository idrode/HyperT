//! Selector de indicadores (tecla `o`) de las Vistas 2 y 3 — overlay centrado
//! con el mismo patrón que los demás modales (`Clear` + Esc cierra).
//!
//! Libertad total en ambas vistas. En la Vista 3 la selección SOLO afecta a
//! qué líneas se dibujan en el panel: el checklist de condiciones, el log y
//! las marcas ▲▼ de ballena se calculan y muestran siempre (el modal lo
//! recuerda con una nota).

use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph};

use crate::app::{App, View};

pub fn draw(f: &mut Frame, app: &App) {
    let Some(cur) = app.ind_ui else { return };
    let tr = crate::i18n::t();
    let v3 = app.view == View::WhaleRsi;

    let trix_label = format!("TRIX({})", crate::signals::TRIX_PERIOD);
    // (etiqueta, activo, índice de fila navegable)
    let rows: Vec<(String, bool, Option<usize>)> = if v3 {
        vec![
            (tr.ind_rsi_label.to_string(), app.ind3.rsi_ma, Some(0)),
            (tr.ind_mod_label.to_string(), app.ind3.mod_b, Some(1)),
            (tr.ind_adx_label.to_string(), app.ind3.adx_dmi, Some(2)),
            (trix_label, app.ind3.trix, Some(3)),
        ]
    } else {
        vec![
            (tr.ind_rsi_label.to_string(), app.ind.rsi, Some(0)),
            (tr.ind_adx_label.to_string(), app.ind.adx_dmi, Some(1)),
            (trix_label, app.ind.trix, Some(2)),
        ]
    };

    let h = rows.len() as u16 + 4 + if v3 { 1 } else { 0 };
    let area = super::exec::centered(46, h, f.area());
    super::shadow::draw(f, area);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(tr.ind_title)
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![Line::raw("")];
    for (label, on, nav) in &rows {
        let mark = if *on { "[x]" } else { "[ ]" };
        match nav {
            Some(i) => {
                let sel = *i == cur;
                let st = if sel {
                    Style::new().fg(Color::Black).bg(Color::Cyan)
                } else {
                    Style::new()
                };
                let cursor = if sel { "▶" } else { " " };
                lines.push(Line::from(vec![
                    Span::raw(format!(" {cursor} ")),
                    Span::styled(format!("{mark} {label}"), st),
                ]));
            }
            None => {
                // stack core de la Vista 3: visible pero no conmutable
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(format!("[x] {label}"), Style::new().fg(Color::DarkGray)),
                ]));
            }
        }
    }
    if v3 {
        // ocultar líneas no apaga nada: checklist y ▲▼ siguen corriendo
        lines.push(Line::from(Span::styled(
            tr.ind_always_note,
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        tr.ind_hint,
        Style::new().fg(Color::DarkGray),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}
