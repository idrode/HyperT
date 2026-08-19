//! Vista 6 — Flujo de Dinero / Posicionamiento. Arriba: ranking de rotación
//! de capital (ΔOI notional cross-pair, ventanas 1h/4h/24h). Abajo: panel de
//! posicionamiento del par seleccionado (funding percentil, premium sostenido,
//! skew de whales, asimetría de liquidaciones ±3%, CVD). Solo lectura; el
//! score compuesto es un ranking de sobrecarga para revisión humana, no una
//! señal de entrada.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table};

use crate::app::{App, FlowWin, PairState, FLOW_CVD_WIN, FLOW_PREM_WIN};
use crate::flow::{
    self, Activity, CvdSignal, FUNDING_PCTL_EXTREME, LIQ_ASYM_EXTREME, LIQ_RATIO_EXTREME,
    PREMIUM_EXTREME_BPS, WHALE_LONG_EXTREME,
};

use super::fmt::{fmt_opt_pct, fmt_usd, sign_color};

/// Altura del panel de posicionamiento (6 líneas + bordes).
const PANEL_H: u16 = 9;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(8), Constraint::Length(PANEL_H)]).split(area);
    draw_rotation(f, app, rows[0]);
    draw_positioning(f, app, rows[1]);
}

fn fmt_usd_signed(v: Option<f64>) -> String {
    match v {
        Some(x) if x > 0.0 => format!("+{}", fmt_usd(x)),
        Some(x) => fmt_usd(x),
        None => "—".to_string(),
    }
}

fn activity_style(a: Activity) -> Style {
    match a {
        Activity::Conviccion => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        Activity::Silenciosa => Style::new().fg(Color::Yellow),
        Activity::Normal => Style::new().fg(Color::Gray),
        Activity::Salida => Style::new().fg(Color::Magenta),
        Activity::Flat => Style::new().fg(Color::DarkGray),
    }
}

fn score_cell(s: flow::Score) -> Cell<'static> {
    if s.avail == 0 {
        return Cell::from("—").style(Style::new().fg(Color::DarkGray));
    }
    let color = match s.bear.cmp(&s.bull) {
        std::cmp::Ordering::Greater => Color::Red,
        std::cmp::Ordering::Less => Color::Green,
        std::cmp::Ordering::Equal => Color::DarkGray,
    };
    Cell::from(format!("▼{} ▲{} /{}", s.bear, s.bull, s.avail)).style(Style::new().fg(color))
}

/// Celda de asimetría de combustible ±3%: reparto abajo/arriba en % del total
/// ("▼64/36" = 64% del combustible por debajo → sesgo bajista).
fn fuel_cell(asym: Option<f64>) -> Cell<'static> {
    let Some(a) = asym else {
        return Cell::from("—").style(Style::new().fg(Color::DarkGray));
    };
    let below = (a + 1.0) * 50.0;
    let txt = format!("{below:.0}/{:.0}", 100.0 - below);
    if a >= LIQ_ASYM_EXTREME {
        Cell::from(format!("▼{txt}")).style(Style::new().fg(Color::Red))
    } else if a <= -LIQ_ASYM_EXTREME {
        Cell::from(format!("▲{txt}")).style(Style::new().fg(Color::Green))
    } else {
        Cell::from(txt).style(Style::new().fg(Color::Gray))
    }
}

