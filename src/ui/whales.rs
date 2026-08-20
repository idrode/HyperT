use std::collections::HashMap;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table};

use crate::app::App;
use crate::data::types::PosInfo;

use super::fmt::{fmt_px, fmt_usd, sign_color};

fn short_addr(a: &str) -> String {
    if a.len() > 12 {
        format!("{}…{}", &a[..6], &a[a.len() - 4..])
    } else {
        a.to_string()
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // una línea extra en los modos de PnL para la cobertura del escaneo: el
    // ranking de PnL solo es legible sabiendo de cuántas cuentas se sabe algo
    let head_h = if app.whale_sort.is_pnl() { 5 } else { 4 };
    let rows_layout = Layout::vertical([Constraint::Length(head_h), Constraint::Min(3)]).split(area);

    draw_summary(f, app, rows_layout[0]);
    draw_table(f, app, rows_layout[1]);

    if app.whale_modal.is_some() {
        app.overlay_drawn.set(true);
        draw_addr_modal(f, app, area);
    }
}

/// Modal con la dirección completa de la whale seleccionada, para copiarla
/// (a mano o con `c` vía OSC 52) y pegarla en la Vista 9 (wallet watch-only).
fn draw_addr_modal(f: &mut Frame, app: &App, area: Rect) {
    let (addr, feedback) = match &app.whale_modal {
        Some(m) => m,
        None => return,
    };
    let s = crate::i18n::t();
    draw_addr_overlay(
        f,
        area,
        addr,
        feedback,
        s.wh_modal_title,
        s.wh_modal_copy_hint,
    );
}

/// Overlay reusable de "dirección completa": lo comparten el modal de whale
/// (Vista 7) y el de wallet relacionada (Vista 9), que solo cambian título y
/// pie de atajos.
pub(super) fn draw_addr_overlay(
    f: &mut Frame,
    area: Rect,
    addr: &str,
    feedback: &Option<String>,
    title: &str,
    hint: &str,
) {
    let w = (addr.len() as u16 + 6)
        .max(hint.len() as u16 + 4)
        .min(area.width);
    let h = 7u16.min(area.height);
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);

    let s = crate::i18n::t();
    let dim = Style::new().fg(Color::DarkGray);
    let mut lines = vec![
        Line::from(Span::styled(s.wh_modal_full_addr, dim)),
        Line::from(Span::styled(
            addr.to_string(),
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    match feedback {
        Some(msg) => lines.push(Line::from(Span::styled(
            msg.clone(),
            Style::new().fg(Color::Green),
        ))),
        None => lines.push(Line::from(Span::styled(hint.to_string(), dim))),
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(title.to_string())
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        r,
    );
}

fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let mut long_ntl = 0.0;
    let mut short_ntl = 0.0;
    let mut by_coin: HashMap<&str, (f64, f64)> = HashMap::new();
    for w in &app.whales {
        for p in &w.positions {
            let e = by_coin.entry(p.coin.as_str()).or_insert((0.0, 0.0));
            if p.szi >= 0.0 {
                long_ntl += p.position_value;
                e.0 += p.position_value;
            } else {
                short_ntl += p.position_value;
                e.1 += p.position_value;
            }
        }
    }
    let total = long_ntl + short_ntl;
    let bias = if total > 0.0 {
        (long_ntl - short_ntl) / total * 100.0
    } else {
        0.0
    };
    let mut top: Vec<(&str, (f64, f64))> = by_coin.into_iter().collect();
    top.sort_by(|a, b| {
        (b.1 .0 + b.1 .1)
            .partial_cmp(&(a.1 .0 + a.1 .1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
    let tr = crate::i18n::t();

    let mut concentr: Vec<Span> = vec![dim(tr.wh_by_pair.to_string())];
    for (coin, (l, s)) in top.iter().take(4) {
        let t = l + s;
        let lpct = if t > 0.0 { l / t * 100.0 } else { 0.0 };
        concentr.push(Span::styled(
            format!("{coin} "),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        concentr.push(Span::styled(
            format!("L{lpct:.0}%"),
            Style::new().fg(Color::Green),
        ));
        concentr.push(dim(format!("/{} · ", fmt_usd(t))));
    }

    let status = match (&app.whale_status, app.whales_at) {
        (_, Some(t)) => format!(
            "{} {} · {} {}s",
            app.whales.len(),
            tr.wh_accounts_pos,
            tr.wh_refresh_ago,
            t.elapsed().as_secs()
        ),
        (Some(s), None) => s.clone(),
        (None, None) => tr.wh_starting.to_string(),
    };

    let mut lines = vec![
        Line::from(vec![
            dim(format!("Σ {} ", tr.wh_long)),
            Span::styled(fmt_usd(long_ntl), Style::new().fg(Color::Green)),
            dim(format!("   Σ {} ", tr.wh_short)),
            Span::styled(fmt_usd(short_ntl), Style::new().fg(Color::Red)),
            dim(format!("   {} ", tr.wh_bias)),
            Span::styled(
                format!("{bias:+.1}% "),
                Style::new().fg(sign_color(Some(bias), false)),
            ),
            dim(format!("· {status}")),
        ]),
        Line::from(concentr),
    ];
    // Cobertura del escaneo: sin esto, ordenar por PnL invita a leer la tabla
    // como un ranking cerrado del top-100, cuando puede faltar gente.
    if app.whale_sort.is_pnl() {
        let scan = app.whale_scan.unwrap_or_default();
        let mut spans = vec![dim(format!(
            "{}/{} {}",
            scan.scanned, scan.total, tr.wh_scan_scanned
        ))];
        if !scan.complete() {
            spans.push(Span::styled(
                format!(" · {}", tr.wh_scan_partial),
                Style::new().fg(Color::Yellow),
            ));
        }
        if scan.failed > 0 {
            spans.push(Span::styled(
                format!(" · {} {}", scan.failed, tr.wh_scan_failed),
                Style::new().fg(Color::Yellow),
            ));
        }
        spans.push(dim(format!(" · {}", tr.wh_absent_note)));
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(tr.wh_title)),
        area,
    );
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    // filas aplanadas (whale, posición) en el orden del modo activo; el orden
    // lo decide `app::whale_row_order`, la misma fuente que usa la selección
    let order = app.whale_rows();
    let pnl_mode = app.whale_sort.is_pnl();
    let flat: Vec<(&str, f64, &PosInfo, Option<f64>, bool)> = order
        .iter()
        .enumerate()
        .map(|(row, &(wi, pi))| {
            let w = &app.whales[wi];
            // el Σ uPnL es de la CUENTA: se pinta solo en la primera fila de
            // cada whale, para que no parezca un valor por posición
            let first = row == 0 || order[row - 1].0 != wi;
            (
                w.addr.as_str(),
                w.account_value,
                &w.positions[pi],
                crate::app::whale_agg_pnl(w),
                first,
            )
        })
        .collect();

    let tr = crate::i18n::t();
    let header_style = Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD);
    let mut header_cells = vec![
        tr.wh_col_account,
        tr.wh_col_value,
        tr.rk_col_pair,
        tr.wh_col_side,
        "Ntl $",
        tr.wh_col_entry,
        "Liq",
        "Lev",
        "uPnL $",
        "ROE%",
    ];
    // la columna del agregado solo aparece en los modos que ordenan por él,
    // que son los únicos en que las filas van agrupadas por cuenta
    if pnl_mode {
        header_cells.push(tr.wh_col_agg_pnl);
    }
    let header = Row::new(header_cells).style(header_style);

    let rows: Vec<Row> = flat
        .iter()
        .map(|(addr, acct, p, agg, first_of_whale)| {
            let long = p.szi >= 0.0;
            let side = if long { "LONG" } else { "SHORT" };
            let side_color = if long { Color::Green } else { Color::Red };
            let lev = format!("{}×{}", p.leverage, if p.is_cross { "c" } else { "i" });
            let mut cells = vec![
                Cell::from(short_addr(addr)).style(Style::new().fg(Color::Cyan)),
                Cell::from(fmt_usd(*acct)),
                Cell::from(p.coin.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(side).style(Style::new().fg(side_color).add_modifier(Modifier::BOLD)),
                Cell::from(fmt_usd(p.position_value)),
                Cell::from(p.entry_px.map(fmt_px).unwrap_or_else(|| "—".into())),
                Cell::from(p.liq_px.map(fmt_px).unwrap_or_else(|| "—".into()))
                    .style(Style::new().fg(Color::Yellow)),
                Cell::from(lev),
                Cell::from(fmt_usd(p.unrealized_pnl))
                    .style(Style::new().fg(sign_color(Some(p.unrealized_pnl), false))),
                Cell::from(format!("{:+.1}", p.roe * 100.0))
                    .style(Style::new().fg(sign_color(Some(p.roe), false))),
            ];
            if pnl_mode {
                // "—" = sin dato (no debería darse en una whale listada);
                // un 0.00 pintado es un cero real, no un hueco.
                // El valor se repite en todas las filas de la cuenta —atenuado
                // en las de continuación— porque con la tabla desplazada la
                // primera fila del grupo puede quedar fuera de pantalla y el
                // dato desaparecería justo cuando se está mirando.
                let cell = match agg {
                    Some(v) if *first_of_whale => Cell::from(fmt_usd(*v))
                        .style(Style::new().fg(sign_color(Some(*v), false))),
                    Some(v) => Cell::from(fmt_usd(*v)).style(Style::new().fg(Color::DarkGray)),
                    None => Cell::from("—").style(Style::new().fg(Color::DarkGray)),
                };
                cells.push(cell);
            }
            Row::new(cells)
        })
        .collect();

    let n = rows.len();
    let mut widths = vec![
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(7),
    ];
    if pnl_mode {
        widths.push(Constraint::Length(10));
    }
    let title = format!(
        " {n} {} · {} ({}) ",
        tr.wh_positions_hint,
        app.whale_sort.label(),
        tr.wh_sort_hint
    );
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title))
        .row_highlight_style(
            Style::new()
                .bg(Color::Rgb(40, 44, 66))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶");
    app.whales_state.select(if n == 0 {
        None
    } else {
        Some(app.whale_sel.min(n - 1))
    });
    // Zona de datos (dentro del borde + fila de cabecera) para mapear clicks.
    let inner = area.inner(Margin::new(1, 1));
    app.whale_rows_area = Some(Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    ));
    f.render_stateful_widget(table, area, &mut app.whales_state);
}
