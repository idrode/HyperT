use std::cmp::Ordering;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, HeatMetric, PairState, OI_WIN_LONG};

use super::fmt::fmt_usd;

const CELL_W: u16 = 14;
const CELL_H: u16 = 3;
/// Tope de pares aunque quepan más celdas: con los ~230 pares del universo el
/// heatmap deja de leerse de un vistazo; el resto se consulta en el Ranking.
const MAX_PAIRS: usize = 30;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let s = crate::i18n::t();
    let block = Block::bordered().title(format!(
        " Heatmap top-{MAX_PAIRS}{}{}{}",
        s.hm_by_oi_metric,
        app.heat_metric.label(),
        s.hm_m_cycles
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < CELL_W || inner.height < CELL_H {
        return;
    }

    let cols = (inner.width / CELL_W).max(1);
    let rows = (inner.height / CELL_H).max(1);
    let n = ((cols as usize) * (rows as usize)).min(MAX_PAIRS);

    // celdas ordenadas por OI notional: el peso por OI es el orden/presencia
    let mut v: Vec<&PairState> = app
        .pairs
        .values()
        .filter(|p| p.oi_notional() > 0.0)
        .collect();
    v.sort_by(|a, b| {
        b.oi_notional()
            .partial_cmp(&a.oi_notional())
            .unwrap_or(Ordering::Equal)
    });
    v.truncate(n);

    for (idx, p) in v.iter().enumerate() {
        let r = (idx as u16) / cols;
        let c = (idx as u16) % cols;
        let rect = Rect::new(inner.x + c * CELL_W, inner.y + r * CELL_H, CELL_W, CELL_H)
            .intersection(inner);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }

        let (t, text) = metric_of(app.heat_metric, p);
        let bg = heat_color(t);
        let lines = vec![
            Line::from(Span::styled(
                p.meta.name.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            )),
            Line::from(text),
            Line::from(Span::styled(
                fmt_usd(p.oi_notional()),
                Style::new().fg(Color::Rgb(200, 200, 200)),
            )),
        ];
        f.render_widget(
            Paragraph::new(lines)
                .style(Style::new().bg(bg).fg(Color::White))
                .alignment(Alignment::Center),
            rect,
        );
    }
}

/// Devuelve (intensidad -1..1 donde +1 = verde, texto de la celda).
fn metric_of(metric: HeatMetric, p: &PairState) -> (f64, String) {
    match metric {
        HeatMetric::FundApr => {
            let apr = p.funding_apr_pct().unwrap_or(0.0);
            // funding positivo = longs pagan = mercado cargado de longs = rojo
            (-apr / 50.0, format!("{apr:+.1}% APR"))
        }
        HeatMetric::OiD1h => match p.oi_delta_pct(OI_WIN_LONG) {
            Some(d) => (d / 5.0, format!("ΔOI {d:+.2}%")),
            None => (0.0, "ΔOI —".to_string()),
        },
        HeatMetric::Chg24 => {
            let c = p.chg24_pct().unwrap_or(0.0);
            (c / 10.0, format!("{c:+.2}% 24h"))
        }
    }
}

fn heat_color(t: f64) -> Color {
    let t = t.clamp(-1.0, 1.0);
    if t >= 0.0 {
        Color::Rgb(20, 45 + (110.0 * t) as u8, 30)
    } else {
        Color::Rgb(45 + (140.0 * (-t)) as u8, 25, 30)
    }
}