fn draw_rotation(f: &mut Frame, app: &mut App, area: Rect) {
    let searching = app.search.active;
    let coins = if searching {
        app.search_results()
    } else {
        app.flow_coins()
    };
    let active = app.flow_win;

    let hdr = |label: String, win: Option<FlowWin>| -> Cell<'static> {
        let style = if win == Some(active) {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD)
        };
        Cell::from(label).style(style)
    };
    let tr = crate::i18n::t();
    let mut header = vec![hdr("#".into(), None), hdr(tr.rk_col_pair.into(), None)];
    for w in FlowWin::ALL {
        header.push(hdr(format!("ΔOI$ {}", w.label()), Some(w)));
        header.push(hdr(format!("%OI {}", w.label()), Some(w)));
    }
    header.extend([
        hdr(format!("Vol× {}", active.label()), Some(active)),
        hdr(tr.fl_col_character.into(), Some(active)),
        hdr(tr.fl_col_whale_l.into(), None),
        hdr("Liq±3%".into(), None),
        hdr("Score".into(), None),
    ]);

    let rows: Vec<Row> = coins
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let p = app.pairs.get(name)?;
            let mut cells = vec![
                Cell::from(format!("{:>3}", i + 1)).style(Style::new().fg(Color::DarkGray)),
                Cell::from(name.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
            ];
            for w in FlowWin::ALL {
                let usd = p.oi_delta_usd(w.dur());
                let pct = p.oi_delta_pct_slow(w.dur());
                cells.push(
                    Cell::from(fmt_usd_signed(usd)).style(Style::new().fg(sign_color(usd, false))),
                );
                cells.push(
                    Cell::from(fmt_opt_pct(pct, 1)).style(Style::new().fg(sign_color(pct, false))),
                );
            }
            let ratio = p.window_vol_ratio(active.dur());
            let act = p
                .oi_delta_pct_slow(active.dur())
                .map(|d| flow::classify_activity(d, ratio))
                .unwrap_or(Activity::Flat);
            cells.push(Cell::from(
                ratio.map(|r| format!("{r:.1}×")).unwrap_or("—".into()),
            ));
            cells.push(Cell::from(act.label()).style(activity_style(act)));
            let whale = app
                .whale_ntl_for(name)
                .filter(|(l, s)| l + s > 0.0)
                .map(|(l, s)| l / (l + s) * 100.0);
            let wstyle = match whale {
                Some(l) if l >= WHALE_LONG_EXTREME => Style::new().fg(Color::Green),
                Some(l) if l <= 100.0 - WHALE_LONG_EXTREME => Style::new().fg(Color::Red),
                Some(_) => Style::new().fg(Color::Gray),
                None => Style::new().fg(Color::DarkGray),
            };
            cells.push(
                Cell::from(whale.map(|l| format!("{l:.0}%")).unwrap_or("—".into())).style(wstyle),
            );
            cells.push(fuel_cell(app.liq_asym_for(name)));
            cells.push(score_cell(flow::score(&app.score_inputs(name))));
            Some(Row::new(cells))
        })
        .collect();

    let title = if searching {
        format!(
            "{}— {} «{}» ({} {} {}) ",
            tr.fl_title,
            tr.t_filter,
            app.search.query,
            coins.len(),
            tr.t_of,
            app.pairs.len()
        )
    } else {
        let marker = if app.flow_desc { "▼" } else { "▲" };
        format!(
            "{}— {}: {} {marker} · {} {} ",
            tr.fl_title,
            tr.t_sort,
            app.flow_sort.label(),
            tr.t_window,
            active.label()
        )
    };
    let widths = [
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(11),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(9),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(header))
        .block(Block::bordered().title(title))
        .row_highlight_style(
            Style::new()
                .bg(Color::Rgb(40, 44, 66))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶");
    let hi = if searching {
        app.search.sel
    } else {
        app.flow_sel
    };
    app.flow_state
        .select(Some(hi.min(coins.len().saturating_sub(1))));
    f.render_stateful_widget(table, area, &mut app.flow_state);
}

fn dim(s: String) -> Span<'static> {
    Span::styled(s, Style::new().fg(Color::DarkGray))
}

