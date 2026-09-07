//! Vista 6 — Flujo de Dinero / Posicionamiento. Arriba: ranking de rotación
//! de capital (ΔOI notional cross-pair, ventanas 1h/4h/24h). Abajo: panel de
//! posicionamiento del par seleccionado (funding percentil, premium sostenido,
//! skew de whales, asimetría de liquidaciones ±3%, CVD). Solo lectura; el
//! score compuesto es un ranking de sobrecarga para revisión humana, no una
//! señal de entrada.

use ratatui::prelude::*;
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::app::{App, FlowWin, PairState, FLOW_CVD_WIN, FLOW_PREM_WIN};
use crate::flow::{
    self, Activity, CvdSignal, FUNDING_PCTL_EXTREME, LIQ_ASYM_EXTREME, LIQ_RATIO_EXTREME,
    PREMIUM_EXTREME_BPS, WHALE_LONG_EXTREME,
};

use super::fmt::{fmt_opt_pct, fmt_usd};
use super::theme;

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
        Activity::Conviccion => Style::new()
            .fg(theme::c().positive)
            .add_modifier(Modifier::BOLD),
        Activity::Silenciosa => Style::new().fg(theme::c().accent_amber),
        Activity::Normal => Style::new().fg(theme::c().neutral),
        Activity::Salida => Style::new().fg(theme::c().accent_magenta),
        Activity::Flat => Style::new().fg(theme::c().muted),
    }
}

fn score_cell(s: flow::Score) -> Cell<'static> {
    if s.avail == 0 {
        return Cell::from("—").style(Style::new().fg(theme::c().muted));
    }
    // intensidad por MARGEN sobre los componentes disponibles: "▼3 ▲0 /3"
    // se lee más fuerte que "▼2 ▲1 /3", que es justo la diferencia útil
    let color = if s.bear == s.bull {
        theme::c().muted
    } else {
        theme::bias_fg((s.bull as f64 - s.bear as f64) / s.avail.max(1) as f64)
    };
    Cell::from(format!("▼{} ▲{} /{}", s.bear, s.bull, s.avail)).style(Style::new().fg(color))
}

