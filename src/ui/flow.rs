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

/// Ancho del widget del FOMC. Fijo: es una ficha de contexto, no debe crecer
/// a costa del panel de posicionamiento, que es el contenido real de la vista.
const FED_W: u16 = 30;
/// Por debajo de esto el panel de posicionamiento se quedaría ilegible, así
/// que el widget de contexto cede el sitio y no se dibuja.
const FED_MIN_REST: u16 = 60;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(8), Constraint::Length(PANEL_H)]).split(area);
    draw_rotation(f, app, rows[0]);

    // El widget del FOMC ocupa el hueco izquierdo de la fila inferior; el
    // panel de posicionamiento sigue a su derecha con el resto del ancho.
    if rows[1].width >= FED_W + FED_MIN_REST {
        let cols =
            Layout::horizontal([Constraint::Length(FED_W), Constraint::Min(0)]).split(rows[1]);
        draw_fed(f, app, cols[0]);
        draw_positioning(f, app, cols[1]);
    } else {
        draw_positioning(f, app, rows[1]);
    }
}

/// Widget de contexto macro: probabilidades de la próxima decisión del FOMC
/// según Polymarket.
///
/// Tres reglas deliberadas, en orden de importancia:
///
/// 1. La etiqueta **Polymarket** va SIEMPRE en el título. Esto es el precio de
///    un mercado de predicción, no el FedWatch del CME (que sale de los
///    futuros de fondos federales); no son lo mismo y no tienen por qué
///    coincidir. El pie lo repite en texto para que no dependa de que alguien
///    interprete bien el título.
/// 2. El dato NO entra en el score compuesto ni en ninguna señal — es contexto
///    aparte, y por eso vive en su propio panel en vez de como una línea más
///    del panel de posicionamiento.
/// 3. NO se usan los colores `positive`/`negative` del tema. En esta app esos
///    dos tonos significan long/short y ganancia/pérdida; pintar "bajada de
///    tipos" de verde sería colar una recomendación direccional dentro de algo
///    que es explícitamente informativo. Se usan acentos neutros.
fn draw_fed(f: &mut Frame, app: &App, area: Rect) {
    let tr = crate::i18n::t();
    let block = theme::block().title(tr.fl_fed_title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(o) = &app.fed_odds else {
        // sin dato: se dice cuál es el estado, no se deja el hueco en blanco
        f.render_widget(
            Paragraph::new(Line::from(dim(tr.fl_fed_waiting.into()))),
            inner,
        );
        return;
    };

    let mut lines = vec![fed_date_line(o), fed_headline(o)];
    lines.push(Line::from(""));
    let (cut, hold, hike) = o.totals();
    lines.push(fed_row(tr.fl_fed_cut, cut, theme::c().accent_cyan));
    lines.push(fed_row(tr.fl_fed_hold, hold, theme::c().neutral));
    lines.push(fed_row(tr.fl_fed_hike, hike, theme::c().accent_amber));
    lines.push(Line::from(dim(tr.fl_fed_note.into())));
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn fed_date_line(o: &crate::data::polymarket::FedOdds) -> Line<'static> {
    let (_, m, d) = o.meeting_ymd();
    let month = MONTHS
        .get(m.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("—");
    let days = o.days_until(crate::data::now_ms());
    Line::from(vec![
        Span::styled(
            format!("{month} {d}"),
            Style::new()
                .fg(theme::c().fg_strong)
                .add_modifier(Modifier::BOLD),
        ),
        dim(format!("  ·  {days}d")),
    ])
}

/// Titular: el tramo más probable, en grande y solo. El desglose completo va
/// debajo, secundario — mismo criterio de "un número claro primero" que pide
/// el CLAUDE.md para los indicadores compuestos.
fn fed_headline(o: &crate::data::polymarket::FedOdds) -> Line<'static> {
    use crate::data::polymarket::FedKind;
    let tr = crate::i18n::t();
    let Some(b) = o.top() else {
        return Line::from(dim(tr.fl_fed_unavailable.into()));
    };
    let what = match b.kind {
        FedKind::Cut => tr.fl_fed_cut,
        FedKind::NoChange => tr.fl_fed_hold,
        FedKind::Hike => tr.fl_fed_hike,
    };
    let mag = match b.bps {
        Some(v) => format!(" {v}{}bp", if b.bps_plus { "+" } else { "" }),
        None => String::new(),
    };
    Line::from(vec![
        Span::styled(
            format!("{what}{mag}"),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        dim(" ".to_string()),
        Span::styled(
            format!("{:.0}%", b.prob * 100.0),
            Style::new()
                .fg(theme::c().fg_strong)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// Una fila del desglose, con barra de bloques para leerla de un vistazo.
fn fed_row(label: &str, p: f64, color: Color) -> Line<'static> {
    const BAR_W: usize = 10;
    let filled = ((p * BAR_W as f64).round() as usize).min(BAR_W);
    Line::from(vec![
        dim(format!("{label:<11}")),
        Span::styled("█".repeat(filled), Style::new().fg(color)),
        Span::styled(
            "·".repeat(BAR_W - filled),
            Style::new().fg(theme::c().no_data),
        ),
        Span::styled(format!(" {:>3.0}%", p * 100.0), Style::new().fg(color)),
    ])
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

    fn pantalla(term: &mut Terminal<TestBackend>, app: &mut App) -> String {
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.push('\n');
        }
        s
    }

    fn odds_de_prueba() -> crate::data::polymarket::FedOdds {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/polymarket_fed_events.json"
        ))
        .unwrap();
        crate::data::polymarket::parse_events(&fixture, 1_789_344_000_000).unwrap()
    }

    /// Requisito duro y no negociable del widget: la procedencia del dato
    /// ("Polymarket") tiene que estar SIEMPRE a la vista, y en ningún caso
    /// puede presentarse como FedWatch — son dos cosas distintas.
    #[test]
    fn el_widget_del_fomc_siempre_se_atribuye_a_polymarket() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        app.view = View::Flow;
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();

        // incluso antes de que llegue el primer dato, la ficha ya se identifica
        let vacio = pantalla(&mut term, &mut app);
        assert!(vacio.contains("Polymarket"), "sin atribución:\n{vacio}");

        app.apply_msg(crate::data::types::DataMsg::FedOdds(odds_de_prueba()));
        let lleno = pantalla(&mut term, &mut app);
        assert!(lleno.contains("Polymarket"), "sin atribución:\n{lleno}");
        // "FedWatch" solo puede aparecer negado (el descargo del pie), nunca
        // como etiqueta del dato
        for m in lleno.match_indices("FedWatch") {
            let antes = &lleno[m.0.saturating_sub(4)..m.0];
            assert!(
                antes.contains("not ") || antes.contains("no "),
                "FedWatch sin negar en {:?}:\n{lleno}",
                antes
            );
        }
        // fecha de la reunión y desglose de los tres sentidos
        assert!(lleno.contains("Sep 16"), "falta la fecha:\n{lleno}");
        // titular = tramo más probable (25bp de subida, 0.785) y desglose
        assert!(lleno.contains("Hike 25bp"), "falta el titular:\n{lleno}");
        assert!(lleno.contains("20%"), "falta el tramo de sin cambio:\n{lleno}");
        // el pie repite la atribución en texto, no solo en el borde
        assert!(lleno.contains("not FedWatch"), "falta el pie:\n{lleno}");

        // en español debe seguir atribuido igual
        crate::i18n::set_lang(crate::i18n::Lang::Es);
        let es = pantalla(&mut term, &mut app);
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert!(es.contains("Polymarket"), "sin atribución en ES:\n{es}");
        assert!(es.contains("Sin cambio"), "sin traducir:\n{es}");
    }

    /// El dato de Polymarket es contexto DESACOPLADO: recibirlo no puede mover
    /// ni un dígito del score compuesto ni del resto del panel.
    #[test]
    fn el_dato_del_fomc_no_toca_el_score_compuesto() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        let antes: Vec<_> = ["BTC", "ETH", "SOL"]
            .iter()
            .map(|c| crate::flow::score(&app.score_inputs(c)))
            .collect();

        app.apply_msg(crate::data::types::DataMsg::FedOdds(odds_de_prueba()));

        let despues: Vec<_> = ["BTC", "ETH", "SOL"]
            .iter()
            .map(|c| crate::flow::score(&app.score_inputs(c)))
            .collect();
        assert_eq!(antes, despues, "el score no debe depender del FOMC");
    }

    /// En un terminal estrecho el widget de contexto cede el sitio: el panel
    /// de posicionamiento es el contenido real de la vista y no puede quedar
    /// aplastado por una ficha informativa.
    #[test]
    fn en_terminal_estrecho_el_widget_cede_el_sitio() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        app.view = View::Flow;
        app.apply_msg(crate::data::types::DataMsg::FedOdds(odds_de_prueba()));

        let mut ancho = Terminal::new(TestBackend::new(160, 40)).unwrap();
        assert!(pantalla(&mut ancho, &mut app).contains("Polymarket"));

        let mut estrecho = Terminal::new(TestBackend::new(80, 40)).unwrap();
        let s = pantalla(&mut estrecho, &mut app);
        assert!(!s.contains("Polymarket"), "debería haber cedido:\n{s}");
        // y el panel que sí importa sigue ahí
        assert!(s.contains("Funding") || s.contains("funding"), "{s}");
    }

    /// Las Vistas 6, 7, 8 y 9 ya no pintan con la paleta ANSI del terminal:
    /// todo su cuerpo sale de `theme.rs`, el marco sigue al tema activo y el
    /// toggle claro/oscuro las repinta. (El QR de WalletConnect es la única
    /// excepción viva del proyecto — blanco/negro literales para que la cámara
    /// lo lea — y solo se dibuja con una conexión en curso, que este test no
    /// monta.)
    #[test]
    fn vistas_6_7_8_9_pintan_con_el_tema() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();

        for view in [View::Flow, View::Whales, View::Wallet, View::Funds] {
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

    /// Los modales de la Vista 8 (depósito, retiro, transferencia) heredan el
    /// fondo del tema: son OPACOS sobre lo que tapan y tampoco traen colores
    /// ANSI sueltos. (El de agent no se puede montar desde aquí: `AgentUi`
    /// guarda la clave privada en un campo privado, y así se queda.)
    #[test]
    fn los_modales_de_la_vista_8_heredan_el_fondo_del_tema() {
        use crate::app::{DepositUi, TransferUi, WithdrawUi};
        use crate::data::types::AccountSnapshot;
        use crate::wallet::walletconnect::{WcSession, WcStatus};

        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();

        for th in [theme::Theme::Dark, theme::Theme::Light] {
            theme::set_theme(th);
            let esperado = match th {
                theme::Theme::Dark => theme::DARK.bg,
                theme::Theme::Light => theme::LIGHT.bg,
            };
            for modal in 0..3 {
                let mut app = app_con_datos();
                app.view = View::Funds;
                // sesión y saldos FALSOS, solo para que los modales lleguen a
                // dibujarse: nada de esto firma ni llama a la red
                app.wc = WcStatus::Connected(WcSession {
                    address: "0x0000000000000000000000000000000000000001".into(),
                    chain: "eip155:42161".into(),
                    peer: None,
                    since: Instant::now(),
                    session_topic: "test".into(),
                });
                app.usdc = Some(Some(100.0));
                app.funds = Some(AccountSnapshot {
                    addr: "0x0000000000000000000000000000000000000001".into(),
                    account_value: 100.0,
                    withdrawable: 100.0,
                    total_margin_used: 0.0,
                    total_ntl_pos: 0.0,
                    positions: vec![],
                });
                match modal {
                    0 => {
                        app.deposit_ui = Some(DepositUi::Confirm {
                            usdc: 5.0,
                            units: 5_000_000,
                        })
                    }
                    1 => {
                        app.withdraw_ui = Some(WithdrawUi::Confirm {
                            usdc: 5.0,
                            units: 5_000_000,
                        })
                    }
                    _ => {
                        app.transfer_ui = Some(TransferUi::Confirm {
                            to_perp: true,
                            usdc: 5.0,
                            units: 5_000_000,
                        })
                    }
                }
                term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer().clone();

                // que el modal se haya dibujado DE VERDAD (si la ruta no
                // estuviera montada, `draw_*_modal` sale antes de pintar y el
                // resto del test pasaría en vacío)
                let mut txt = String::new();
                for y in 0..buf.area.height {
                    for x in 0..buf.area.width {
                        txt.push_str(buf.cell((x, y)).unwrap().symbol());
                    }
                }
                let tr = crate::i18n::t();
                let titulo = match modal {
                    0 => tr.fu_dep_confirm_title,
                    1 => tr.fu_wd_confirm_title,
                    _ => tr.fu_xfer_confirm_title,
                };
                assert!(
                    txt.contains(titulo.trim()),
                    "el modal {modal} no llegó a dibujarse"
                );

                // el modal se dibuja centrado: la fila del medio del cuerpo
                // tiene que estar pintada con el fondo del tema, no
                // transparente sobre lo de debajo
                let y = buf.area.height / 2;
                let opacas = (0..buf.area.width)
                    .filter(|x| buf.cell((*x, y)).unwrap().bg == esperado)
                    .count();
                assert!(
                    opacas > 20,
                    "el modal {modal} no es opaco con el tema {th:?} \
                     (solo {opacas} celdas con el fondo del tema)"
                );
                for yy in 1..buf.area.height - 1 {
                    for x in 0..buf.area.width {
                        let cell = buf.cell((x, yy)).unwrap();
                        for col in [cell.fg, cell.bg] {
                            assert!(
                                matches!(col, Color::Rgb(..) | Color::Reset),
                                "color ANSI suelto en el modal {modal}: {col:?}"
                            );
                        }
                    }
                }
            }
        }
        theme::set_theme(theme::Theme::Dark);
    }

    /// PROTOTIPO de la Vista 1: `v` abre el panel lateral con el score del par
    /// seleccionado (el MISMO que la Vista 6, no un cálculo nuevo), anclado a
    /// la izquierda, con sombra asomando abajo-derecha; `v` otra vez lo cierra.
    #[test]
    fn el_panel_rapido_de_la_vista_1_reusa_el_score_y_lleva_sombra() {
        use crossterm::event::{KeyCode, KeyEvent};

        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        theme::set_theme(theme::Theme::Dark);
        let mut app = app_con_datos();
        app.view = View::Ranking;
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();

        let pinta = |term: &mut Terminal<TestBackend>, app: &mut App| {
            term.draw(|f| crate::ui::draw(f, app)).unwrap();
            let buf = term.backend().buffer().clone();
            let mut txt = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    txt.push_str(buf.cell((x, y)).unwrap().symbol());
                }
                txt.push('\n');
            }
            (txt, buf)
        };

        let (sin, _) = pinta(&mut term, &mut app);
        assert!(!sin.contains("señales"), "cerrado por defecto:\n{sin}");

        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        let (con, buf) = pinta(&mut term, &mut app);

        // el par del panel es el seleccionado, y su contenido es el score de
        // la Vista 6 para ESE par, calculado aquí de forma independiente
        let coin = app.sorted_coins()[app.sel].clone();
        let s = crate::flow::score(&app.score_inputs(&coin));
        assert!(
            con.contains(&format!("⌁ {coin}")),
            "título del panel:\n{con}"
        );
        assert!(
            con.contains(&format!(
                "{} de {} señales",
                s.avail,
                crate::flow::SCORE_COMPONENTS
            )),
            "el desglose honesto sale del score compartido:\n{con}"
        );

        // sombra: a la derecha del panel hay celdas con el color de sombra del
        // tema, y no las hay a su izquierda
        let sombra = theme::shadow_bg();
        let hay_sombra = (0..buf.area.height)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf.cell((x, y)).unwrap().bg == sombra);
        assert!(hay_sombra, "el panel no dibujó sombra:\n{con}");

        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        let (cerrado, _) = pinta(&mut term, &mut app);
        assert!(!cerrado.contains("señales"), "`v` cierra:\n{cerrado}");
        theme::set_theme(theme::Theme::Dark);
    }

    /// REGRESIÓN del panic `index out of bounds: the len is 32 but the index
    /// is 32` (src/ui/ranking.rs) al abrir el panel rápido con un par cuyo
    /// score apunta 100% a un lado: la barra se llenaba hasta `mid + mid`, que
    /// es exactamente el ancho del buffer. Nada que ver con la posición del par
    /// en la tabla — solo hace falta un sesgo pleno, que es más probable en los
    /// pares de más abajo porque tienen menos componentes con dato.
    #[test]
    fn el_panel_rapido_no_revienta_con_sesgo_pleno() {
        use crossterm::event::{KeyCode, KeyEvent};

        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut app = app_con_datos();
        app.view = View::Ranking;
        // BTC solo tiene la señal de whales (100% largas) → bull 1 / avail 1
        // → sesgo +100%, el caso que llenaba la barra hasta el borde
        let coin = "BTC";
        app.sel = app
            .sorted_coins()
            .iter()
            .position(|c| c == coin)
            .expect("BTC en la tabla");
        let s = crate::flow::score(&app.score_inputs(coin));
        assert_eq!(
            (s.bull, s.bear, s.avail),
            (1, 0, 1),
            "el caso reproducido debe ser sesgo pleno"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
        // sin el fix, este draw entra en panic y tumba la app entera
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();

        let buf = term.backend().buffer().clone();
        let mut txt = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                txt.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            txt.push('\n');
        }
        assert!(txt.contains("+100%"), "sesgo pleno en pantalla:\n{txt}");
        theme::set_theme(theme::Theme::Dark);
    }
}