fn flag(s: String, color: Color) -> Span<'static> {
    Span::styled(
        format!("  {s}"),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn draw_positioning(f: &mut Frame, app: &App, area: Rect) {
    let tr = crate::i18n::t();
    let Some(p) = app.selected_pair() else {
        f.render_widget(
            Paragraph::new(tr.fl_hint_pin).block(Block::bordered().title(tr.fl_positioning)),
            area,
        );
        return;
    };
    let coin = p.meta.name.clone();
    let block = Block::bordered().title(format!(
        "{}— {coin} · {} ",
        tr.fl_positioning, tr.fl_pos_subtitle
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines = vec![
        funding_line(p),
        premium_line(p),
        whales_line(app, p, &coin),
        liq_line(app, &coin),
        cvd_line(app),
        score_line(app, &coin),
    ];
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

fn funding_line(p: &PairState) -> Line<'static> {
    let tr = crate::i18n::t();
    let mut spans = vec![
        dim(format!("{:<10}", tr.fl_funding)),
        Span::styled(
            fmt_opt_pct(p.funding_hourly_pct(), 4),
            Style::new().fg(sign_color(p.funding_hourly_pct(), true)),
        ),
        dim("/h · APR ".into()),
        Span::styled(
            fmt_opt_pct(p.funding_apr_pct(), 1),
            Style::new().fg(sign_color(p.funding_apr_pct(), true)),
        ),
    ];
    match p.funding_percentile() {
        Some(pc) => {
            spans.push(dim(format!(" {} ", tr.fl_percentile_vs)));
            spans.push(Span::styled(
                format!("p{pc:.0}"),
                Style::new().add_modifier(Modifier::BOLD),
            ));
            if let Some(e) = &p.extra {
                if let (Some(first), Some(last)) = (e.funding_hist.first(), e.funding_hist.last()) {
                    let days = (last.0.saturating_sub(first.0)) as f64 / 86_400_000.0;
                    spans.push(dim(format!(" ({days:.0}d)")));
                }
            }
            if pc >= FUNDING_PCTL_EXTREME {
                spans.push(flag(tr.fl_crowd_long.into(), Color::Red));
            } else if pc <= 100.0 - FUNDING_PCTL_EXTREME {
                spans.push(flag(tr.fl_crowd_short.into(), Color::Green));
            }
        }
        None => spans.push(dim(format!(" {}", tr.fl_percentile_none))),
    }
    Line::from(spans)
}

fn premium_line(p: &PairState) -> Line<'static> {
    let now = p.premium_bps();
    let mean = p.premium_mean_bps(FLOW_PREM_WIN);
    let tr = crate::i18n::t();
    let mut spans = vec![
        dim(format!("{:<10}", tr.fl_premium)),
        Span::styled(
            match now {
                Some(b) => format!("{b:+.1}bp"),
                None => "—".into(),
            },
            Style::new().fg(sign_color(now, true)),
        ),
        dim(format!(" {} ", tr.fl_sustained_1h)),
        Span::styled(
            match mean {
                Some(b) => format!("{b:+.1}bp"),
                None => tr.fl_accumulating.into(),
            },
            Style::new().fg(sign_color(mean, true)),
        ),
    ];
    match mean {
        Some(b) if b >= PREMIUM_EXTREME_BPS => {
            spans.push(flag(tr.fl_buy_pressure.into(), Color::Red))
        }
        Some(b) if b <= -PREMIUM_EXTREME_BPS => {
            spans.push(flag(tr.fl_sell_pressure.into(), Color::Green))
        }
        _ => {}
    }
    Line::from(spans)
}

fn whales_line(app: &App, p: &PairState, coin: &str) -> Line<'static> {
    let tr = crate::i18n::t();
    let mut spans = vec![dim(format!("{:<10}", tr.fl_whales))];
    match app.whale_ntl_for(coin).filter(|(l, s)| l + s > 0.0) {
        Some((l, s)) => {
            let pct = l / (l + s) * 100.0;
            spans.push(Span::styled(fmt_usd(l), Style::new().fg(Color::Green)));
            spans.push(dim(tr.fl_long_sep.into()));
            spans.push(Span::styled(fmt_usd(s), Style::new().fg(Color::Red)));
            spans.push(dim(tr.fl_short_sep.into()));
            spans.push(Span::styled(
                format!("L{pct:.0}%"),
                Style::new().add_modifier(Modifier::BOLD),
            ));
            // la señal estrella: crowd (funding) y whales en lados opuestos
            if let Some(fp) = p.funding_percentile() {
                if fp >= FUNDING_PCTL_EXTREME && pct < 50.0 {
                    spans.push(flag(tr.fl_contra_bear.into(), Color::Red));
                } else if fp <= 100.0 - FUNDING_PCTL_EXTREME && pct > 50.0 {
                    spans.push(flag(tr.fl_contra_bull.into(), Color::Green));
                }
            }
        }
        None => spans.push(dim(tr.fl_no_whales.into())),
    }
    Line::from(spans)
}

fn liq_line(app: &App, coin: &str) -> Line<'static> {
    let tr = crate::i18n::t();
    let mut spans = vec![dim(format!("{:<10}", tr.fl_liqs))];
    match app.liq_fuel_for(coin) {
        Some((below, above)) => {
            spans.push(dim(format!("{} ", tr.fl_fuel_below)));
            spans.push(Span::styled(
                fmt_usd(below),
                Style::new().fg(Color::Magenta),
            ));
            spans.push(dim(format!(" {} ", tr.fl_fuel_above)));
            spans.push(Span::styled(fmt_usd(above), Style::new().fg(Color::Cyan)));
            if below > 0.0 && below >= above * LIQ_RATIO_EXTREME {
                spans.push(flag(tr.fl_least_res_below.into(), Color::Red));
            } else if above > 0.0 && above >= below * LIQ_RATIO_EXTREME {
                spans.push(flag(tr.fl_least_res_above.into(), Color::Green));
            }
            spans.push(dim(format!("  {}", tr.fl_estimated_v5)));
        }
        None => spans.push(dim(tr.fl_candles_loading.into())),
    }
    Line::from(spans)
}

