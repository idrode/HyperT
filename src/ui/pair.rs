use ratatui::prelude::*;
use ratatui::symbols;
use ratatui::widgets::{Axis, Chart, Dataset, GraphType, Paragraph, Sparkline};

use crate::app::{App, DeltaState, PairState, OI_WIN_LONG, OI_WIN_SHORT};
use crate::data::types::CandlePoint;
use crate::signals::WhaleParams;

use super::fmt::{age_label, fmt_opt_pct, fmt_px, fmt_usd, time_label};
use super::oscimg::{self, DeltaSpec, LineColor, OscLine, OscSpec};
use super::taplot;
use super::theme;
use super::whalersi::rsi_zone_color;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let mouse = app.mouse_pos;
    let ind = app.ind;
    // el caché de imagen (gfx), el delta y el par se prestan por campos
    // disjuntos de App
    let gfx = &mut app.gfx;
    let delta = app.delta.as_ref();
    let Some(p) = app.selected_coin.as_deref().and_then(|c| app.pairs.get(c)) else {
        let s = crate::i18n::t();
        f.render_widget(
            Paragraph::new(s.pr_select_pair).block(theme::block().title(s.pr_word_pair)),
            area,
        );
        return;
    };

    let rows = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(8),
        Constraint::Length(5),
    ])
    .split(area);

    draw_summary(f, p, rows[0]);

    let mid_cols = Layout::horizontal([
        Constraint::Percentage(CHART_COL_PCT),
        Constraint::Percentage(100 - CHART_COL_PCT),
    ])
    .split(rows[1]);
    // columna izquierda: velas arriba + RSI/ADX/DMI apilado debajo; la vela i
    // ocupa las mismas columnas de pantalla en ambos paneles (mismo eje X)
    let ta_h = if mid_cols[0].height >= 8 + TA_PANEL_H {
        TA_PANEL_H
    } else {
        0
    };
    // el delta por vela solo se apila si, además del sub-panel TA, queda alto
    // suficiente — nunca sacrifica velas ni el TA por él
    let delta_h = if ta_h > 0 && mid_cols[0].height >= 8 + TA_PANEL_H + DELTA_PANEL_H {
        DELTA_PANEL_H
    } else {
        0
    };
    let left = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(ta_h),
        Constraint::Length(delta_h),
    ])
    .split(mid_cols[0]);
    let hover = p
        .extra
        .as_ref()
        .and_then(|e| taplot::hover_idx(mouse, mid_cols[0], AXIS_W, CANDLE_CELLS, e.candles.len()));
    draw_price_chart(f, p, hover, left[0]);
    if ta_h > 0 {
        draw_ta_panel(f, p, gfx, hover, ind, left[1]);
    }
    if delta_h > 0 {
        let delta = delta.filter(|d| d.coin == p.meta.name);
        draw_delta_panel(f, p, delta, gfx, hover, left[2]);
    }
    draw_funding_chart(f, p, mid_cols[1]);

    let bottom =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[2]);
    let oi_series: Vec<f64> = p.hist.iter().map(|h| h.oi).collect();
    draw_spark(
        f,
        crate::i18n::t().pr_spark_oi,
        &oi_series,
        theme::c().accent_amber,
        bottom[0],
    );
    let mid_series: Vec<f64> = p.mid_hist.iter().map(|(_, m)| *m).collect();
    draw_spark(
        f,
        crate::i18n::t().pr_spark_mid,
        &mid_series,
        theme::c().accent_cyan,
        bottom[1],
    );
}