/// Celda de asimetría de combustible ±3%: reparto abajo/arriba en % del total
/// ("▼64/36" = 64% del combustible por debajo → sesgo bajista).
fn fuel_cell(asym: Option<f64>) -> Cell<'static> {
    let Some(a) = asym else {
        return Cell::from("—").style(Style::new().fg(theme::c().muted));
    };
    let below = (a + 1.0) * 50.0;
    let txt = format!("{below:.0}/{:.0}", 100.0 - below);
    if a >= LIQ_ASYM_EXTREME {
        Cell::from(format!("▼{txt}")).style(Style::new().fg(theme::c().negative))
    } else if a <= -LIQ_ASYM_EXTREME {
        Cell::from(format!("▲{txt}")).style(Style::new().fg(theme::c().positive))
    } else {
        Cell::from(txt).style(Style::new().fg(theme::c().neutral))
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
            Style::new()
                .fg(theme::c().accent_cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new()
                .fg(theme::c().neutral)
                .add_modifier(Modifier::BOLD)
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
                Cell::from(format!("{:>3}", i + 1)).style(Style::new().fg(theme::c().muted)),
                Cell::from(name.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
            ];
            for w in FlowWin::ALL {
                let usd = p.oi_delta_usd(w.dur());
                let pct = p.oi_delta_pct_slow(w.dur());
                cells.push(
                    Cell::from(fmt_usd_signed(usd))
                        .style(Style::new().fg(theme::sign_color(usd, false))),
                );
                cells.push(
                    Cell::from(fmt_opt_pct(pct, 1))
                        .style(Style::new().fg(theme::sign_color(pct, false))),
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
            // el % de whales largas se pinta con INTENSIDAD, no con tres
            // escalones: 50% es neutro y el tono se satura hacia el verde de
            // marca o el coral según se aleja (los extremos siguen marcados
            // en negrita, que es lo que ya llamaba la atención)
            let wstyle = match whale {
                Some(l) => {
                    let st = Style::new().fg(theme::bias_fg((l - 50.0) / 50.0));
                    if l >= WHALE_LONG_EXTREME || l <= 100.0 - WHALE_LONG_EXTREME {
                        st.add_modifier(Modifier::BOLD)
                    } else {
                        st
                    }
                }
                None => Style::new().fg(theme::c().muted),
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
        .block(theme::block().title(title))
        .row_highlight_style(
            Style::new()
                .bg(theme::c().selection_bg)
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
    Span::styled(s, Style::new().fg(theme::c().muted))
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
            Paragraph::new(tr.fl_hint_pin).block(theme::block().title(tr.fl_positioning)),
            area,
        );
        return;
    };
    let coin = p.meta.name.clone();
    let block = theme::block().title(format!(
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
            Style::new().fg(theme::sign_color(p.funding_hourly_pct(), true)),
        ),
        dim("/h · APR ".into()),
        Span::styled(
            fmt_opt_pct(p.funding_apr_pct(), 1),
            Style::new().fg(theme::sign_color(p.funding_apr_pct(), true)),
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
                spans.push(flag(tr.fl_crowd_long.into(), theme::c().negative));
            } else if pc <= 100.0 - FUNDING_PCTL_EXTREME {
                spans.push(flag(tr.fl_crowd_short.into(), theme::c().positive));
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
            Style::new().fg(theme::sign_color(now, true)),
        ),
        dim(format!(" {} ", tr.fl_sustained_1h)),
        Span::styled(
            match mean {
                Some(b) => format!("{b:+.1}bp"),
                None => tr.fl_accumulating.into(),
            },
            Style::new().fg(theme::sign_color(mean, true)),
        ),
    ];
    match mean {
        Some(b) if b >= PREMIUM_EXTREME_BPS => {
            spans.push(flag(tr.fl_buy_pressure.into(), theme::c().negative))
        }
        Some(b) if b <= -PREMIUM_EXTREME_BPS => {
            spans.push(flag(tr.fl_sell_pressure.into(), theme::c().positive))
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
            spans.push(Span::styled(
                fmt_usd(l),
                Style::new().fg(theme::c().positive),
            ));
            spans.push(dim(tr.fl_long_sep.into()));
            spans.push(Span::styled(
                fmt_usd(s),
                Style::new().fg(theme::c().negative),
            ));
            spans.push(dim(tr.fl_short_sep.into()));
            spans.push(Span::styled(
                format!("L{pct:.0}%"),
                Style::new().add_modifier(Modifier::BOLD),
            ));
            // la señal estrella: crowd (funding) y whales en lados opuestos
            if let Some(fp) = p.funding_percentile() {
                if fp >= FUNDING_PCTL_EXTREME && pct < 50.0 {
                    spans.push(flag(tr.fl_contra_bear.into(), theme::c().negative));
                } else if fp <= 100.0 - FUNDING_PCTL_EXTREME && pct > 50.0 {
                    spans.push(flag(tr.fl_contra_bull.into(), theme::c().positive));
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
                Style::new().fg(theme::c().accent_magenta),
            ));
            spans.push(dim(format!(" {} ", tr.fl_fuel_above)));
            spans.push(Span::styled(
                fmt_usd(above),
                Style::new().fg(theme::c().accent_cyan),
            ));
            if below > 0.0 && below >= above * LIQ_RATIO_EXTREME {
                spans.push(flag(tr.fl_least_res_below.into(), theme::c().negative));
            } else if above > 0.0 && above >= below * LIQ_RATIO_EXTREME {
                spans.push(flag(tr.fl_least_res_above.into(), theme::c().positive));
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
                Style::new().fg(theme::sign_color(Some(st.cum), false)),
            ));
            spans.push(dim(format!(" {} {mins}m", tr.fl_since_ago)));
            match app.cvd_window(FLOW_CVD_WIN) {
                Some((delta, px)) => {
                    spans.push(dim(" · Δ15m ".into()));
                    spans.push(Span::styled(
                        fmt_usd_signed(Some(delta)),
                        Style::new().fg(theme::sign_color(Some(delta), false)),
                    ));
                    spans.push(dim(format!(" {} {px:+.2}% → ", tr.fl_with_px)));
                    match app.cvd_signal() {
                        Some(CvdSignal::AbsorcionCompras) => spans.push(Span::styled(
                            CvdSignal::AbsorcionCompras.label(),
                            Style::new()
                                .fg(theme::c().negative)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Some(CvdSignal::AbsorcionVentas) => spans.push(Span::styled(
                            CvdSignal::AbsorcionVentas.label(),
                            Style::new()
                                .fg(theme::c().positive)
                                .add_modifier(Modifier::BOLD),
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
    let margen = (s.bull as f64 - s.bear as f64) / s.avail.max(1) as f64;
    spans.push(Span::styled(
        format!("▼{}", s.bear),
        Style::new()
            .fg(theme::bias_fg(-margen.abs().min(1.0)))
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("▲{}", s.bull),
        Style::new()
            .fg(theme::bias_fg(margen.abs().min(1.0)))
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(dim(format!(
        " {}",
        tr.fl_of_components.replacen("{}", &s.avail.to_string(), 1)
    )));
    if s.bear >= 3 && s.bear > s.bull {
        spans.push(flag(tr.fl_boat_longs.into(), theme::c().negative));
    } else if s.bull >= 3 && s.bull > s.bear {
        spans.push(flag(tr.fl_boat_shorts.into(), theme::c().positive));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    use crate::app::{App, View};
    use crate::data::types::{CtxSnapshot, DataMsg, PairMeta, PosInfo, WhaleInfo};
    use std::time::Instant;

    use super::theme;

    fn app_con_datos() -> App {
        std::env::set_var("CHART_PROTO", "halfblocks");
        let (extra_tx, _extra) = tokio::sync::mpsc::channel(8);
        let (wallet_tx, _w) = tokio::sync::watch::channel(Vec::new());
        let (usdc_tx, _u) = tokio::sync::watch::channel(None);
        let (coin_tx, _c) = tokio::sync::watch::channel(None);
        let (wc_tx, _wc) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            extra_tx,
            wallet_tx,
            usdc_tx,
            coin_tx,
            wc_tx,
            "test",
            crate::ui::oscimg::Gfx::new(),
        );
        let ctxs: Vec<(PairMeta, CtxSnapshot)> = ["BTC", "ETH", "SOL"]
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let px = 1_000.0 * (i as f64 + 1.0);
                (
                    PairMeta {
                        name: (*name).into(),
                        sz_decimals: 4,
                        max_leverage: 20,
                    },
                    CtxSnapshot {
                        t: Instant::now(),
                        t_ms: 0,
                        mark_px: px,
                        mid_px: Some(px),
                        oracle_px: px,
                        funding: 0.0001 * (i as f64 - 1.0),
                        open_interest: 1_000.0,
                        premium: Some(0.0002),
                        day_ntl_vlm: 1e8,
                        prev_day_px: px * 0.98,
                    },
                )
            })
            .collect();
        app.apply_msg(DataMsg::Ctxs(ctxs));
        app.selected_coin = Some("BTC".into());
        // whales con los dos lados, para que el sesgo y el skew tengan tono
        app.whales = ["BTC", "ETH"]
            .iter()
            .enumerate()
            .map(|(i, coin)| WhaleInfo {
                addr: format!("0xw{i}"),
                account_value: 1e7,
                positions: vec![PosInfo {
                    coin: (*coin).into(),
                    szi: if i == 0 { 5.0 } else { -3.0 },
                    entry_px: Some(1_000.0),
                    position_value: 1e6 * (i as f64 + 1.0),
                    unrealized_pnl: if i == 0 { 1_000.0 } else { -500.0 },
                    roe: 0.1,
                    leverage: 5,
                    is_cross: true,
                    liq_px: Some(900.0),
                    since_open_funding: 0.0,
                }],
            })
            .collect();
        app
    }

    /// Las Vistas 6, 7 y 9 ya no pintan con la paleta ANSI del terminal: todo
    /// su cuerpo sale de `theme.rs`, el marco sigue al tema activo y el
    /// toggle claro/oscuro las repinta.
    #[test]
    fn vistas_6_7_9_pintan_con_el_tema() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();

        for view in [View::Flow, View::Whales, View::Wallet] {
            app.view = view;
            for th in [theme::Theme::Dark, theme::Theme::Light] {
                theme::set_theme(th);
                term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer().clone();

                // el cuerpo de la vista es rows[1] del layout: sin la
                // cabecera ni el pie, que son chrome común y aún no migrados
                let mut vistos = 0;
                for y in 1..buf.area.height - 1 {
                    for x in 0..buf.area.width {
                        let cell = buf.cell((x, y)).unwrap();
                        for col in [cell.fg, cell.bg] {
                            match col {
                                Color::Rgb(..) | Color::Reset => vistos += 1,
                                otro => panic!(
                                    "color ANSI suelto en {view:?} ({x},{y}): {otro:?} \
                                     — debe salir de theme.rs"
                                ),
                            }
                        }
                    }
                }
                assert!(vistos > 0, "la vista {view:?} no pintó nada");

                // el marco es el del tema activo
                let borde = buf.cell((0, 1)).unwrap().fg;
                let esperado = match th {
                    theme::Theme::Dark => theme::DARK.border,
                    theme::Theme::Light => theme::LIGHT.border,
                };
                assert_eq!(borde, esperado, "marco de {view:?} con el tema {th:?}");
            }
        }
        theme::set_theme(theme::Theme::Dark);
    }
}
