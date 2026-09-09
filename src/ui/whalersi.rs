//! Vista 3 — panel Ballenas + RSI/ADX/DMI (port del Pine `whales+RSI`, v6).
//! TA puro sobre precio: marca velas que rompen el Bollinger de precio con RSI
//! extremo, DI contrario dominante y ADX bajo — reversión en mercado sin
//! tendencia, no continuación. ESTIMACIÓN técnica, sin OI ni datos on-chain.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, PairExtraData, PairState};
use crate::signals::{WhaleParams, WhaleSide};

use super::fmt::{age_label, fmt_px, time_label};
use super::oscimg::{self, LineColor, OscLine, OscSpec};
use super::pair;
use super::taplot;
use super::theme;

/// Ancho reservado al eje 0-100 a la derecha del panel.
const AXIS_W: u16 = 5;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let tr = crate::i18n::t();
    let placeholder = |f: &mut Frame, msg: &'static str| {
        f.render_widget(
            Paragraph::new(msg).block(theme::block().title(tr.w3_title)),
            area,
        );
    };
    let ind3 = app.ind3;
    // el caché de imagen (gfx) y el par se prestan por campos disjuntos de App
    let gfx = &mut app.gfx;
    let Some(p) = app.selected_coin.as_deref().and_then(|c| app.pairs.get(c)) else {
        placeholder(f, tr.w3_select_pair);
        return;
    };
    let Some(e) = &p.extra else {
        placeholder(f, tr.t_loading_candles);
        return;
    };
    if e.candles.len() < 2 {
        placeholder(f, tr.t_no_candles);
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
    draw_chart(f, e, gfx, hover, win, ind3, cols[0]);
    draw_log(f, e, cols[1]);
}

/// Color del RSI según zona, como el rsiColor del Pine.
/// Compartido con el sub-panel de indicadores de la Vista 2.
pub(super) fn rsi_zone_color(v: f64, wp: &WhaleParams) -> Color {
    if v >= wp.overbought {
        theme::c().negative
    } else if v <= wp.oversold {
        theme::c().positive
    } else {
        theme::c().accent_magenta
    }
}