fn cvd_line(app: &App) -> Line<'static> {
    let tr = crate::i18n::t();
    let mut spans = vec![dim(format!("{:<10}", "CVD"))];
    match &app.cvd {
        Some(st) => {
            let mins = st.since.elapsed().as_secs() / 60;
            spans.push(Span::styled(
                fmt_usd_signed(Some(st.cum)),
                Style::new().fg(sign_color(Some(st.cum), false)),
            ));
            spans.push(dim(format!(" {} {mins}m", tr.fl_since_ago)));
            match app.cvd_window(FLOW_CVD_WIN) {
                Some((delta, px)) => {
                    spans.push(dim(" · Δ15m ".into()));
                    spans.push(Span::styled(
                        fmt_usd_signed(Some(delta)),
                        Style::new().fg(sign_color(Some(delta), false)),
                    ));
                    spans.push(dim(format!(" {} {px:+.2}% → ", tr.fl_with_px)));
                    match app.cvd_signal() {
                        Some(CvdSignal::AbsorcionCompras) => spans.push(Span::styled(
                            CvdSignal::AbsorcionCompras.label(),
                            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
                        )),
                        Some(CvdSignal::AbsorcionVentas) => spans.push(Span::styled(
                            CvdSignal::AbsorcionVentas.label(),
                            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                        )),
                        _ => spans.push(dim(CvdSignal::Neutro.label().into())),
                    }
                }
                None => spans.push(dim(format!(" {}", tr.fl_diverg_need))),
            }
        }
        None => spans.push(dim(tr.fl_waiting_trades.into())),
    }
    Line::from(spans)
}

fn score_line(app: &App, coin: &str) -> Line<'static> {
    let tr = crate::i18n::t();
    let s = flow::score(&app.score_inputs(coin));
    let mut spans = vec![dim(format!("{:<10}", tr.fl_score))];
    if s.avail == 0 {
        spans.push(dim(tr.fl_no_components.into()));
        return Line::from(spans);
    }
    spans.push(Span::styled(
        format!("▼{}", s.bear),
        Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("▲{}", s.bull),
        Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
    ));
    spans.push(dim(format!(
        " {}",
        tr.fl_of_components.replacen("{}", &s.avail.to_string(), 1)
    )));
    if s.bear >= 3 && s.bear > s.bull {
        spans.push(flag(tr.fl_boat_longs.into(), Color::Red));
    } else if s.bull >= 3 && s.bull > s.bear {
        spans.push(flag(tr.fl_boat_shorts.into(), Color::Green));
    }
    Line::from(spans)
}
