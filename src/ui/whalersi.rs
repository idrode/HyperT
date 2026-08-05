//! Vista 3 — panel Ballenas + RSI/ADX/DMI (port del Pine `whales+RSI`, v6).
//! TA puro sobre precio: marca velas que rompen el Bollinger de precio con RSI
//! extremo, DI contrario dominante y ADX bajo — reversión en mercado sin
//! tendencia, no continuación. ESTIMACIÓN técnica, sin OI ni datos on-chain.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};

use crate::app::{App, PairExtraData, PairState};
use crate::signals::{WhaleParams, WhaleSide};

use super::fmt::{age_label, fmt_px, time_label};
use super::oscimg::{self, LineColor, OscLine, OscSpec};
use super::pair;
use super::taplot;

const TITLE: &str = " Ballenas + RSI/ADX/DMI ";
/// Ancho reservado al eje 0-100 a la derecha del panel.
const AXIS_W: u16 = 5;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let placeholder = |f: &mut Frame, msg: &'static str| {
        f.render_widget(
            Paragraph::new(msg).block(Block::bordered().title(TITLE)),
            area,
        );
    };
    // el caché de imagen (gfx) y el par se prestan por campos disjuntos de App
    let gfx = &mut app.gfx;
    let Some(p) = app.selected_coin.as_deref().and_then(|c| app.pairs.get(c)) else {
        placeholder(
            f,
            "Selecciona un par en el Ranking (Enter) o pulsa 3 de nuevo.",
        );
        return;
    };
    let Some(e) = &p.extra else {
        placeholder(f, "cargando velas…");
        return;
    };
    if e.candles.len() < 2 {
        placeholder(f, "sin datos de velas");
        return;
    }

    let rows = Layout::vertical([Constraint::Length(6), Constraint::Min(8)]).split(area);
    draw_summary(f, p, e, rows[0]);
    let cols =
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).split(rows[1]);
    // ventana temporal compartida con la Vista 2: mismo nº de velas visibles
    // para la misma área de vista → mismo rango de fechas en ambas vistas,
    // aunque este panel sea más estrecho (el trazo se escala al ancho)
    let win = pair::visible_candles(area);
    let hover = taplot::hover_idx_scaled(app.mouse_pos, cols[0], AXIS_W, win, e.candles.len());
    draw_chart(f, e, gfx, hover, win, cols[0]);
    draw_log(f, e, cols[1]);
}

/// Color del RSI según zona, como el rsiColor del Pine.
/// Compartido con el sub-panel de indicadores de la Vista 2.
pub(super) fn rsi_zone_color(v: f64, wp: &WhaleParams) -> Color {
    if v >= wp.overbought {
        Color::Red
    } else if v <= wp.oversold {
        Color::Green
    } else {
        Color::Magenta
    }
}