fn draw_summary(f: &mut Frame, p: &PairState, e: &PairExtraData, area: Rect) {
    let tr = crate::i18n::t();
    let wp = WhaleParams::default();
    let panel = &e.panel;
    let i = e.candles.len() - 1;
    let last = &e.candles[i];

    let dim = |s: String| Span::styled(s, Style::new().fg(theme::c().muted));
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
            WhaleSide::Buy => (tr.w3_trig_buy, theme::c().positive),
            WhaleSide::Sell => (tr.w3_trig_sell, theme::c().negative),
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
            Style::new()
                .fg(theme::c().accent_cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(fmt_px(p.mid), Style::new().add_modifier(Modifier::BOLD)),
        dim(format!(
            "  TF {} · {} {} ",
            e.interval.label(),
            e.candles.len(),
            tr.w3_tf_candles_last
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
        None => l1.push(dim(tr.w3_no_trigger_loaded.to_string())),
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
                theme::c().muted
            },
        ),
        dim("  MA ".to_string()),
        colored(num(panel.rsi_ma[i]), theme::c().accent_amber),
        dim("  %B ".to_string()),
        colored(num(panel.mod_rsi[i]), theme::c().accent_blue),
        dim("  ADX ".to_string()),
        Span::raw(num(d.adx)),
        dim("  +DI ".to_string()),
        colored(num(d.plus_di), theme::c().positive),
        dim("  −DI ".to_string()),
        colored(num(d.minus_di), theme::c().negative),
        dim(format!(
            "   {} [{} … {}]",
            tr.w3_bb_price,
            px_or_dash(panel.bb_lower[i]),
            px_or_dash(panel.bb_upper[i])
        )),
    ]);

    // checklist en vivo de los 5 filtros de cada lado sobre la última vela
    let cond = |label: String, ok: Option<bool>| -> Span<'static> {
        match ok {
            Some(true) => Span::styled(format!("{label}✓ "), Style::new().fg(theme::c().positive)),
            Some(false) => Span::styled(format!("{label}✗ "), Style::new().fg(theme::c().muted)),
            None => Span::styled(format!("{label}? "), Style::new().fg(theme::c().muted)),
        }
    };
    let fin = |v: f64, pred: bool| if v.is_finite() { Some(pred) } else { None };
    let (bb_lo, bb_up) = (panel.bb_lower[i], panel.bb_upper[i]);
    let l3 = Line::from(vec![
        Span::styled(
            tr.w3_long,
            Style::new()
                .fg(theme::c().positive)
                .add_modifier(Modifier::BOLD),
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
            tr.w3_short,
            Style::new()
                .fg(theme::c().negative)
                .add_modifier(Modifier::BOLD),
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
        Paragraph::new(vec![Line::from(l1), l2, l3, l4])
            .block(theme::block().title(format!(" {} — {} ", p.meta.name, tr.w3_summary_title))),
        area,
    );
}

fn draw_chart(
    f: &mut Frame,
    e: &PairExtraData,
    gfx: &mut oscimg::Gfx,
    hover: Option<usize>,
    win: usize,
    ind3: crate::app::Ind3Sel,
    area: Rect,
) {
    let tr = crate::i18n::t();
    let wp = WhaleParams::default();
    let panel = &e.panel;
    let n = e.candles.len();
    // misma ventana visible que la Vista 2 (win velas), no una propia
    let start = n.saturating_sub(win);
    // título según las líneas visibles (tecla o). Las marcas ▲▼ van siempre:
    // son el disparo de la vista, no una línea ocultable.
    let mut parts: Vec<&str> = Vec::new();
    if ind3.rsi_ma {
        parts.push("RSI · MA");
    }
    if ind3.mod_b {
        parts.push("%B");
    }
    if ind3.adx_dmi {
        parts.push("ADX/±DI");
    }
    if ind3.trix {
        parts.push("TRIX");
    }
    parts.push(tr.w3_whale_marks);
    let mut block = theme::block().title(format!(
        " whales+RSI {} ×{} — {} — {} ",
        e.interval.label(),
        n - start,
        parts.join(" · "),
        tr.w3_chart_title,
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
        // el hover es texto: muestra SIEMPRE los valores del checklist,
        // estén o no dibujadas sus líneas (solo TRIX es opcional aquí)
        let trix_txt = if ind3.trix {
            let v = e.trix[i];
            if v.is_finite() {
                format!(" · TRIX {v:+.1}")
            } else {
                " · TRIX —".to_string()
            }
        } else {
            String::new()
        };
        block = block.title_bottom(format!(
            " {} · RSI {} · MA {} · %B {} · ADX {} · +DI {} · −DI {}{trix_txt} ",
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
    // solo las líneas SELECCIONADAS van al raster (tecla o) — las series se
    // calculan siempre igualmente: checklist, log y ▲▼ no dependen de esto
    let mut lines = Vec::new();
    if ind3.adx_dmi {
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
    if ind3.rsi_ma {
        if let Some((up, lo)) = &panel.rsi_bb {
            lines.push(OscLine {
                vals: up,
                width: 1,
                color: LineColor::Fixed(oscimg::dim_green()),
            });
            lines.push(OscLine {
                vals: lo,
                width: 1,
                color: LineColor::Fixed(oscimg::dim_green()),
            });
        }
    }
    // TRIX opcional, reescalado al eje 0-100 (0→50): debajo de %B/MA/RSI para
    // no competir visualmente con el stack del checklist
    let trix_scaled = ind3
        .trix
        .then(|| super::taplot::scale_zero_centered(&e.trix).0);
    if let Some(ts) = &trix_scaled {
        lines.push(OscLine {
            vals: ts,
            width: 1,
            color: LineColor::Fixed(oscimg::cyan()),
        });
    }
    let zone = |v: f64| oscimg::rsi_zone_rgb(v, &wp);
    if ind3.mod_b {
        lines.push(OscLine {
            vals: &panel.mod_rsi,
            width: 1,
            color: LineColor::Fixed(oscimg::blue()),
        });
    }
    if ind3.rsi_ma {
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
    let bars = panel
        .triggers
        .iter()
        .map(|t| {
            let col = match t.side {
                WhaleSide::Buy => oscimg::bar_buy(),
                WhaleSide::Sell => oscimg::bar_sell(),
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
        segs: vec![],
    };
    oscimg::draw_into(
        f,
        chart,
        gfx,
        oscimg::OscSlot::WhaleRsi,
        e.stamp,
        ind3.mask(),
        spec,
    );

    // eje 0-100 con los niveles y el RSI actual resaltado
    let h = axis.height as usize;
    let row_of = |v: f64| -> usize {
        (((100.0 - v) / 100.0 * h as f64) - 0.5)
            .round()
            .clamp(0.0, h as f64 - 1.0) as usize
    };
    let mut labels: Vec<Line> = vec![Line::raw(""); h];
    for (v, c) in [
        (100.0, theme::c().muted),
        (wp.overbought, theme::c().negative_dim),
        (50.0, theme::c().muted),
        (wp.oversold, theme::c().positive_dim),
        (0.0, theme::c().muted),
    ] {
        labels[row_of(v)] = Line::from(Span::styled(format!("{v:.0}"), Style::new().fg(c)));
    }
    // el marcador ▶ del eje acompaña a la LÍNEA de RSI: sin ella no señala
    // nada (el valor sigue siempre visible en el resumen y el hover)
    if ind3.rsi_ma {
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

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use crate::app::App;
    use crate::data::types::{CandlePoint, CtxSnapshot, DataMsg, Interval, PairMeta};
    use std::time::Instant;

    /// App con BTC seleccionado y ~90 velas sintéticas cargadas (vía el mismo
    /// DataMsg::PairExtra del camino real), en Vista 3, sin tty (halfblocks).
    fn app_vista3() -> App {
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
            super::oscimg::Gfx::new(),
        );
        let meta = PairMeta {
            name: "BTC".into(),
            sz_decimals: 5,
            max_leverage: 40,
        };
        let snap = CtxSnapshot {
            t: Instant::now(),
            t_ms: 0,
            mark_px: 100_000.0,
            mid_px: Some(100_000.0),
            oracle_px: 100_000.0,
            funding: 0.0,
            open_interest: 0.0,
            premium: None,
            day_ntl_vlm: 0.0,
            prev_day_px: 100_000.0,
        };
        app.apply_msg(DataMsg::Ctxs(vec![(meta, snap)]));
        app.selected_coin = Some("BTC".into());
        let candles: Vec<CandlePoint> = (0..90)
            .map(|i| {
                let base = 100_000.0 + 800.0 * (i as f64 * 0.35).sin();
                CandlePoint {
                    t_close: 60_000 * (i as u64 + 1),
                    open: base,
                    high: base + 120.0,
                    low: base - 120.0,
                    close: base + 40.0,
                    volume: 1.0,
                }
            })
            .collect();
        app.apply_msg(DataMsg::PairExtra {
            coin: "BTC".into(),
            interval: Interval::M1,
            candles,
            funding_hist: vec![],
        });
        app.view = crate::app::View::WhaleRsi;
        app
    }

    fn frame(term: &mut Terminal<TestBackend>, app: &mut App) -> String {
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

    /// EVIDENCIA del bug reportado (panel no se actualiza tras cerrar el
    /// selector): reproduce la secuencia exacta de teclas y verifica que la
    /// clave de invalidación dispara un re-raster en el frame siguiente al
    /// toggle, y que el título del panel refleja el cambio tras cerrar.
    #[test]
    fn toggle_del_selector_re_rasteriza_el_panel() {
        use crossterm::event::{KeyCode, KeyEvent};
        use std::sync::atomic::Ordering;

        let mut app = app_vista3();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let s0 = frame(&mut term, &mut app);
        assert!(!s0.contains("· TRIX ·"), "TRIX apagado por defecto:\n{s0}");
        let n0 = super::oscimg::RASTER_COUNT.load(Ordering::SeqCst);
        // frame idéntico: el caché debe aguantar (0 rasters nuevos)
        frame(&mut term, &mut app);
        assert_eq!(
            super::oscimg::RASTER_COUNT.load(Ordering::SeqCst),
            n0,
            "sin cambios no debe re-rasterizar"
        );

        // secuencia del usuario: o abre, ↓↓↓ hasta TRIX (fila 3), Enter
        // conmuta, Esc cierra
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        frame(&mut term, &mut app); // frame con el modal abierto
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let s_modal = frame(&mut term, &mut app);
        let n_after_toggle = super::oscimg::RASTER_COUNT.load(Ordering::SeqCst);
        assert!(
            n_after_toggle > n0,
            "la clave de invalidación (selección) debe disparar re-raster \
             en el frame siguiente al toggle, aún con el modal abierto"
        );
        assert!(s_modal.contains("[x]"), "checkbox marcado:\n{s_modal}");
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        let s1 = frame(&mut term, &mut app);
        assert!(
            s1.contains("· TRIX ·"),
            "el título del panel debe reflejar TRIX tras cerrar:\n{s1}"
        );
        // y desactivarlo vuelve a invalidar (k vuelve a la fila de TRIX
        // porque el cursor se resetea al abrir: ↑ desde 0 envuelve a 3)
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        app.handle_key(KeyEvent::from(KeyCode::Up));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        let s2 = frame(&mut term, &mut app);
        assert!(!s2.contains("· TRIX ·"), "toggle de vuelta:\n{s2}");
        assert!(
            super::oscimg::RASTER_COUNT.load(Ordering::SeqCst) > n_after_toggle,
            "apagarlo también re-rasteriza"
        );
    }

    /// Garantía de la libertad total en Vista 3: con TODAS las líneas del
    /// panel ocultas, el checklist de condiciones, el log de disparos y las
    /// marcas ▲▼ siguen calculándose y mostrándose en texto — la selección
    /// solo decide qué trazos van al raster, nunca apaga la lógica.
    #[test]
    fn ocultar_todas_las_lineas_no_apaga_checklist_ni_disparos() {
        let mut app = app_vista3();
        let triggers_antes = app
            .selected_coin
            .as_deref()
            .and_then(|c| app.pairs.get(c))
            .and_then(|p| p.extra.as_ref())
            .map(|e| e.panel.triggers.len())
            .unwrap();
        app.ind3.rsi_ma = false;
        app.ind3.mod_b = false;
        app.ind3.adx_dmi = false;
        app.ind3.trix = false;
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let s = frame(&mut term, &mut app);
        // checklist textual de los 5 filtros por lado, intacto
        assert!(s.contains("▲ long"), "checklist long presente:\n{s}");
        assert!(s.contains("▼ short"), "checklist short presente:\n{s}");
        assert!(s.contains("RSI<40"), "condición RSI del checklist:\n{s}");
        assert!(s.contains("ADX<28"), "condición ADX del checklist:\n{s}");
        // resumen con los valores numéricos (RSI/MA/%B/ADX/±DI) sigue ahí
        assert!(s.contains("RSI "), "valores del resumen:\n{s}");
        // log de disparos con su recuento real (el cálculo no se apagó).
        // El título sale de i18n: se compone igual que en el render, para no
        // volver a clavar el texto de un idioma concreto en el test.
        let tr = crate::i18n::t();
        assert!(
            s.contains(&format!("{} — {triggers_antes} /", tr.w3_log_title)),
            "log de disparos con recuento {triggers_antes}:\n{s}"
        );
        // y las marcas ▲▼ siguen yendo al raster aunque no haya líneas
        let p = app
            .selected_coin
            .as_deref()
            .and_then(|c| app.pairs.get(c))
            .unwrap();
        assert_eq!(
            p.extra.as_ref().unwrap().panel.triggers.len(),
            triggers_antes,
            "los triggers no dependen de la selección"
        );
        // el título del panel mantiene ▲▼ (nunca es ocultable)
        assert!(
            s.contains(tr.w3_whale_marks),
            "marcas siempre en el título:\n{s}"
        );
    }

    /// El toggle de tema (tecla `T`) recolorea TAMBIÉN los paneles de imagen
    /// de las Vistas 2 y 3, no solo el texto: cambiar de tema invalida el
    /// caché del raster (re-rasteriza) y el marco cambia de color. Y el
    /// checklist/las marcas ▲▼ siguen intactos: esto es solo color.
    #[test]
    fn el_toggle_de_tema_recolorea_texto_e_imagen() {
        use crossterm::event::{KeyCode, KeyEvent};
        use std::sync::atomic::Ordering;

        let _g = super::theme::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        super::theme::set_theme(super::theme::Theme::Dark);

        let mut app = app_vista3();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        let borde = |term: &Terminal<TestBackend>| {
            // esquina superior izquierda del panel: color del marco
            term.backend().buffer().cell((0, 1)).unwrap().fg
        };
        let triggers = |app: &App| {
            app.pairs["BTC"]
                .extra
                .as_ref()
                .unwrap()
                .panel
                .triggers
                .len()
        };

        for view in [crate::app::View::Pair, crate::app::View::WhaleRsi] {
            app.view = view;
            frame(&mut term, &mut app);
            frame(&mut term, &mut app); // caché caliente
            let n0 = super::oscimg::RASTER_COUNT.load(Ordering::SeqCst);
            let borde_oscuro = borde(&term);
            let trig0 = triggers(&app);

            app.handle_key(KeyEvent::from(KeyCode::Char('T')));
            assert_eq!(super::theme::theme(), super::theme::Theme::Light);
            frame(&mut term, &mut app);

            assert!(
                super::oscimg::RASTER_COUNT.load(Ordering::SeqCst) > n0,
                "el cambio de tema debe re-rasterizar el panel de {view:?}"
            );
            assert_eq!(borde_oscuro, super::theme::DARK.border);
            assert_eq!(borde(&term), super::theme::LIGHT.border);
            assert_eq!(trig0, triggers(&app), "el color no toca la detección");

            app.handle_key(KeyEvent::from(KeyCode::Char('T')));
            frame(&mut term, &mut app);
            assert_eq!(borde(&term), super::theme::DARK.border);
        }
        super::theme::set_theme(super::theme::Theme::Dark);
    }
    /// Regresión del bug de i18n de la Vista 3: el toggle EN/ES debe cambiar
    /// SU texto, no solo el del resto de la app. Antes de migrarla, esta vista
    /// tenía los literales en español clavados en el código y se quedaba igual
    /// en ambos idiomas.
    #[test]
    fn el_toggle_de_idioma_cambia_los_textos_de_la_vista_3() {
        let _g = crate::ui::theme::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut app = app_vista3();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();

        let prev = crate::i18n::lang();
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let en = frame(&mut term, &mut app);
        crate::i18n::set_lang(crate::i18n::Lang::Es);
        let es = frame(&mut term, &mut app);
        crate::i18n::set_lang(prev);

        // títulos y textos propios de la vista, en cada idioma
        assert!(en.contains("Whale triggers"), "log en inglés:\n{en}");
        assert!(es.contains("Disparos ballena"), "log en español:\n{es}");
        assert!(en.contains("▲▼ whale"), "marcas en inglés:\n{en}");
        assert!(es.contains("▲▼ ballena"), "marcas en español:\n{es}");
        assert!(en.contains("i changes TF"), "atajos en inglés:\n{en}");
        assert!(es.contains("i cambia TF"), "atajos en español:\n{es}");
        assert_ne!(en, es, "la vista debe re-renderizar distinto por idioma");
    }
}

fn draw_log(f: &mut Frame, e: &PairExtraData, area: Rect) {
    let tr = crate::i18n::t();
    let trig = &e.panel.triggers;
    let block = theme::block().title(format!(
        " {} — {} / {} ",
        tr.w3_log_title,
        trig.len(),
        e.candles.len()
    ));
    let dim = |s: &'static str| Span::styled(s, Style::new().fg(theme::c().muted));
    let mut lines: Vec<Line> = Vec::new();
    if trig.is_empty() {
        lines.push(Line::from(dim(tr.w3_log_empty)));
        lines.push(Line::raw(""));
        lines.push(Line::from(dim(tr.w3_log_conditions)));
        lines.push(Line::from(dim(tr.w3_log_cond1)));
        lines.push(Line::from(dim(tr.w3_log_cond2)));
    } else {
        lines.push(Line::from(dim(tr.w3_log_header)));
        let max_rows = (area.height.saturating_sub(3)) as usize;
        for t in trig.iter().rev().take(max_rows.max(1)) {
            let c = &e.candles[t.idx];
            let (arrow, side_txt, color) = match t.side {
                WhaleSide::Buy => ("▲", tr.w3_side_buy, theme::c().positive),
                WhaleSide::Sell => ("▼", tr.w3_side_sell, theme::c().negative),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {arrow} {side_txt} "),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:>4.1} ", t.height)),
                Span::styled(format!("{:>6.2}% ", t.dist_pct), Style::new().fg(color)),
                Span::raw(format!("{:<10} ", fmt_px(c.close))),
                Span::styled(age_label(c.t_close), Style::new().fg(theme::c().muted)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}
