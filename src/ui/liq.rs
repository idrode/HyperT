use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, PairState};
use crate::{liq, liqdens};

use super::fmt::{fmt_px, fmt_usd};

/// Ancho del panel de densidad ΔOI; por debajo solo se muestra el mapa.
const DENS_W: u16 = 46;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.selected_pair().is_none() {
        let s = crate::i18n::t();
        f.render_widget(
            Paragraph::new(s.liq_select_pair)
                .block(Block::bordered().title(format!(" {} ", s.liq_title_word))),
            area,
        );
        return;
    }
    if area.width >= DENS_W + 70 {
        let cols =
            Layout::horizontal([Constraint::Min(60), Constraint::Length(DENS_W)]).split(area);
        draw_map(f, app, cols[0]);
        draw_density(f, app.selected_pair().expect("comprobado arriba"), cols[1]);
    } else {
        draw_map(f, app, area);
    }
}

fn draw_map(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = app.selected_pair() else {
        return;
    };
    let s = crate::i18n::t();
    let coin = p.meta.name.clone();
    let range = app.liq_range();
    let mark = p.mid;
    let title = format!(
        " {} {coin} — {} · {} · ±{:.0}% (r) · TF {} (i) · ←→ {} ",
        s.liq_title_word,
        s.liq_estimate_note,
        s.liq_whales_real,
        range * 100.0,
        p.extra.as_ref().map(|e| e.interval.label()).unwrap_or("…"),
        s.liq_pair,
    );
    let block = Block::bordered().title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 30 {
        return;
    }

    let Some(e) = &p.extra else {
        f.render_widget(Paragraph::new(s.liq_loading_est), inner);
        return;
    };
    if mark <= 0.0 {
        return;
    }

    let whale_liqs = app.whale_liqs_for(&coin);
    // una fila por bucket, reservando una para la línea de mark
    let n_buckets = (inner.height as usize).saturating_sub(1).max(2);
    let buckets = liq::estimate(
        &e.candles,
        p.oi_notional(),
        mark,
        &whale_liqs,
        n_buckets,
        range,
    );
    if buckets.is_empty() {
        return;
    }
    let max_val = buckets
        .iter()
        .map(|b| b.long_est + b.short_est + b.whale_ntl)
        .fold(0.0_f64, f64::max)
        .max(1e-9);

    // etiqueta precio (12) + valor (9) + espacio → resto para la barra
    let bar_w = (inner.width as usize).saturating_sub(12 + 9 + 3).max(10);
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    let mut mark_drawn = false;
    // de mayor precio (arriba) a menor (abajo)
    for b in buckets.iter().rev() {
        if !mark_drawn && b.px < mark {
            let label = format!("── mark {} ", fmt_px(mark));
            let fill = "─".repeat((inner.width as usize).saturating_sub(label.len() + 1));
            lines.push(Line::from(Span::styled(
                format!("{label}{fill}"),
                Style::new().fg(Color::Yellow),
            )));
            mark_drawn = true;
            if lines.len() >= inner.height as usize {
                break;
            }
        }
        let total = b.long_est + b.short_est + b.whale_ntl;
        let frac = total / max_val;
        let filled = ((bar_w as f64) * frac).round() as usize;
        let long_part = if total > 0.0 {
            ((filled as f64) * (b.long_est / total)).round() as usize
        } else {
            0
        };
        let short_part = filled.saturating_sub(long_part);
        let mut spans = vec![Span::styled(
            format!("{:>11} ", fmt_px(b.px)),
            Style::new().fg(Color::Gray),
        )];
        // longs liquidándose (por debajo del mark) en magenta, shorts en cyan
        spans.push(Span::styled(
            "█".repeat(long_part),
            Style::new().fg(Color::Magenta),
        ));
        spans.push(Span::styled(
            "█".repeat(short_part),
            Style::new().fg(Color::Cyan),
        ));
        spans.push(Span::raw(" ".repeat(bar_w.saturating_sub(filled) + 1)));
        if total > max_val * 0.005 {
            spans.push(Span::styled(
                format!("{:>8}", fmt_usd(total)),
                Style::new().fg(Color::DarkGray),
            ));
        }
        if b.whale_ntl > 0.0 {
            spans.push(Span::styled(
                format!(" ◆{}", fmt_usd(b.whale_ntl)),
                Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::from(spans));
        if lines.len() >= inner.height as usize {
            break;
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Panel de densidad de liquidación por ΔOI (port de liq.pine): top de bins
/// con más niveles hipotéticos vivos, con distancia % al mark.
fn draw_density(f: &mut Frame, p: &PairState, area: Rect) {
    let s = crate::i18n::t();
    let block = Block::bordered().title(s.liq_dens_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 30 {
        return;
    }
    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));

    let Some(e) = &p.extra else {
        f.render_widget(Paragraph::new(s.t_loading_candles), inner);
        return;
    };
    let bars = p.liq_bars();
    if bars.len() < liqdens::MIN_BARS {
        let lines = vec![
            Line::from(format!(
                "{}: {}/{} (TF {})",
                s.liq_accum_oi,
                bars.len(),
                liqdens::MIN_BARS,
                e.interval.label()
            )),
            Line::raw(""),
            Line::from(dim(s.liq_dens_note1.into())),
            Line::from(dim(s.liq_dens_note2.into())),
        ];
        f.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let Some(d) = liqdens::density(&bars) else {
        f.render_widget(Paragraph::new(s.liq_not_enough), inner);
        return;
    };

    let mark = p.mid;
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            dim(format!("{} ", s.liq_oi_candles)),
            Span::raw(format!("{}", bars.len())),
            dim(format!(" · {} ", s.liq_classified)),
            Span::raw(format!("{}", d.classified)),
            dim(format!(" · {} ", s.liq_alive_levels)),
            Span::raw(format!("{}", d.alive_levels.len())),
        ]),
        Line::from(dim(format!(
            "{} [{} … {}] · {} {}",
            s.liq_range,
            fmt_px(d.range_low),
            fmt_px(d.range_high),
            liqdens::N_BINS,
            s.liq_bins,
        ))),
    ];

    let rows = (inner.height as usize).saturating_sub(lines.len());
    let top = liqdens::top_bins(&d, mark, rows);
    if top.is_empty() {
        lines.push(Line::from(s.liq_no_alive.to_string()));
    }
    let max_count = top.iter().map(|b| b.count).max().unwrap_or(1).max(1);
    // precio (11) + densidad (4) + distancia (8) + espacios → resto de barra
    let bar_w = (inner.width as usize).saturating_sub(11 + 4 + 8 + 3).max(4);
    for b in &top {
        let px = (b.low + b.high) / 2.0;
        let dist = if mark > 0.0 {
            (px / mark - 1.0) * 100.0
        } else {
            0.0
        };
        // por encima del mark se liquidan shorts (cyan), por debajo longs
        let color = if px >= mark {
            Color::Cyan
        } else {
            Color::Magenta
        };
        let filled = ((bar_w as u32 * b.count + max_count / 2) / max_count) as usize;
        lines.push(Line::from(vec![
            Span::styled(format!("{:>11} ", fmt_px(px)), Style::new().fg(Color::Gray)),
            Span::raw(format!("{:>3} ", b.count)),
            Span::styled(format!("{dist:+7.2}% "), Style::new().fg(color)),
            Span::styled("█".repeat(filled.min(bar_w)), Style::new().fg(color)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}