fn draw_summary(f: &mut Frame, p: &PairState, area: Rect) {
    let chg = p.chg24_pct();
    let f_h = p.funding_hourly_pct();
    let apr = p.funding_apr_pct();
    let prem = p.premium_bps();
    let d5 = p.oi_delta_pct(OI_WIN_SHORT);
    let d1h = p.oi_delta_pct(OI_WIN_LONG);
    let reg = p.regime(OI_WIN_LONG);
    let (oi_base, oracle) = match p.ctx {
        Some(c) => (c.open_interest, c.oracle_px),
        None => (0.0, 0.0),
    };

    let bold = |s: String| Span::styled(s, Style::new().add_modifier(Modifier::BOLD));
    let dim = |s: &'static str| Span::styled(s, Style::new().fg(theme::c().muted));
    let colored = |s: String, c: Color| Span::styled(s, Style::new().fg(c));

    let tr = crate::i18n::t();
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", p.meta.name),
                Style::new()
                    .fg(theme::c().accent_cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            bold(fmt_px(p.mid)),
            dim(tr.pr_24h),
            colored(fmt_opt_pct(chg, 2), theme::sign_color(chg, false)),
            dim(tr.pr_max_lev),
            Span::raw(format!("{}×", p.meta.max_leverage)),
        ]),
        Line::from(vec![
            dim(tr.pr_funding),
            colored(fmt_opt_pct(f_h, 4), theme::sign_color(f_h, true)),
            dim("/h ("),
            colored(fmt_opt_pct(apr, 1), theme::sign_color(apr, true)),
            dim(tr.pr_apr_premium),
            colored(
                format!("{} bp", super::fmt::fmt_opt(prem, 1)),
                theme::sign_color(prem, true),
            ),
            dim(tr.pr_oracle),
            Span::raw(fmt_px(oracle)),
        ]),
        Line::from(vec![
            dim("OI "),
            Span::raw(format!(
                "{oi_base:.prec$} {} ",
                p.meta.name,
                prec = p.meta.sz_decimals.min(2) as usize
            )),
            dim("("),
            Span::raw(fmt_usd(p.oi_notional())),
            dim(")   ΔOI 5m "),
            colored(fmt_opt_pct(d5, 2), theme::sign_color(d5, false)),
            dim("  1h "),
            colored(fmt_opt_pct(d1h, 2), theme::sign_color(d1h, false)),
            dim(tr.pr_vol24),
            Span::raw(fmt_usd(p.volume24())),
        ]),
        Line::from(vec![
            dim(tr.pr_flow_1h),
            Span::styled(
                reg.label(),
                Style::new()
                    .fg(theme::regime_color(reg))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    // confirmación secundaria (RSI/ADX sobre las velas cargadas), nunca señal primaria
    let ta_line = match &p.extra {
        Some(e) => {
            let tf = e.interval.label();
            let rsi_span = match e.rsi {
                Some(r) => {
                    let c = if r >= 70.0 {
                        theme::c().negative
                    } else if r <= 30.0 {
                        theme::c().positive
                    } else {
                        theme::c().neutral
                    };
                    Span::styled(format!("{r:.0}"), Style::new().fg(c))
                }
                None => Span::raw("—"),
            };
            let dmi_spans = match &e.dmi {
                Some(d) => vec![
                    Span::raw(format!("{:.0}", d.adx)),
                    dim("  +DI "),
                    colored(format!("{:.0}", d.plus_di), theme::c().positive),
                    dim(" −DI "),
                    colored(format!("{:.0}", d.minus_di), theme::c().negative),
                ],
                None => vec![Span::raw("—")],
            };
            let mut spans = vec![
                Span::styled(
                    format!("{} ({tf}): RSI(14) ", tr.pr_confirmation),
                    Style::new().fg(theme::c().muted),
                ),
                rsi_span,
                dim("   ADX(14) "),
            ];
            spans.extend(dmi_spans);
            Line::from(spans)
        }
        None => Line::from(dim(tr.pr_confirm_loading)),
    };
    lines.push(ta_line);

    f.render_widget(
        Paragraph::new(lines)
            .block(theme::block().title(format!(" {}{}", p.meta.name, tr.pr_perp))),
        area,
    );
}

/// Ancho reservado al eje de precio a la derecha del gráfico de velas.
const AXIS_W: u16 = 10;
/// Columnas de texto por vela: 1 de cuerpo + 1 de hueco.
const CANDLE_CELLS: u16 = 2;
/// % del ancho de la vista para la columna de velas+TA (el resto, funding).
const CHART_COL_PCT: u16 = 62;

/// Nº de velas visibles dado el ancho del panel de velas (bordes y eje fuera).
fn max_vis_for(panel_w: u16) -> usize {
    ((panel_w.saturating_sub(2 + AXIS_W) / CANDLE_CELLS) as usize).max(2)
}

/// Ventana temporal de referencia: nº de velas que esta vista muestra para el
/// área completa de la vista. La Vista 3 (whalersi) consume el mismo nº para
/// que ambas pinten exactamente el mismo rango de fechas con la misma
/// temporalidad — su panel es más estrecho, así que escala el trazo en vez de
/// recalcular su propia ventana.
pub(super) fn visible_candles(view: Rect) -> usize {
    let cols = Layout::horizontal([
        Constraint::Percentage(CHART_COL_PCT),
        Constraint::Percentage(100 - CHART_COL_PCT),
    ])
    .split(view);
    max_vis_for(cols[0].width)
}
/// Alto del sub-panel RSI/ADX/DMI bajo las velas (se omite si no cabe).
const TA_PANEL_H: u16 = 9;
/// Alto de la barra de delta por vela bajo el sub-panel TA (se omite si no cabe).
const DELTA_PANEL_H: u16 = 5;

fn draw_price_chart(f: &mut Frame, p: &PairState, hover: Option<usize>, area: Rect) {
    let tr = crate::i18n::t();
    let placeholder = |f: &mut Frame, msg: &str| {
        f.render_widget(
            Paragraph::new(msg.to_string()).block(theme::block().title(tr.pr_candles_tf)),
            area,
        );
    };
    let Some(e) = &p.extra else {
        placeholder(f, tr.t_loading_candles);
        return;
    };
    if e.candles.len() < 2 {
        placeholder(f, tr.t_no_candles);
        return;
    }
    let inner = theme::block().inner(area);
    if inner.width < AXIS_W + 4 * CANDLE_CELLS || inner.height < 4 {
        placeholder(f, "");
        return;
    }
    let chart = Rect::new(inner.x, inner.y, inner.width - AXIS_W, inner.height);
    let axis = Rect::new(chart.right(), inner.y, AXIS_W, inner.height);

    let max_vis = max_vis_for(area.width);
    let vis = &e.candles[e.candles.len().saturating_sub(max_vis)..];
    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in vis {
        ymin = ymin.min(c.low);
        ymax = ymax.max(c.high);
    }
    let pad = ((ymax - ymin) * 0.03).max(ymax * 0.0005);
    let (ymin, ymax) = (ymin - pad, ymax + pad);
    let span = (ymax - ymin).max(1e-12);

    let last = vis.last().unwrap();
    let last_color = if last.close >= last.open {
        theme::c().candle_up
    } else {
        theme::c().candle_down
    };

    let mut block = theme::block().title(format!(
        " {} {} ×{}{}",
        tr.pr_word_candles,
        e.interval.label(),
        vis.len(),
        tr.pr_candles_mouse,
    ));
    if let Some(i) = hover {
        block = block.title_bottom(hover_line(&vis[i]));
    }
    f.render_widget(block, area);

    // Velas dibujadas a mano sobre el buffer con resolución vertical de medio
    // bloque (2 sub-filas por celda): cuerpo sólido █/▀/▄ de una celda de
    // ancho y mecha │/╵/╷ centrada en la misma columna. El Braille anterior
    // (puntos) daba aspecto de scatter; esto se lee como velas de exchange.
    let hh = chart.height as usize * 2;
    let srow = |v: f64| -> usize {
        (((ymax - v) / span) * hh as f64)
            .floor()
            .clamp(0.0, hh as f64 - 1.0) as usize
    };
    let buf = f.buffer_mut();

    // fondo: rejilla alineada con los labels del eje + línea del último cierre
    for frac in [0.25, 0.5, 0.75] {
        let y = chart.y + (srow(ymin + span * frac) / 2) as u16;
        for x in chart.left()..chart.right() {
            buf[(x, y)].set_symbol("─").set_fg(theme::c().grid);
        }
    }
    let y_last = chart.y + (srow(last.close) / 2) as u16;
    for x in chart.left()..chart.right() {
        buf[(x, y_last)]
            .set_symbol("╌")
            .set_fg(theme::c().crosshair);
    }

    for (i, c) in vis.iter().enumerate() {
        // misma paleta verde/rojo que el resto de valores +/- de la UI;
        // la vela bajo el cursor se resalta en su variante clara
        let up = c.close >= c.open;
        let color = match (up, hover == Some(i)) {
            (true, false) => theme::c().candle_up,
            (false, false) => theme::c().candle_down,
            (true, true) => theme::c().candle_up_hi,
            (false, true) => theme::c().candle_down_hi,
        };
        let x = chart.x + i as u16 * CANDLE_CELLS;
        let (s_hi, s_lo) = (srow(c.high), srow(c.low));
        let (s_bt, s_bb) = (srow(c.open.max(c.close)), srow(c.open.min(c.close)));
        let body = |s: usize| s >= s_bt && s <= s_bb;
        let wick = |s: usize| s >= s_hi && s <= s_lo;
        for cy in (s_hi / 2)..=(s_lo / 2) {
            let (top, bot) = (cy * 2, cy * 2 + 1);
            let sym = match (body(top), body(bot)) {
                (true, true) => "█",
                (true, false) => "▀",
                (false, true) => "▄",
                (false, false) => match (wick(top), wick(bot)) {
                    (true, true) => "│",
                    (true, false) => "╵",
                    (false, true) => "╷",
                    (false, false) => continue,
                },
            };
            buf[(x, chart.y + cy as u16)].set_symbol(sym).set_fg(color);
        }
    }

    // eje de precio: labels de rejilla + último cierre resaltado
    let h = axis.height as usize;
    let row_of = |v: f64| -> usize {
        (((ymax - v) / span * h as f64) - 0.5)
            .round()
            .clamp(0.0, h as f64 - 1.0) as usize
    };
    let mut labels: Vec<Line> = vec![Line::raw(""); h];
    for frac in [0.0, 0.25, 0.5, 0.75, 1.0] {
        let v = ymax - span * frac;
        labels[row_of(v)] = Line::from(Span::styled(fmt_px(v), Style::new().fg(theme::c().muted)));
    }
    labels[row_of(last.close)] = Line::from(Span::styled(
        format!("▶{}", fmt_px(last.close)),
        Style::new().fg(last_color).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(Paragraph::new(labels), axis);
}

/// Sub-panel RSI/ADX/DMI apilado bajo las velas, estilo panel de indicador de
/// TradingView: misma ventana visible y misma columna de pantalla por vela.
/// Imagen real compartida con la Vista 3 (`oscimg`: plotters vía protocolo
/// Kitty, fallback halfblocks) y hover compartido con las velas: hora +
/// valores de la vela apuntada en el borde inferior. Reusa las series de
/// `e.panel` ya calculadas — cero cálculo nuevo.
fn draw_ta_panel(
    f: &mut Frame,
    p: &PairState,
    gfx: &mut oscimg::Gfx,
    hover: Option<usize>,
    ind: crate::app::IndSel,
    area: Rect,
) {
    let wp = WhaleParams::default();
    // título según la selección (tecla o): solo lo que de verdad se pinta
    let mut tspans = vec![Span::raw(" ")];
    let sep = |v: &mut Vec<Span>| {
        if v.len() > 1 {
            v.push(Span::raw(" · "));
        }
    };
    if ind.rsi {
        tspans.push(Span::styled(
            "RSI",
            Style::new().fg(theme::c().accent_magenta),
        ));
        tspans.push(Span::styled(
            " · MA",
            Style::new().fg(theme::c().accent_amber),
        ));
    }
    if ind.adx_dmi {
        sep(&mut tspans);
        tspans.push(Span::styled("ADX", Style::new().fg(theme::c().neutral)));
        tspans.push(Span::styled(" · +DI", Style::new().fg(theme::c().positive)));
        tspans.push(Span::styled(" · −DI", Style::new().fg(theme::c().negative)));
    }
    if ind.trix {
        sep(&mut tspans);
        tspans.push(Span::styled(
            format!("TRIX({})", crate::signals::TRIX_PERIOD),
            Style::new().fg(theme::c().accent_cyan),
        ));
    }
    if tspans.len() == 1 {
        tspans.push(Span::styled(
            crate::i18n::t().pr_ind_none.to_string(),
            Style::new().fg(theme::c().muted),
        ));
    }
    tspans.push(Span::raw(crate::i18n::t().pr_same_axis));
    let mut block = theme::block().title(Line::from(tspans));
    let Some(e) = &p.extra else {
        f.render_widget(
            Paragraph::new(crate::i18n::t().t_loading_candles).block(block),
            area,
        );
        return;
    };
    let n = e.candles.len();
    if n < 2 {
        f.render_widget(
            Paragraph::new(crate::i18n::t().t_no_candles).block(block),
            area,
        );
        return;
    }
    let inner = block.inner(area);
    if inner.width < AXIS_W + 4 * CANDLE_CELLS || inner.height < 3 {
        f.render_widget(block, area);
        return;
    }
    let chart = Rect::new(inner.x, inner.y, inner.width - AXIS_W, inner.height);
    let axis = Rect::new(chart.right(), inner.y, AXIS_W, inner.height);
    let start = n.saturating_sub(max_vis_for(area.width));
    let panel = &e.panel;
    // TRIX reescalado al eje 0-100 del panel (0→50, ±máx→±45); el hover y el
    // eje traducen de vuelta al valor real
    let (trix_scaled, _trix_max) = super::taplot::scale_zero_centered(&e.trix);

    // hora + valores de la vela bajo el cursor en el borde inferior
    if let Some(i) = hover.map(|h| start + h).filter(|i| *i < n) {
        let d = &panel.dmi[i];
        let num = |v: f64| {
            if v.is_finite() {
                format!("{v:.0}")
            } else {
                "—".to_string()
            }
        };
        let mut txt = format!(" {}", time_label(e.candles[i].t_close));
        if ind.rsi {
            txt += &format!(" · RSI {} · MA {}", num(panel.rsi[i]), num(panel.rsi_ma[i]));
        }
        if ind.adx_dmi {
            txt += &format!(
                " · ADX {} · +DI {} · −DI {}",
                num(d.adx),
                num(d.plus_di),
                num(d.minus_di)
            );
        }
        if ind.trix {
            let v = e.trix[i];
            txt += &if v.is_finite() {
                format!(" · TRIX {v:+.1}")
            } else {
                " · TRIX —".to_string()
            };
        }
        txt.push(' ');
        block = block.title_bottom(txt);
    }
    f.render_widget(block, area);

    let adx: Vec<f64> = panel.dmi.iter().map(|d| d.adx).collect();
    let pdi: Vec<f64> = panel.dmi.iter().map(|d| d.plus_di).collect();
    let mdi: Vec<f64> = panel.dmi.iter().map(|d| d.minus_di).collect();
    // imagen real compartida con la Vista 3 (oscimg): niveles 30/50/70 y banda
    // van dentro del raster; re-rasteriza solo si cambian datos, selección
    // (tecla o → ind.mask() como clave) o tamaño
    let zone = |v: f64| oscimg::rsi_zone_rgb(v, &wp);
    let mut lines = Vec::new();
    if ind.adx_dmi {
        lines.push(OscLine {
            vals: &adx,
            width: 1,
            color: LineColor::Fixed(oscimg::gray()),
        });
        lines.push(OscLine {
            vals: &pdi,
            width: 1,
            color: LineColor::Fixed(oscimg::green()),
        });
        lines.push(OscLine {
            vals: &mdi,
            width: 1,
            color: LineColor::Fixed(oscimg::red()),
        });
    }
    if ind.trix {
        lines.push(OscLine {
            vals: &trix_scaled,
            width: if ind.rsi { 1 } else { 2 },
            color: LineColor::Fixed(oscimg::cyan()),
        });
    }
    if ind.rsi {
        lines.push(OscLine {
            vals: &panel.rsi_ma,
            width: 1,
            color: LineColor::Fixed(oscimg::yellow()),
        });
        lines.push(OscLine {
            vals: &panel.rsi,
            width: 2,
            color: LineColor::ByValue(&zone),
        });
    }
    let spec = OscSpec {
        start,
        len: n - start,
        cols_per_pt: CANDLE_CELLS as f64,
        half_cols: 0.5,
        oversold: wp.oversold,
        overbought: wp.overbought,
        lines,
        bars: vec![],
        marks: vec![],
    };
    oscimg::draw_into(
        f,
        chart,
        gfx,
        oscimg::OscSlot::PairTa,
        e.stamp,
        ind.mask(),
        spec,
    );

    // eje 0-100 compacto: niveles + RSI actual resaltado
    let h = axis.height as usize;
    let row_of = |v: f64| -> usize {
        (((100.0 - v) / 100.0 * h as f64) - 0.5)
            .round()
            .clamp(0.0, h as f64 - 1.0) as usize
    };
    let mut labels: Vec<Line> = vec![Line::raw(""); h];
    for (v, c) in [
        (wp.overbought, theme::c().negative_dim),
        (50.0, theme::c().muted),
        (wp.oversold, theme::c().positive_dim),
    ] {
        labels[row_of(v)] = Line::from(Span::styled(format!("{v:.0}"), Style::new().fg(c)));
    }
    if ind.rsi {
        if let Some(r) = panel.last_rsi() {
            labels[row_of(r)] = Line::from(Span::styled(
                format!("▶{r:.0}"),
                Style::new()
                    .fg(rsi_zone_color(r, &wp))
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    f.render_widget(Paragraph::new(labels), axis);
}

/// Barra de delta por vela bajo el sub-panel TA: volumen comprador vs. vendedor
/// agresor de CADA vela (agregado, no por nivel de precio — paso previo al
/// footprint), reusando el mismo canal `trades` que alimenta el CVD y el mismo
/// pipeline raster (plotters vía Kitty) que los osciladores. Verde arriba =
/// domina la compra agresora; rojo abajo = domina la venta. Warmup honesto:
/// las velas previas al tracking del par salen vacías (no hay histórico de
/// trades en la API, solo el flujo en vivo), igual que el CVD.
fn draw_delta_panel(
    f: &mut Frame,
    p: &PairState,
    delta: Option<&DeltaState>,
    gfx: &mut oscimg::Gfx,
    hover: Option<usize>,
    area: Rect,
) {
    let tr = crate::i18n::t();
    let mut block = theme::block().title(Line::from(vec![
        Span::raw(tr.pr_delta_candle),
        Span::styled(tr.pr_buy, Style::new().fg(theme::c().positive)),
        Span::raw("−"),
        Span::styled(tr.pr_sell, Style::new().fg(theme::c().negative)),
        Span::raw(tr.pr_aggressor_axis),
    ]));
    let Some(e) = &p.extra else {
        f.render_widget(Paragraph::new(tr.t_loading_candles).block(block), area);
        return;
    };
    let n = e.candles.len();
    if n < 2 {
        f.render_widget(Paragraph::new(tr.t_no_candles).block(block), area);
        return;
    }
    let inner = block.inner(area);
    if inner.width < AXIS_W + 4 * CANDLE_CELLS || inner.height < 2 {
        f.render_widget(block, area);
        return;
    }
    // mismo reparto que las velas: cuerpo del gráfico + eje reservado a la
    // derecha, para que la barra i caiga en la columna de la vela i
    let chart = Rect::new(inner.x, inner.y, inner.width - AXIS_W, inner.height);
    let axis = Rect::new(chart.right(), inner.y, AXIS_W, inner.height);
    let start = n.saturating_sub(max_vis_for(area.width));
    let vis = &e.candles[start..];
    let iv_ms = e.interval.ms();

    let vals: Vec<Option<f64>> = match delta {
        Some(d) => d.per_candle(vis, iv_ms),
        None => vec![None; vis.len()],
    };

    // delta de la vela bajo el cursor en el borde inferior (coherente con el
    // hover de OHLC de las velas y del sub-panel TA)
    if let Some(i) = hover.filter(|h| *h < vals.len()) {
        let txt = match vals[i] {
            Some(v) => {
                let sign = if v >= 0.0 { "+" } else { "-" };
                format!(
                    " {} · Δ {}{} ",
                    time_label(vis[i].t_close),
                    sign,
                    fmt_usd(v.abs())
                )
            }
            None => format!(" {}{}", time_label(vis[i].t_close), tr.pr_no_trades),
        };
        block = block.title_bottom(txt);
    }
    f.render_widget(block, area);

    let key = delta.map_or(0, |d| d.raster_key(iv_ms, start, vis.len()));
    let spec = DeltaSpec {
        vals: &vals,
        cols_per_pt: CANDLE_CELLS as f64,
        half_cols: 0.5,
    };
    oscimg::draw_delta_into(f, chart, gfx, key, spec);

    // eje compacto: mayor delta absoluto visible como referencia de escala
    let max = vals.iter().flatten().fold(0.0_f64, |m, v| m.max(v.abs()));
    let h = axis.height as usize;
    if h >= 2 && max > 0.0 {
        let mut labels: Vec<Line> = vec![Line::raw(""); h];
        labels[0] = Line::from(Span::styled(
            format!("+{}", fmt_usd(max)),
            Style::new().fg(theme::c().positive),
        ));
        labels[h - 1] = Line::from(Span::styled(
            format!("-{}", fmt_usd(max)),
            Style::new().fg(theme::c().negative),
        ));
        if h >= 3 {
            labels[h / 2] = Line::from(Span::styled("0", Style::new().fg(theme::c().muted)));
        }
        f.render_widget(Paragraph::new(labels), axis);
    }
}

/// Línea OHLC de la vela bajo el cursor (borde inferior del gráfico).
fn hover_line(c: &CandlePoint) -> Line<'static> {
    let up = c.close >= c.open;
    let color = if up {
        theme::c().candle_up
    } else {
        theme::c().candle_down
    };
    let chg = if c.open > 0.0 {
        (c.close / c.open - 1.0) * 100.0
    } else {
        0.0
    };
    let dim = |s: String| Span::styled(s, Style::new().fg(theme::c().muted));
    Line::from(vec![
        dim(" O ".into()),
        Span::raw(fmt_px(c.open)),
        dim(" H ".into()),
        Span::raw(fmt_px(c.high)),
        dim(" L ".into()),
        Span::raw(fmt_px(c.low)),
        dim(" C ".into()),
        Span::styled(
            format!("{} ({chg:+.2}%)", fmt_px(c.close)),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
        dim(format!(
            " · vol {} · {} · {} ",
            fmt_usd(c.volume * c.close),
            time_label(c.t_close),
            age_label(c.t_close)
        )),
    ])
}

/// La descarga trae ~30d (para el percentil de la Vista 6); esta gráfica
/// mantiene su ventana histórica de ~3d mostrando solo la cola.
const FUNDING_CHART_HOURS: usize = 72;

fn draw_funding_chart(f: &mut Frame, p: &PairState, area: Rect) {
    let tr = crate::i18n::t();
    let block = theme::block().title(tr.pr_funding_apr_hist);
    let Some(e) = &p.extra else {
        f.render_widget(Paragraph::new(tr.t_loading).block(block), area);
        return;
    };
    if e.funding_hist.len() < 2 {
        f.render_widget(Paragraph::new(tr.pr_no_funding_hist).block(block), area);
        return;
    }
    let tail = &e.funding_hist[e.funding_hist.len().saturating_sub(FUNDING_CHART_HOURS)..];
    let pts: Vec<(f64, f64)> = tail
        .iter()
        .enumerate()
        .map(|(i, (_, rate))| (i as f64, rate * 24.0 * 365.0 * 100.0))
        .collect();
    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, y) in &pts {
        ymin = ymin.min(*y);
        ymax = ymax.max(*y);
    }
    // escala simétrica alrededor de 0 para leer el signo de un vistazo
    let m = ymin.abs().max(ymax.abs()).max(1.0) * 1.05;
    let n = (pts.len() - 1) as f64;

    let zero: Vec<(f64, f64)> = vec![(0.0, 0.0), (n, 0.0)];
    let ds_zero = Dataset::default()
        .marker(symbols::Marker::Dot)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(theme::c().muted))
        .data(&zero);
    let ds = Dataset::default()
        .marker(symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::new().fg(theme::c().accent_amber))
        .data(&pts);
    let chart = Chart::new(vec![ds_zero, ds])
        .block(block)
        .x_axis(
            Axis::default()
                .bounds([0.0, n])
                .labels(["-3d".to_string(), tr.pr_now.to_string()])
                .style(Style::new().fg(theme::c().muted)),
        )
        .y_axis(
            Axis::default()
                .bounds([-m, m])
                .labels([format!("{:-.0}%", -m), "0".to_string(), format!("{m:+.0}%")])
                .style(Style::new().fg(theme::c().muted)),
        );
    f.render_widget(chart, area);
}

fn draw_spark(f: &mut Frame, title: &str, values: &[f64], color: Color, area: Rect) {
    let w = area.width.saturating_sub(2) as usize;
    let vals: &[f64] = if values.len() > w && w > 0 {
        &values[values.len() - w..]
    } else {
        values
    };
    if vals.len() < 2 {
        f.render_widget(
            Paragraph::new(crate::i18n::t().pr_accumulating)
                .block(theme::block().title(format!(" {title} "))),
            area,
        );
        return;
    }
    let (mut mn, mut mx) = (f64::INFINITY, f64::NEG_INFINITY);
    for v in vals {
        mn = mn.min(*v);
        mx = mx.max(*v);
    }
    let span = (mx - mn).max(1e-12);
    let data: Vec<u64> = vals
        .iter()
        .map(|v| (((v - mn) / span) * 100.0).round() as u64)
        .collect();
    let block = theme::block().title(format!(" {title} [{} … {}] ", fmt_px(mn), fmt_px(mx)));
    f.render_widget(
        Sparkline::default()
            .block(block)
            .style(Style::new().fg(color))
            .data(data),
        area,
    );
}
