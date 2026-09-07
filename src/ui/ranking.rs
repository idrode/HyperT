use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

use crate::app::{App, SortCol, OI_WIN_LONG, OI_WIN_SHORT};

use super::fmt::{fmt_opt, fmt_opt_pct, fmt_px, fmt_usd};
use super::theme;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // con el buscador abierto la tabla muestra los resultados filtrados y el
    // cursor sigue la selección del buscador
    let searching = app.search.active;
    let coins = if searching {
        app.search_results()
    } else {
        app.sorted_coins()
    };

    let marker = if app.sort_desc { "▼" } else { "▲" };
    let hdr = |label: &str, col: Option<SortCol>| -> Cell<'static> {
        let active = col == Some(app.sort);
        let text = if active {
            format!("{label}{marker}")
        } else {
            label.to_string()
        };
        Cell::from(text).style(theme::header_style(active))
    };
    let s = crate::i18n::t();
    let header = Row::new(vec![
        hdr("#", None),
        hdr(s.rk_col_pair, Some(SortCol::Coin)),
        hdr(s.rk_col_price, Some(SortCol::Px)),
        hdr("24h%", Some(SortCol::Chg24)),
        hdr("F/h%", None),
        hdr("F APR%", Some(SortCol::FundApr)),
        hdr("Prem bp", Some(SortCol::Premium)),
        hdr("OI $", Some(SortCol::OiNotional)),
        hdr("ΔOI 5m", Some(SortCol::OiD5m)),
        hdr("ΔOI 1h", Some(SortCol::OiD1h)),
        hdr(s.rk_col_flow, None),
        hdr("Vol 24h", Some(SortCol::Vol24)),
    ]);

    let rows: Vec<Row> = coins
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let p = app.pairs.get(name)?;
            let chg = p.chg24_pct();
            let f_h = p.funding_hourly_pct();
            let apr = p.funding_apr_pct();
            let prem = p.premium_bps();
            let d5 = p.oi_delta_pct(OI_WIN_SHORT);
            let d1h = p.oi_delta_pct(OI_WIN_LONG);
            let reg = p.regime(OI_WIN_LONG);
            let sign = |v: Option<f64>, invert: bool| Style::new().fg(theme::sign_color(v, invert));
            Some(Row::new(vec![
                Cell::from(format!("{:>3}", i + 1)).style(Style::new().fg(theme::c().muted)),
                Cell::from(name.clone()).style(
                    Style::new()
                        .fg(theme::c().fg_strong)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(fmt_px(p.mid)).style(Style::new().fg(theme::c().fg)),
                Cell::from(fmt_opt_pct(chg, 2)).style(sign(chg, false)),
                Cell::from(fmt_opt(f_h, 4)).style(sign(f_h, true)),
                Cell::from(fmt_opt_pct(apr, 1)).style(sign(apr, true)),
                Cell::from(fmt_opt(prem, 1)).style(sign(prem, true)),
                Cell::from(fmt_usd(p.oi_notional())).style(Style::new().fg(theme::c().fg)),
                Cell::from(fmt_opt_pct(d5, 2)).style(sign(d5, false)),
                Cell::from(fmt_opt_pct(d1h, 2)).style(sign(d1h, false)),
                Cell::from(reg.label()).style(Style::new().fg(theme::regime_color(reg))),
                Cell::from(fmt_usd(p.volume24())).style(Style::new().fg(theme::c().fg)),
            ]))
        })
        .collect();

    let title = if searching {
        format!(
            " {} — {} «{}» ({} {} {}) ",
            s.rk_title,
            s.t_filter,
            app.search.query,
            coins.len(),
            s.t_of,
            app.pairs.len()
        )
    } else {
        format!(
            " {} ({} perps) — {}: {} ",
            s.rk_title,
            coins.len(),
            s.t_sort,
            app.sort.label()
        )
    };
    let widths = [
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Length(11),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(15),
        Constraint::Length(8),
    ];
    // marco del tema: el aspecto (grueso / doble / bloques / fino) se elige en
    // theme::BORDER_LOOK, no aquí
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::border_set())
        .border_style(theme::border_style())
        .title(Span::styled(title, theme::title_style()))
        .style(theme::base());
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .style(theme::base())
        .row_highlight_style(theme::row_highlight())
        .highlight_symbol(Span::styled("▶", Style::new().fg(theme::c().cursor)));

    let hi = if searching { app.search.sel } else { app.sel };
    app.table_state
        .select(Some(hi.min(coins.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut app.table_state);
}