fn draw_summary(f: &mut Frame, p: &PairState, e: &PairExtraData, area: Rect) {
    let wp = WhaleParams::default();
    let panel = &e.panel;
    let i = e.candles.len() - 1;
    let last = &e.candles[i];

    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
    let colored = |s: String, c: Color| Span::styled(s, Style::new().fg(c));
    let num = |v: f64| {
        if v.is_finite() {
            format!("{v:.0}")
        } else {
            "—".to_string()
        }
    };
    let px_or_dash = |v: f64| {
        if v.is_finite() {
            fmt_px(v)
        } else {
            "—".to_string()
        }
    };

    let last_trig = panel.triggers.last().map(|t| {
        let (arrow, c) = match t.side {
            WhaleSide::Buy => ("▲ compra", Color::Green),
            WhaleSide::Sell => ("▼ venta", Color::Red),
        };
        (
            format!("{arrow} int {:.1} ", t.height),
            c,
            age_label(e.candles[t.idx].t_close),
        )
    });
    let mut l1 = vec![
        Span::styled(
            format!("{} ", p.meta.name),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(fmt_px(p.mid), Style::new().add_modifier(Modifier::BOLD)),
        dim(format!(
            "  TF {} · {} velas · último disparo: ",
            e.interval.label(),
            e.candles.len()
        )),
    ];
    match last_trig {
        Some((txt, c, age)) => {
            l1.push(Span::styled(
                txt,
                Style::new().fg(c).add_modifier(Modifier::BOLD),
            ));
            l1.push(dim(age));
        }
        None => l1.push(dim("ninguno en las velas cargadas".to_string())),
    }

    let rsi = panel.rsi[i];
    let d = &panel.dmi[i];
    let l2 = Line::from(vec![
        dim("RSI ".to_string()),
        colored(
            num(rsi),
            if rsi.is_finite() {
                rsi_zone_color(rsi, &wp)
            } else {
                Color::DarkGray
            },
        ),
        dim("  MA ".to_string()),
        colored(num(panel.rsi_ma[i]), Color::Yellow),
        dim("  %B ".to_string()),
        colored(num(panel.mod_rsi[i]), Color::Blue),
        dim("  ADX ".to_string()),
        Span::raw(num(d.adx)),
        dim("  +DI ".to_string()),
        colored(num(d.plus_di), Color::Green),
        dim("  −DI ".to_string()),
        colored(num(d.minus_di), Color::Red),
        dim(format!(
            "   BB precio [{} … {}]",
            px_or_dash(panel.bb_lower[i]),
            px_or_dash(panel.bb_upper[i])
        )),
    ]);

    // checklist en vivo de los 5 filtros de cada lado sobre la última vela
    let cond = |label: String, ok: Option<bool>| -> Span<'static> {
        match ok {
            Some(true) => Span::styled(format!("{label}✓ "), Style::new().fg(Color::Green)),
            Some(false) => Span::styled(format!("{label}✗ "), Style::new().fg(Color::DarkGray)),
            None => Span::styled(format!("{label}? "), Style::new().fg(Color::DarkGray)),
        }
    };
    let fin = |v: f64, pred: bool| if v.is_finite() { Some(pred) } else { None };
    let (bb_lo, bb_up) = (panel.bb_lower[i], panel.bb_upper[i]);
    let l3 = Line::from(vec![
        Span::styled(
            "▲ long  ",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        cond("low≤BB".into(), fin(bb_lo, last.low <= bb_lo)),
        cond(
            format!("RSI<{:.0}", wp.rsi_max_long),
            fin(rsi, rsi < wp.rsi_max_long),
        ),
        cond(
            format!("+DI<{:.0}", wp.pdi_max_long),
            fin(d.plus_di, d.plus_di < wp.pdi_max_long),
        ),
        cond(
            format!("−DI>{:.0}", wp.mdi_min_long),
            fin(d.minus_di, d.minus_di > wp.mdi_min_long),
        ),
        cond(
            format!("ADX<{:.0}", wp.adx_max_long),
            fin(d.adx, d.adx < wp.adx_max_long),
        ),
    ]);
    let l4 = Line::from(vec![
        Span::styled(
            "▼ short ",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        cond("high≥BB".into(), fin(bb_up, last.high >= bb_up)),
        cond(
            format!("RSI>{:.0}", wp.rsi_min_short),
            fin(rsi, rsi > wp.rsi_min_short),
        ),
        cond(
            format!("+DI>{:.0}", wp.pdi_min_short),
            fin(d.plus_di, d.plus_di > wp.pdi_min_short),
        ),
        cond(
            format!("−DI<{:.0}", wp.mdi_max_short),
            fin(d.minus_di, d.minus_di < wp.mdi_max_short),
        ),
        cond(
            format!("ADX<{:.0}", wp.adx_max_short),
            fin(d.adx, d.adx < wp.adx_max_short),
        ),
    ]);

    f.render_widget(
        Paragraph::new(vec![Line::from(l1), l2, l3, l4]).block(Block::bordered().title(format!(
            " {} — whales+RSI (estimado, TA puro) ",
            p.meta.name
        ))),
        area,
    );
}

fn draw_chart(
    f: &mut Frame,
    e: &PairExtraData,
    gfx: &mut oscimg::Gfx,
    hover: Option<usize>,
    win: usize,
    area: Rect,
) {
    let wp = WhaleParams::default();
    let panel = &e.panel;
    let n = e.candles.len();
    // misma ventana visible que la Vista 2 (win velas), no una propia
    let start = n.saturating_sub(win);
    let mut block = Block::bordered().title(format!(
        " whales+RSI {} ×{} — RSI · MA · %B · ADX/±DI · ▲▼ ballena — i cambia TF ",
        e.interval.label(),
        n - start,
    ));
    let inner = block.inner(area);
    if inner.width < AXIS_W + 10 || inner.height < 4 {
        f.render_widget(block, area);
        return;
    }
    let chart = Rect::new(inner.x, inner.y, inner.width - AXIS_W, inner.height);
    let axis = Rect::new(chart.right(), inner.y, AXIS_W, inner.height);
    // win huecos repartidos por el ancho del panel (fraccionario, ver oscimg)
    let cols_per_pt = chart.width as f64 / win as f64;

    // hora + valores de la vela bajo el cursor en el borde inferior, como el
    // hover de OHLC de las velas de la Vista 2
    if let Some(i) = hover.map(|h| start + h).filter(|i| *i < n) {
        let d = &panel.dmi[i];
        let num = |v: f64| {
            if v.is_finite() {
                format!("{v:.0}")
            } else {
                "—".to_string()
            }
        };
        block = block.title_bottom(format!(
            " {} · RSI {} · MA {} · %B {} · ADX {} · +DI {} · −DI {} ",
            time_label(e.candles[i].t_close),
            num(panel.rsi[i]),
            num(panel.rsi_ma[i]),
            num(panel.mod_rsi[i]),
            num(d.adx),
            num(d.plus_di),
            num(d.minus_di),
        ));
    }
    f.render_widget(block, area);

    let adx: Vec<f64> = panel.dmi.iter().map(|d| d.adx).collect();
    let pdi: Vec<f64> = panel.dmi.iter().map(|d| d.plus_di).collect();
    let mdi: Vec<f64> = panel.dmi.iter().map(|d| d.minus_di).collect();

    // imagen real compartida con el sub-panel de la Vista 2 (oscimg): niveles,
    // columnas de ballena y marcas ▲▼ van dentro del raster. ADX/DMI debajo,
    // %B y MA encima, RSI al final (queda por encima de todo), como el Pine.
    let mut lines = vec![
        OscLine {
            vals: &adx,
            width: 1,
            color: LineColor::Fixed(oscimg::GRAY),
        },
        OscLine {
            vals: &pdi,
            width: 1,
            color: LineColor::Fixed(oscimg::GREEN),
        },
        OscLine {
            vals: &mdi,
            width: 1,
            color: LineColor::Fixed(oscimg::RED),
        },
    ];
    if let Some((up, lo)) = &panel.rsi_bb {
        lines.push(OscLine {
            vals: up,
            width: 1,
            color: LineColor::Fixed(oscimg::DIM_GREEN),
        });
        lines.push(OscLine {
            vals: lo,
            width: 1,
            color: LineColor::Fixed(oscimg::DIM_GREEN),
        });
    }
    let zone = |v: f64| oscimg::rsi_zone_rgb(v, &wp);
    lines.push(OscLine {
        vals: &panel.mod_rsi,
        width: 1,
        color: LineColor::Fixed(oscimg::BLUE),
    });
    lines.push(OscLine {
        vals: &panel.rsi_ma,
        width: 1,
        color: LineColor::Fixed(oscimg::YELLOW),
    });
    lines.push(OscLine {
        vals: &panel.rsi,
        width: 2,
        color: LineColor::ByValue(&zone),
    });
    let bars = panel
        .triggers
        .iter()
        .map(|t| {
            let col = match t.side {
                WhaleSide::Buy => oscimg::BAR_BUY,
                WhaleSide::Sell => oscimg::BAR_SELL,
            };
            (t.idx, t.height, col)
        })
        .collect();
    let marks = panel
        .triggers
        .iter()
        .map(|t| (t.idx, matches!(t.side, WhaleSide::Buy)))
        .collect();
    let spec = OscSpec {
        start,
        len: n - start,
        cols_per_pt,
        half_cols: cols_per_pt / 2.0,
        oversold: wp.oversold,
        overbought: wp.overbought,
        lines,
        bars,
        marks,
    };
    oscimg::draw_into(f, chart, gfx, oscimg::OscSlot::WhaleRsi, e.stamp, spec);

    // eje 0-100 con los niveles y el RSI actual resaltado
    let h = axis.height as usize;
    let row_of = |v: f64| -> usize {
        (((100.0 - v) / 100.0 * h as f64) - 0.5)
            .round()
            .clamp(0.0, h as f64 - 1.0) as usize
    };
    let mut labels: Vec<Line> = vec![Line::raw(""); h];
    for (v, c) in [
        (100.0, Color::DarkGray),
        (wp.overbought, Color::Rgb(150, 70, 75)),
        (50.0, Color::DarkGray),
        (wp.oversold, Color::Rgb(60, 130, 80)),
        (0.0, Color::DarkGray),
    ] {
        labels[row_of(v)] = Line::from(Span::styled(format!("{v:.0}"), Style::new().fg(c)));
    }
    if let Some(r) = panel.last_rsi() {
        labels[row_of(r)] = Line::from(Span::styled(
            format!("▶{r:.0}"),
            Style::new()
                .fg(rsi_zone_color(r, &wp))
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(labels), axis);
}

fn draw_log(f: &mut Frame, e: &PairExtraData, area: Rect) {
    let trig = &e.panel.triggers;
    let block = Block::bordered().title(format!(
        " Disparos ballena — {} en {} velas ",
        trig.len(),
        e.candles.len()
    ));
    let dim = |s: &'static str| Span::styled(s, Style::new().fg(Color::DarkGray));
    let mut lines: Vec<Line> = Vec::new();
    if trig.is_empty() {
        lines.push(Line::from(dim("sin disparos en las velas cargadas")));
        lines.push(Line::raw(""));
        lines.push(Line::from(dim("condiciones (ver checklist arriba):")));
        lines.push(Line::from(dim("vela fuera del BB de precio + RSI")));
        lines.push(Line::from(dim("extremo + DI contrario + ADX bajo")));
    } else {
        lines.push(Line::from(dim(" lado     int  fuera%  cierre      cuándo")));
        let max_rows = (area.height.saturating_sub(3)) as usize;
        for t in trig.iter().rev().take(max_rows.max(1)) {
            let c = &e.candles[t.idx];
            let (arrow, side_txt, color) = match t.side {
                WhaleSide::Buy => ("▲", "compra", Color::Green),
                WhaleSide::Sell => ("▼", "venta ", Color::Red),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {arrow} {side_txt} "),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:>4.1} ", t.height)),
                Span::styled(format!("{:>6.2}% ", t.dist_pct), Style::new().fg(color)),
                Span::raw(format!("{:<10} ", fmt_px(c.close))),
                Span::styled(age_label(c.t_close), Style::new().fg(Color::DarkGray)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}
