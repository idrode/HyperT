use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, PairState};
use crate::{liq, liqdens};

use super::fmt::{fmt_px, fmt_usd};
use super::theme;

/// Ancho del panel de densidad ΔOI; por debajo solo se muestra el mapa.
const DENS_W: u16 = 46;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if app.selected_pair().is_none() {
        let s = crate::i18n::t();
        f.render_widget(
            Paragraph::new(s.liq_select_pair)
                .block(theme::block().title(format!(" {} ", s.liq_title_word))),
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
    let block = theme::block().title(title);
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
                Style::new().fg(theme::c().accent_amber),
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
            Style::new().fg(theme::c().neutral),
        )];
        // combustible de LONGS (se liquidan por debajo del mark) en el verde
        // de marca; de SHORTS (por encima) en el coral. La intensidad de cada
        // tramo va por su notional relativo al bucket más cargado: barra
        // larga = tono saturado, barra corta = tono apagado del mismo lado.
        spans.push(Span::styled(
            "█".repeat(long_part),
            Style::new().fg(theme::liq_bar(true, b.long_est / max_val)),
        ));
        spans.push(Span::styled(
            "█".repeat(short_part),
            Style::new().fg(theme::liq_bar(false, b.short_est / max_val)),
        ));
        spans.push(Span::raw(" ".repeat(bar_w.saturating_sub(filled) + 1)));
        if total > max_val * 0.005 {
            spans.push(Span::styled(
                format!("{:>8}", fmt_usd(total)),
                Style::new().fg(theme::c().muted),
            ));
        }
        if b.whale_ntl > 0.0 {
            spans.push(Span::styled(
                format!(" ◆{}", fmt_usd(b.whale_ntl)),
                // liquidaciones REALES de whales: fuera de la escala
                // verde/coral del combustible estimado a propósito, para que
                // no se confundan con el gradiente de fondo
                Style::new()
                    .fg(theme::c().fg_strong)
                    .add_modifier(Modifier::BOLD),
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
    let block = theme::block().title(s.liq_dens_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 3 || inner.width < 30 {
        return;
    }
    let dim = |s: String| Span::styled(s, Style::new().fg(theme::c().muted));

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
        // por encima del mark se liquidan shorts (coral), por debajo longs
        // (verde de marca) — misma semántica que las barras del mapa
        let frac = b.count as f64 / max_count as f64;
        let color = theme::liq_bar(px < mark, frac);
        let filled = ((bar_w as u32 * b.count + max_count / 2) / max_count) as usize;
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:>11} ", fmt_px(px)),
                Style::new().fg(theme::c().neutral),
            ),
            Span::raw(format!("{:>3} ", b.count)),
            Span::styled(format!("{dist:+7.2}% "), Style::new().fg(color)),
            Span::styled("█".repeat(filled.min(bar_w)), Style::new().fg(color)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    use crate::app::App;
    use crate::data::types::{
        CandlePoint, CtxSnapshot, DataMsg, Interval, PairMeta, PosInfo, WhaleInfo,
    };
    use std::time::Instant;

    use super::theme;

    /// App en Vista 5 con BTC, velas sintéticas y una whale con liquidación
    /// conocida (para que aparezca el ◆).
    fn app_vista5() -> App {
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
            open_interest: 500.0,
            premium: None,
            day_ntl_vlm: 1e9,
            prev_day_px: 100_000.0,
        };
        app.apply_msg(DataMsg::Ctxs(vec![(meta, snap)]));
        app.selected_coin = Some("BTC".into());
        let candles: Vec<CandlePoint> = (0..90)
            .map(|i| {
                let base = 100_000.0 + 2_000.0 * (i as f64 * 0.35).sin();
                CandlePoint {
                    t_close: 60_000 * (i as u64 + 1),
                    open: base,
                    high: base + 500.0,
                    low: base - 500.0,
                    close: base + 100.0,
                    volume: 20.0,
                }
            })
            .collect();
        app.apply_msg(DataMsg::PairExtra {
            coin: "BTC".into(),
            interval: Interval::M1,
            candles,
            funding_hist: vec![],
        });
        app.whales = vec![WhaleInfo {
            addr: "0xdead".into(),
            account_value: 1e7,
            positions: vec![PosInfo {
                coin: "BTC".into(),
                szi: 10.0,
                entry_px: Some(100_000.0),
                position_value: 1e6,
                unrealized_pnl: 0.0,
                roe: 0.0,
                leverage: 10,
                is_cross: true,
                liq_px: Some(97_000.0),
                since_open_funding: 0.0,
            }],
        }];
        app.view = crate::app::View::Liq;
        app
    }

    /// Las barras del mapa hablan el idioma del tema: combustible de longs en
    /// el verde de marca, de shorts en el coral, con gradiente por notional —
    /// y los ◆ de whales siguen distinguiéndose de ese gradiente. La etiqueta
    /// ESTIMADO no se pierde por el camino.
    #[test]
    fn el_mapa_usa_las_dos_familias_y_los_rombos_destacan() {
        let _g = theme::TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for th in [theme::Theme::Dark, theme::Theme::Light] {
            theme::set_theme(th);
            let mut app = app_vista5();
            let mut term = Terminal::new(TestBackend::new(120, 30)).unwrap();
            term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer().clone();

            let mut txt = String::new();
            let mut barras: Vec<Color> = Vec::new();
            let mut rombos: Vec<Color> = Vec::new();
            let mut vista_rombo = false;
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    let cell = buf.cell((x, y)).unwrap();
                    txt.push_str(cell.symbol());
                    match cell.symbol() {
                        "█" => barras.push(cell.fg),
                        // el ◆ del título es la leyenda, no un dato: solo
                        // interesan los del cuerpo del mapa
                        "◆" if y > 1 => {
                            vista_rombo = true;
                            rombos.push(cell.fg);
                        }
                        _ => {}
                    }
                }
                txt.push('\n');
            }

            assert!(
                txt.contains(crate::i18n::t().liq_estimate_note.trim()),
                "la etiqueta ESTIMADO sigue en el título:\n{txt}"
            );
            assert!(!barras.is_empty(), "sin barras que comprobar:\n{txt}");
            assert!(vista_rombo, "sin ◆ de whale que comprobar:\n{txt}");

            // cada barra es un paso del gradiente de UNA de las dos familias
            let rgb = |c: Color| match c {
                Color::Rgb(r, g, b) => [r as i32, g as i32, b as i32],
                other => panic!("color no RGB en la barra: {other:?}"),
            };
            // la fracción es continua, así que se busca el paso más cercano
            // del gradiente de cada familia y se exige que uno de los dos
            // clave (±1 por canal, redondeo de la mezcla)
            let familia = |c: Color| {
                let x = rgb(c);
                let cerca = |long: bool| {
                    (0..=1000)
                        .map(|k| rgb(theme::liq_bar(long, k as f64 / 1000.0)))
                        .map(|y| (0..3).map(|i| (x[i] - y[i]).abs()).max().unwrap())
                        .min()
                        .unwrap()
                };
                let (dl, ds) = (cerca(true), cerca(false));
                assert!(
                    dl.min(ds) <= 1,
                    "color de barra fuera del gradiente: {c:?} (long {dl}, short {ds})"
                );
                dl < ds
            };
            let lados: Vec<bool> = barras.iter().map(|c| familia(*c)).collect();
            assert!(lados.contains(&true), "falta combustible de longs (verde)");
            assert!(
                lados.contains(&false),
                "falta combustible de shorts (coral)"
            );

            // el ◆ no se confunde con ninguna barra
            for r in &rombos {
                assert_eq!(*r, theme::c().fg_strong);
                assert!(!barras.contains(r), "el ◆ se confunde con una barra");
            }
        }
        theme::set_theme(theme::Theme::Dark);
    }
}
