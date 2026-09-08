use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

use crate::app::{App, SortCol, OI_WIN_LONG, OI_WIN_SHORT};

use super::fmt::{fmt_opt, fmt_opt_pct, fmt_px, fmt_usd};
use super::shadow;
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

    if app.quick_score {
        let coin = if searching {
            coins.get(app.search.sel).cloned()
        } else {
            coins.get(app.sel).cloned()
        };
        if let Some(coin) = coin {
            draw_quick_score(f, app, &coin, area);
        }
    }
}

// ── PROTOTIPO: vista rápida del score (tecla `v`) ─────────────────────────
// Panel flotante anclado a la IZQUIERDA sobre la tabla, no un modal centrado.
// Fuente de datos: el score compuesto YA EXISTENTE de la Vista 6 — aquí no se
// calcula nada nuevo, solo se presenta. Cuando el score ponderado sustituya al
// de conteo, cambia la fuente y no el widget.
// Es un punto de partida para iterar sobre el aspecto, no la versión final.

const QUICK_W: u16 = 34;
const QUICK_H: u16 = 9;

fn draw_quick_score(f: &mut Frame, app: &App, coin: &str, area: Rect) {
    if area.width < QUICK_W + shadow::DX + 2 || area.height < QUICK_H + shadow::DY + 2 {
        return;
    }
    let r = Rect::new(area.x + 1, area.y + 2, QUICK_W, QUICK_H);
    let s = crate::flow::score(&app.score_inputs(coin));

    // titular: UN número claro. Sesgo = margen entre lados sobre los
    // componentes CON DATO (el denominador honesto va aparte, abajo).
    let pct = if s.avail == 0 {
        0.0
    } else {
        (s.bull as f64 - s.bear as f64) / s.avail as f64 * 100.0
    };
    let p = theme::c();
    let (titular, color) = if s.avail == 0 {
        ("—".to_string(), p.muted)
    } else if pct.abs() < 20.0 {
        ("NEUTRAL".to_string(), p.neutral)
    } else {
        let fuerza = if pct.abs() >= 60.0 {
            "fuerte"
        } else {
            "moderado"
        };
        let lado = if pct > 0.0 { "LONG" } else { "SHORT" };
        (
            format!("sesgo {lado} {fuerza}"),
            theme::bias_fg(pct / 100.0),
        )
    };

    shadow::draw(f, r);
    f.render_widget(ratatui::widgets::Clear, r);
    let block = theme::block().title(Span::styled(format!(" ⌁ {coin} "), theme::title_style()));
    let inner = block.inner(r);
    f.render_widget(block, r);
    if inner.width < 8 || inner.height < 5 {
        return;
    }

    let w = inner.width as usize;
    let barra = score_bar(pct, w);

    let num = if s.avail == 0 {
        "—".to_string()
    } else {
        format!("{pct:+.0}%")
    };
    // sin ningún componente con dato no hay sesgo que enseñar: el mismo
    // mensaje de warmup que ya usa la Vista 6, no un guion sin explicación
    let titular = if s.avail == 0 {
        crate::i18n::t().fl_no_components.to_string()
    } else {
        titular
    };
    let lines = vec![
        Line::from(Span::styled(
            format!("{num:^w$}"),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("{titular:^w$}"),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(barra, Style::new().fg(color))),
        Line::raw(""),
        // desglose SECUNDARIO: está, pero no compite con el titular
        Line::from(vec![
            Span::styled("▼", Style::new().fg(p.negative)),
            Span::styled(format!("{} ", s.bear), Style::new().fg(p.muted)),
            Span::styled("▲", Style::new().fg(p.positive)),
            Span::styled(format!("{} ", s.bull), Style::new().fg(p.muted)),
            Span::styled(
                format!("· {} de {} señales", s.avail, crate::flow::SCORE_COMPONENTS),
                Style::new().fg(p.muted),
            ),
        ]),
        Line::from(Span::styled(
            "6 = detalle · v/Esc cierra",
            Style::new().fg(p.muted),
        )),
    ];
    f.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
}

/// Barra de sesgo −100 … +100 con el cero fijo en el centro, de ancho `w`.
///
/// El radio es lo que cabe al lado MÁS CORTO del centro: con un ancho par el
/// hueco de la derecha tiene una celda menos, y llenar `mid + mid` se salía
/// del buffer (panic real: "index out of bounds: the len is 32 but the index
/// is 32" al abrir el panel con un sesgo del 100%). Aparte del cálculo
/// correcto, todo se escribe con `get_mut`: un panel decorativo no puede
/// tumbar la app pase lo que pase con el ancho.
fn score_bar(pct: f64, w: usize) -> String {
    let mid = w.saturating_sub(1) / 2;
    let radio = mid.min(w.saturating_sub(1).saturating_sub(mid));
    let llenos = ((pct.abs().min(100.0) / 100.0) * radio as f64).round() as usize;
    let mut barra = vec![' '; w];
    if let Some(cel) = barra.get_mut(mid) {
        *cel = '│';
    }
    for k in 1..=llenos.min(radio) {
        let i = if pct >= 0.0 {
            mid + k
        } else {
            mid.saturating_sub(k)
        };
        if let Some(cel) = barra.get_mut(i) {
            *cel = '█';
        }
    }
    barra.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::score_bar;

    /// La barra nunca se sale del buffer, sea cual sea el ancho o el sesgo —
    /// el panic original era exactamente esto (ancho 32, sesgo +100%).
    #[test]
    fn la_barra_de_sesgo_cabe_siempre() {
        for w in 0..64usize {
            for pct in [-200.0, -100.0, -99.9, -50.0, 0.0, 50.0, 99.9, 100.0, 200.0] {
                let b = score_bar(pct, w);
                assert_eq!(b.chars().count(), w, "ancho {w}, sesgo {pct}");
            }
        }
    }

    /// Y dice lo que tiene que decir: cero centrado, y el relleno crece hacia
    /// el lado del sesgo sin invadir el otro.
    #[test]
    fn la_barra_llena_el_lado_correcto() {
        let w = 32;
        let mid = (w - 1) / 2;
        let cero: Vec<char> = score_bar(0.0, w).chars().collect();
        assert_eq!(cero[mid], '│');
        assert!(!cero.contains(&'█'), "sin sesgo no hay relleno");

        let arriba: Vec<char> = score_bar(100.0, w).chars().collect();
        assert!(
            arriba[mid + 1..].contains(&'█'),
            "el sesgo LONG va a la derecha"
        );
        assert!(!arriba[..mid].contains(&'█'), "y no invade la izquierda");

        let abajo: Vec<char> = score_bar(-100.0, w).chars().collect();
        assert!(
            abajo[..mid].contains(&'█'),
            "el sesgo SHORT va a la izquierda"
        );
        assert!(!abajo[mid + 1..].contains(&'█'), "y no invade la derecha");
    }
}
