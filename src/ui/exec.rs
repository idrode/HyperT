//! Render del panel de ejecución (Vista 8) — maqueta interactiva sin envío
//! real. Cada control registra su rect en el hitmap del ratón; todo control
//! tiene además su camino de teclado equivalente (ver app::handle_funds_key):
//! ningún elemento es solo-ratón ni solo-teclado.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, Wrap};

use crate::app::App;
use crate::exec::{self, Confirm, ExecState, Focus, Hit, OrdType, Side, SizeUnit, SlTpEdit};

use super::fmt::{fmt_px, fmt_usd, sign_color};

const SLIDER_W: u16 = 16;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let (coin, mid, max_lev, sz_dec) = match app.selected_pair() {
        Some(p) => (
            p.meta.name.clone(),
            p.mid,
            p.meta.max_leverage,
            p.meta.sz_decimals,
        ),
        None => ("—".to_string(), 0.0, 40, 4),
    };

    // margen disponible real según el modo de cuenta (unificada → spot;
    // estándar → withdrawable de perps); None = aún sin leer o sin sesión
    let avail = app.perps_avail().map(|a| (a, app.is_unified()));
    let cols = Layout::horizontal([Constraint::Length(46), Constraint::Min(24)]).split(area);
    draw_form(
        f,
        &app.exec,
        &coin,
        mid,
        max_lev,
        sz_dec,
        avail,
        app.net_label,
        cols[0],
        &mut hits,
    );
    draw_right(f, app, cols[1], &mut hits);

    // con un modal abierto solo sus controles son clicables
    if app.exec.confirm.is_some() || app.exec.sltp.is_some() {
        hits.clear();
    }
    if let Some(m) = app.exec.sltp.clone() {
        app.overlay_drawn.set(true);
        draw_sltp(f, app, &m, &mut hits);
    }
    if let Some(c) = app.exec.confirm.clone() {
        app.overlay_drawn.set(true);
        draw_confirm(f, app, &c, &mut hits);
    }
    app.exec.hits.extend(hits);
}

/// Constructor de una línea con rects exactos por tramo, para el hitmap.
struct LineB {
    spans: Vec<Span<'static>>,
    x: u16,
    y: u16,
    x0: u16,
    w: u16,
}

impl LineB {
    fn new(area: Rect, row: u16) -> Self {
        Self {
            spans: Vec::new(),
            x: area.x,
            y: area.y + row,
            x0: area.x,
            w: area.width,
        }
    }

    fn push(&mut self, s: impl Into<String>, st: Style) -> Rect {
        let s: String = s.into();
        let w = s.chars().count() as u16;
        let r = Rect::new(self.x, self.y, w, 1);
        self.spans.push(Span::styled(s, st));
        self.x = self.x.saturating_add(w);
        r
    }

    fn render(self, f: &mut Frame) {
        let fa = f.area();
        if self.y >= fa.bottom() || self.x0 >= fa.right() {
            return;
        }
        let w = self.w.min(fa.right() - self.x0);
        f.render_widget(
            Paragraph::new(Line::from(self.spans)),
            Rect::new(self.x0, self.y, w, 1),
        );
    }
}

fn dim() -> Style {
    Style::new().fg(Color::DarkGray)
}

fn btn() -> Style {
    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

fn label(b: &mut LineB, txt: &str, focused: bool) -> Rect {
    let st = if focused {
        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        dim()
    };
    let r0 = b.push(if focused { "▸ " } else { "  " }, st);
    let r1 = b.push(format!("{txt:<12}"), st);
    Rect::new(r0.x, r0.y, r0.width + r1.width, 1)
}

fn chip(b: &mut LineB, txt: &str, on: bool, color: Color) -> Rect {
    let st = if on {
        Style::new()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::Gray)
    };
    b.push(format!(" {txt} "), st)
}

/// Input de texto de una línea; el rect devuelto incluye colchón clicable.
fn input(b: &mut LineB, val: &str, focused: bool, editing: bool, placeholder: &str) -> Rect {
    let x0 = b.x;
    if val.is_empty() && !editing {
        b.push(placeholder, dim().add_modifier(Modifier::ITALIC));
    } else {
        let st = if focused {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        b.push(val, st);
    }
    if editing {
        b.push("▏", Style::new().fg(Color::Cyan));
    }
    let pad = 14u16.saturating_sub(b.x - x0);
    if pad > 0 {
        b.push(" ".repeat(pad as usize), Style::new());
    }
    Rect::new(x0, b.y, b.x - x0, 1)
}

#[allow(clippy::too_many_arguments)]
fn draw_form(
    f: &mut Frame,
    st: &ExecState,
    coin: &str,
    mid: f64,
    max_lev: usize,
    sz_dec: u32,
    // (margen disponible, ¿cuenta unificada?) — None = aún sin leer
    avail: Option<(f64, bool)>,
    // red real de la sesión: el título decía "testnet" fijo aunque el panel
    // ya opera también contra mainnet (paso 7.5)
    net: &str,
    area: Rect,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let tr = crate::i18n::t();
    let block = if st.real {
        Block::bordered()
            .title(tr.ex_title_real.replacen("{}", net, 1))
            .border_style(Style::new().fg(Color::Red))
    } else {
        Block::bordered()
            .title(tr.ex_title_mock)
            .border_style(Style::new().fg(Color::Yellow))
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 6 || inner.width < 34 {
        return;
    }
    let mut hit = |r: Rect, h: Hit, inner: Rect| {
        let c = r.intersection(inner);
        if c.width > 0 && c.height > 0 {
            hits.push((c, h));
        }
    };
    let foc = st.focus;
    let long = st.side.is_long();
    let entry_est = match st.typ {
        OrdType::Market => (mid > 0.0).then_some(mid),
        OrdType::Limit => exec::parse_num(&st.limit_px),
    };
    let px_conv = entry_est.or((mid > 0.0).then_some(mid));
    let sizes = exec::parse_num(&st.size)
        .and_then(|v| px_conv.and_then(|px| exec::size_both(v, st.unit, px)));

    let mut r = 0u16;

    // Par
    {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_pair, foc == Focus::Pair);
        hit(lr, Hit::Focus(Focus::Pair), inner);
        let hr = b.push("‹", btn());
        hit(hr, Hit::PairStep(-1), inner);
        b.push(" ", Style::new());
        b.push(coin, Style::new().add_modifier(Modifier::BOLD));
        b.push(" ", Style::new());
        let hr = b.push("›", btn());
        hit(hr, Hit::PairStep(1), inner);
        b.push(tr.ex_max_lev.replacen("{}", &max_lev.to_string(), 1), dim());
        b.render(f);
        r += 1;
    }
    // Lado
    {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_side, foc == Focus::Side);
        hit(lr, Hit::Focus(Focus::Side), inner);
        let hr = chip(&mut b, "LONG", long, Color::Green);
        hit(hr, Hit::SetSide(Side::Long), inner);
        b.push("  ", Style::new());
        let hr = chip(&mut b, "SHORT", !long, Color::Red);
        hit(hr, Hit::SetSide(Side::Short), inner);
        b.render(f);
        r += 1;
    }
    // Apalancamiento (stepper + slider clicable/arrastrable)
    {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_lev, foc == Focus::Lev);
        hit(lr, Hit::Focus(Focus::Lev), inner);
        let hr = b.push(" − ", btn());
        hit(hr, Hit::LevStep(-1), inner);
        let max = max_lev.max(1) as u32;
        let pos = if max > 1 {
            ((st.lev.saturating_sub(1)) as f64 / (max - 1) as f64 * (SLIDER_W - 1) as f64).round()
                as u16
        } else {
            0
        };
        let track: String = (0..SLIDER_W)
            .map(|i| if i == pos { '●' } else { '─' })
            .collect();
        let hr = b.push(track, Style::new().fg(Color::Cyan));
        hit(hr, Hit::LevSlider, inner);
        let hr = b.push(" + ", btn());
        hit(hr, Hit::LevStep(1), inner);
        match &st.lev_edit {
            Some(buf) => {
                b.push(format!(" {buf}"), Style::new().add_modifier(Modifier::BOLD));
                b.push("▏", Style::new().fg(Color::Cyan));
                b.push("×", Style::new());
            }
            None => {
                b.push(
                    format!(" {}×", st.lev),
                    Style::new().add_modifier(Modifier::BOLD),
                );
            }
        }
        b.render(f);
        r += 1;
    }
    // Tipo
    {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_type, foc == Focus::OrdType);
        hit(lr, Hit::Focus(Focus::OrdType), inner);
        let hr = chip(&mut b, tr.ex_market, st.typ == OrdType::Market, Color::Cyan);
        hit(hr, Hit::SetType(OrdType::Market), inner);
        b.push("  ", Style::new());
        let hr = chip(&mut b, tr.ex_limit, st.typ == OrdType::Limit, Color::Cyan);
        hit(hr, Hit::SetType(OrdType::Limit), inner);
        b.render(f);
        r += 1;
    }
    // Precio límite (solo con orden límite)
    if st.typ == OrdType::Limit {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_limit_px, foc == Focus::LimitPx);
        hit(lr, Hit::Edit(Focus::LimitPx), inner);
        let editing = st.editing && foc == Focus::LimitPx;
        let hr = input(
            &mut b,
            &st.limit_px,
            foc == Focus::LimitPx,
            editing,
            tr.ex_ph_price,
        );
        hit(hr, Hit::Edit(Focus::LimitPx), inner);
        if !st.limit_px.is_empty() && exec::parse_num(&st.limit_px).is_none() {
            b.push(tr.ex_invalid, Style::new().fg(Color::Red));
        } else if let (Some(px), true) = (exec::parse_num(&st.limit_px), mid > 0.0) {
            b.push(
                format!("({:+.2}{})", (px / mid - 1.0) * 100.0, tr.ex_of_mid),
                dim(),
            );
        }
        b.render(f);
        r += 1;
    }
    // Tamaño + unidad
    {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, tr.ex_size, foc == Focus::Size);
        hit(lr, Hit::Edit(Focus::Size), inner);
        let editing = st.editing && foc == Focus::Size;
        let hr = input(&mut b, &st.size, foc == Focus::Size, editing, tr.ex_ph_size);
        hit(hr, Hit::Edit(Focus::Size), inner);
        let hr = chip(&mut b, "USD", st.unit == SizeUnit::Usd, Color::Cyan);
        hit(hr, Hit::SetUnit(SizeUnit::Usd), inner);
        b.push(" ", Style::new());
        let hr = chip(&mut b, coin, st.unit == SizeUnit::Asset, Color::Cyan);
        hit(hr, Hit::SetUnit(SizeUnit::Asset), inner);
        b.render(f);
        r += 1;
    }
    // conversión en vivo del tamaño
    {
        let mut b = LineB::new(inner, r);
        b.push("              ", Style::new());
        match sizes {
            Some((usd, asset)) => {
                b.push(
                    format!("≈ {:.*} {coin}  (${:.2})", sz_dec as usize, asset, usd),
                    dim(),
                );
            }
            None => {
                b.push("≈ —", dim());
            }
        }
        b.render(f);
        r += 1;
    }
    // SL / TP
    for (fo, val, name, is_sl) in [
        (Focus::Sl, &st.sl, tr.ex_sl, true),
        (Focus::Tp, &st.tp, tr.ex_tp, false),
    ] {
        let mut b = LineB::new(inner, r);
        let lr = label(&mut b, name, foc == fo);
        hit(lr, Hit::Edit(fo), inner);
        let editing = st.editing && foc == fo;
        let hr = input(&mut b, val, foc == fo, editing, tr.ex_ph_trigger);
        hit(hr, Hit::Edit(fo), inner);
        if let Some(e) = entry_est {
            match exec::parse_trigger(val, e, long, is_sl) {
                Ok(None) => {}
                Ok(Some(px)) => {
                    let pct = (px / e - 1.0) * 100.0;
                    match exec::trigger_side_err(px, e, long, is_sl) {
                        None => {
                            // sin paréntesis: la columna es estrecha (46)
                            b.push(format!("→ {} {pct:+.1}%", fmt_px(px)), dim());
                        }
                        Some(err) => {
                            b.push(format!("✗ {err}"), Style::new().fg(Color::Red));
                        }
                    }
                }
                Err(err) => {
                    b.push(format!("✗ {err}"), Style::new().fg(Color::Red));
                }
            }
        }
        b.render(f);
        r += 1;
    }
    r += 1; // separador
            // resumen de riesgo — siempre visible ANTES de confirmar
    {
        let mut b = LineB::new(inner, r);
        b.push(tr.ex_entry_est, dim());
        b.push(
            entry_est.map(fmt_px).unwrap_or_else(|| "—".into()),
            Style::new().add_modifier(Modifier::BOLD),
        );
        b.render(f);
        r += 1;
    }
    {
        let liq = entry_est.and_then(|e| exec::liq_price(e, st.lev, max_lev, long));
        let mut b = LineB::new(inner, r);
        b.push(tr.ex_liq_est, dim());
        match (liq, entry_est) {
            (Some(l), Some(e)) => {
                b.push(
                    format!("{} ({:+.1}%)", fmt_px(l), (l / e - 1.0) * 100.0),
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                );
                b.push(tr.ex_isolated, dim());
            }
            _ => {
                b.push("—", dim());
            }
        }
        b.render(f);
        r += 1;
    }
    {
        let mut b = LineB::new(inner, r);
        b.push(tr.ex_margin_req, dim());
        match sizes {
            Some((usd, _)) => {
                b.push(format!("${:.2}", usd / st.lev.max(1) as f64), Style::new());
                b.push(format!("  ({}× / ${usd:.0})", st.lev), dim());
            }
            None => {
                b.push("—", dim());
            }
        }
        b.render(f);
        r += 1;
    }
    // margen disponible real de la cuenta conectada, con su fuente según el
    // modo: en cuenta unificada el clearinghouseState de perps diría 0 y la
    // cifra correcta viene del saldo spot (fuente de verdad única)
    {
        let mut b = LineB::new(inner, r);
        b.push(tr.ex_margin_avail, dim());
        match avail {
            Some((a, unified)) => {
                let req = sizes.map(|(usd, _)| usd / st.lev.max(1) as f64);
                let st_a = if req.is_some_and(|req| req > a) {
                    Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                b.push(format!("${a:.2}"), st_a);
                // la columna es estrecha (46): etiquetas cortas
                b.push(
                    if unified { tr.ex_unified_spot } else { tr.ex_perps },
                    dim(),
                );
                if req.is_some_and(|req| req > a) {
                    b.push(tr.ex_insufficient, Style::new().fg(Color::Red));
                }
            }
            None => {
                b.push("—", dim());
            }
        }
        b.render(f);
        r += 1;
    }
    r += 1;
    // botón de revisión (la confirmación real es el modal con resumen)
    {
        let mut b = LineB::new(inner, r);
        b.push("  ", Style::new());
        let bs = if foc == Focus::Submit {
            Style::new()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
        };
        let hr = b.push(tr.ex_review, bs);
        hit(hr, Hit::Submit, inner);
        b.render(f);
        r += 1;
    }
    if let Some(e) = &st.err {
        let left = inner.height.saturating_sub(r);
        if left > 0 {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!("  ✗ {e}"),
                    Style::new().fg(Color::Red),
                ))
                .wrap(Wrap { trim: false }),
                Rect::new(inner.x, inner.y + r, inner.width, left),
            );
        }
    }
}

fn draw_right(f: &mut Frame, app: &App, area: Rect, hits: &mut Vec<(Rect, Hit)>) {
    let tr = crate::i18n::t();
    let st = &app.exec;
    let ord_h = (st.orders.len() as u16 + 3).clamp(4, area.height / 2);
    let rows = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(ord_h),
        Constraint::Length(1),
    ])
    .split(area);

    // ── posiciones abiertas (mock) con mark/PnL en vivo ──
    let header = Row::new(vec![
        tr.ex_pair,
        tr.ex_side,
        tr.ex_size,
        tr.wa_col_entry,
        tr.ex_col_mark,
        tr.ex_col_liq,
        tr.ex_col_lev,
        tr.ex_col_upnl,
        tr.ex_col_roe,
        "SL",
        "TP",
    ])
    .style(Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD));
    let body: Vec<Row> = st
        .positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let long = p.is_long();
            let mark = app.pairs.get(&p.coin).map(|x| x.mid).unwrap_or(0.0);
            let maxl = app
                .pairs
                .get(&p.coin)
                .map(|x| x.meta.max_leverage)
                .unwrap_or(40);
            // real: la liquidación EXACTA que reporta la API; maqueta: estimada
            let liq = p.liq.or_else(|| exec::liq_price(p.entry, p.lev, maxl, long));
            let (pnl, roe) = exec::pos_pnl(p, mark).unzip();
            let opt = |v: Option<f64>| v.map(fmt_px).unwrap_or_else(|| "—".into());
            let mut row = Row::new(vec![
                Cell::from(p.coin.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(if long { "LONG" } else { "SHORT" }).style(
                    Style::new()
                        .fg(if long { Color::Green } else { Color::Red })
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(format!("{:+.4}", p.szi)),
                Cell::from(fmt_px(p.entry)),
                Cell::from(fmt_px(mark)),
                Cell::from(opt(liq)).style(Style::new().fg(Color::Yellow)),
                Cell::from(format!("{}×", p.lev)),
                Cell::from(
                    pnl.map(|v| format!("{v:+.2}"))
                        .unwrap_or_else(|| "—".into()),
                )
                .style(Style::new().fg(sign_color(pnl, false))),
                Cell::from(
                    roe.map(|v| format!("{v:+.1}"))
                        .unwrap_or_else(|| "—".into()),
                )
                .style(Style::new().fg(sign_color(roe, false))),
                Cell::from(opt(p.sl)),
                Cell::from(opt(p.tp)),
            ]);
            if st.focus == Focus::Pos(i) {
                row = row.style(Style::new().bg(Color::DarkGray));
            }
            row
        })
        .collect();
    let widths = [
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let title = panel_title(
        st.real,
        tr.ex_positions,
        st.positions.iter().filter(|p| p.demo).count(),
        st.positions.len(),
    );
    f.render_widget(
        Table::new(body, widths)
            .header(header)
            .block(Block::bordered().title(title)),
        rows[0],
    );
    let tin = Block::bordered().inner(rows[0]);
    for i in 0..st.positions.len() {
        let y = tin.y + 1 + i as u16;
        if y < tin.bottom() {
            hits.push((Rect::new(tin.x, y, tin.width, 1), Hit::Focus(Focus::Pos(i))));
        }
    }

    // botones de la posición enfocada (equivalentes de x / e)
    {
        let on_pos = matches!(st.focus, Focus::Pos(_));
        let bs = if on_pos {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            dim()
        };
        let mut b = LineB::new(rows[1], 0);
        b.push("  ", Style::new());
        let hr = b.push(tr.ex_close_market_btn, bs);
        hits.push((hr.intersection(rows[1]), Hit::ClosePos));
        b.push("  ", Style::new());
        let hr = b.push(tr.ex_edit_sltp_btn, bs);
        hits.push((hr.intersection(rows[1]), Hit::EditSlTp));
        if !on_pos {
            b.push(tr.ex_focus_pos_hint, dim());
        }
        b.render(f);
    }

    // ── órdenes abiertas (límite + triggers SL/TP) ──
    let header = Row::new(vec![
        tr.ex_pair,
        tr.ex_type,
        tr.ex_side,
        tr.ex_col_price,
        tr.ex_size,
        tr.ex_col_ntl,
    ])
        .style(Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD));
    let body: Vec<Row> = st
        .orders
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let long = o.side == Side::Long;
            let mut row = Row::new(vec![
                Cell::from(o.coin.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(o.kind.label()),
                Cell::from(o.side.label()).style(Style::new().fg(if long {
                    Color::Green
                } else {
                    Color::Red
                })),
                Cell::from(fmt_px(o.px)),
                Cell::from(format!("{:.4}", o.sz)),
                Cell::from(fmt_usd(o.px * o.sz)),
            ]);
            if st.focus == Focus::Ord(i) {
                row = row.style(Style::new().bg(Color::DarkGray));
            }
            row
        })
        .collect();
    let widths = [
        Constraint::Length(6),
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(8),
    ];
    let title = panel_title(
        st.real,
        tr.ex_open_orders,
        st.orders.iter().filter(|o| o.demo).count(),
        st.orders.len(),
    );
    f.render_widget(
        Table::new(body, widths)
            .header(header)
            .block(Block::bordered().title(title)),
        rows[2],
    );
    let oin = Block::bordered().inner(rows[2]);
    for i in 0..st.orders.len() {
        let y = oin.y + 1 + i as u16;
        if y < oin.bottom() {
            hits.push((Rect::new(oin.x, y, oin.width, 1), Hit::Focus(Focus::Ord(i))));
        }
    }

    // botón de cancelación + línea de estado
    {
        let on_ord = matches!(st.focus, Focus::Ord(_));
        let bs = if on_ord {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            dim()
        };
        let mut b = LineB::new(rows[3], 0);
        b.push("  ", Style::new());
        let hr = b.push(tr.ex_cancel_ord_btn, bs);
        hits.push((hr.intersection(rows[3]), Hit::CancelOrd));
        b.push("  ", Style::new());
        if let Some(e) = &st.err {
            b.push(format!("✗ {e}"), Style::new().fg(Color::Red));
        } else if let Some(s) = &st.status {
            b.push(format!("✓ {s}"), Style::new().fg(Color::Green));
        } else if st.real {
            let agent = app
                .trade
                .as_ref()
                .map(|t| {
                    let a = &t.agent_addr;
                    if a.len() > 12 {
                        format!("{}…{}", &a[..8], &a[a.len() - 4..])
                    } else {
                        a.clone()
                    }
                })
                .unwrap_or_default();
            // corto a propósito: la columna puede ser estrecha
            b.push(
                tr.ex_real_agent_signs.replacen("{}", &agent, 1),
                Style::new().fg(Color::Red),
            );
        } else {
            b.push(tr.ex_mock_nothing_sent, dim());
        }
        b.render(f);
    }
}

/// Título de tabla con recuento — REALES en modo real, maqueta si no
/// (con cuántas filas son la siembra demo).
fn panel_title(real: bool, name: &str, demo: usize, total: usize) -> String {
    let tr = crate::i18n::t();
    if real {
        return match total {
            0 => tr.ex_t_real_none.replacen("{}", name, 1),
            t => tr
                .ex_t_real_n
                .replacen("{}", name, 1)
                .replacen("{}", &t.to_string(), 1),
        };
    }
    match (total, demo) {
        (0, _) => tr.ex_t_mock_none.replacen("{}", name, 1),
        (t, 0) => tr
            .ex_t_mock_n
            .replacen("{}", name, 1)
            .replacen("{}", &t.to_string(), 1),
        (t, d) => tr
            .ex_t_mock_n_demo
            .replacen("{}", name, 1)
            .replacen("{}", &t.to_string(), 1)
            .replacen("{}", &d.to_string(), 1),
    }
}

pub(crate) fn centered(w: u16, h: u16, r: Rect) -> Rect {
    let w = w.min(r.width);
    let h = h.min(r.height);
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}

/// Botonera común de los modales: y confirma · n/Esc cancela, clicable.
/// La comparten los modales de este panel y el del depósito real (fondos.rs).
pub(crate) fn modal_buttons(
    f: &mut Frame,
    inner: Rect,
    row: u16,
    yes: &str,
    hits: &mut Vec<(Rect, Hit)>,
) {
    let mut b = LineB::new(inner, row);
    b.push("  ", Style::new());
    let hr = b.push(
        format!(" y {yes} "),
        Style::new()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD),
    );
    hits.push((hr.intersection(inner), Hit::ConfirmYes));
    b.push("   ", Style::new());
    let hr = b.push(
        crate::i18n::t().ex_btn_cancel,
        Style::new()
            .fg(Color::Black)
            .bg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    );
    hits.push((hr.intersection(inner), Hit::ConfirmNo));
    b.render(f);
}

/// Modal de confirmación: resumen COMPLETO de la acción antes de "enviarla".
fn draw_confirm(f: &mut Frame, app: &App, c: &Confirm, hits: &mut Vec<(Rect, Hit)>) {
    let kv = |k: &str, v: String, st: Style| {
        Line::from(vec![
            Span::styled(format!("  {k:<13}"), dim()),
            Span::styled(v, st),
        ])
    };
    let tr = crate::i18n::t();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let real = app.exec.real;
    let net = app.net_label;
    // la nota final del resumen: en real deja claro que ESTO va al exchange
    let nota = |accion: &str| {
        if real {
            Line::from(Span::styled(
                tr.ex_note_real
                    .replacen("{}", net, 1)
                    .replacen("{}", accion, 1),
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ))
        } else {
            Line::from(Span::styled(
                tr.ex_note_mock,
                Style::new().fg(Color::Yellow),
            ))
        }
    };
    let (title, lines, yes) = match c {
        Confirm::Order(d) => {
            let sz_dec = app
                .pairs
                .get(&d.coin)
                .map(|p| p.meta.sz_decimals)
                .unwrap_or(4);
            let side_st = Style::new()
                .fg(if d.side.is_long() {
                    Color::Green
                } else {
                    Color::Red
                })
                .add_modifier(Modifier::BOLD);
            let trig = |v: Option<f64>| match v {
                Some(px) => format!("{} ({:+.1}%)", fmt_px(px), (px / d.entry - 1.0) * 100.0),
                None => "—".into(),
            };
            let lines = vec![
                kv(
                    tr.ex_pair,
                    format!(
                        "{} · {} {}× · {}",
                        d.coin,
                        d.side.label(),
                        d.lev,
                        d.typ.label()
                    ),
                    side_st,
                ),
                kv(
                    tr.ex_kv_size,
                    format!(
                        "{:.*} {}  ≈ ${:.2}",
                        sz_dec as usize, d.sz_asset, d.coin, d.sz_usd
                    ),
                    bold,
                ),
                kv(tr.ex_kv_entry_est, fmt_px(d.entry), bold),
                kv(
                    tr.ex_kv_liq_est,
                    match d.liq {
                        Some(l) => format!("{} ({:+.1}%)", fmt_px(l), (l / d.entry - 1.0) * 100.0),
                        None => tr.ex_no_liq_1x.into(),
                    },
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                kv(tr.ex_sl, trig(d.sl), Style::new()),
                kv(tr.ex_tp, trig(d.tp), Style::new()),
                kv(
                    tr.ex_kv_margin_req,
                    format!("${:.2}", d.sz_usd / d.lev.max(1) as f64),
                    Style::new(),
                ),
                Line::raw(""),
                nota(tr.ex_the_order),
            ];
            (
                if real {
                    tr.ex_confirm_order_real.replacen("{}", net, 1)
                } else {
                    tr.ex_confirm_order_mock.to_string()
                },
                lines,
                tr.ex_btn_confirm,
            )
        }
        Confirm::Close(i) => {
            let Some(p) = app.exec.positions.get(*i) else {
                return;
            };
            let long = p.is_long();
            let mark = app.pairs.get(&p.coin).map(|x| x.mid).unwrap_or(0.0);
            let pnl_txt = match exec::pos_pnl(p, mark) {
                Some((pnl, roe)) => format!("{pnl:+.2} $ ({roe:+.1}%)"),
                None => "—".into(),
            };
            let lines = vec![
                kv(
                    tr.ex_kv_close,
                    tr.ex_at_market
                        .replacen("{}", &p.coin, 1)
                        .replacen("{}", if long { "LONG" } else { "SHORT" }, 1)
                        .replacen("{}", &format!("{:+.4}", p.szi), 1),
                    Style::new()
                        .fg(if long { Color::Green } else { Color::Red })
                        .add_modifier(Modifier::BOLD),
                ),
                kv(
                    tr.ex_kv_entry_mark,
                    format!("{} → {}", fmt_px(p.entry), fmt_px(mark)),
                    Style::new(),
                ),
                kv(
                    tr.ex_kv_pnl_est,
                    pnl_txt,
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Line::raw(""),
                nota(tr.ex_the_market_close),
            ];
            (
                if real {
                    tr.ex_close_pos_real.replacen("{}", net, 1)
                } else {
                    tr.ex_close_pos_mock.to_string()
                },
                lines,
                tr.ex_btn_close,
            )
        }
    };
    let mut lines = lines;
    // fricción reforzada de mainnet (paso 7.5): la botonera y `y` no bastan,
    // hay que teclear la frase completa y Enter — dinero real en juego
    if let Some(typed) = &app.exec.confirm_phrase {
        let ok = typed == exec::MAINNET_PHRASE;
        lines.push(Line::from(vec![
            Span::styled(
                tr.ex_type_phrase.replacen("{}", exec::MAINNET_PHRASE, 1),
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{typed}▏"),
                Style::new()
                    .fg(if ok { Color::Green } else { Color::White })
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    let h = lines.len() as u16 + 4;
    let area = centered(58, h, f.area());
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let n = lines.len() as u16;
    f.render_widget(
        Paragraph::new(lines),
        Rect::new(inner.x, inner.y, inner.width, n.min(inner.height)),
    );
    if inner.height > n + 1 {
        modal_buttons(f, inner, n + 1, yes, hits);
    }
}

/// Modal de edición de SL/TP de una posición abierta.
fn draw_sltp(f: &mut Frame, app: &App, m: &SlTpEdit, hits: &mut Vec<(Rect, Hit)>) {
    let Some(p) = app.exec.positions.get(m.pos) else {
        return;
    };
    let tr = crate::i18n::t();
    let long = p.is_long();
    let area = centered(54, 9, f.area());
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(
            tr.ex_sltp_title
                .replacen("{}", &p.coin, 1)
                .replacen("{}", if long { "LONG" } else { "SHORT" }, 1)
                .replacen("{}", &fmt_px(p.entry), 1),
        )
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    for (row, (fo, val, name, on, is_sl)) in [
        (Focus::Sl, &m.sl, "SL", !m.on_tp, true),
        (Focus::Tp, &m.tp, "TP", m.on_tp, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut b = LineB::new(inner, row as u16);
        let lr = label(&mut b, name, on);
        hits.push((lr.intersection(inner), Hit::Edit(fo)));
        let hr = input(&mut b, val, on, on, tr.ex_ph_trigger_clear);
        hits.push((hr.intersection(inner), Hit::Edit(fo)));
        match exec::parse_trigger(val, p.entry, long, is_sl) {
            Ok(Some(px)) => {
                let pct = (px / p.entry - 1.0) * 100.0;
                match exec::trigger_side_err(px, p.entry, long, is_sl) {
                    None => {
                        b.push(format!("→ {} ({pct:+.1}%)", fmt_px(px)), dim());
                    }
                    Some(e) => {
                        b.push(format!("✗ {e}"), Style::new().fg(Color::Red));
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                b.push(format!("✗ {e}"), Style::new().fg(Color::Red));
            }
        }
        b.render(f);
    }
    let hint = match &m.err {
        Some(e) => Span::styled(format!("  ✗ {e}"), Style::new().fg(Color::Red)),
        None => Span::styled(tr.ex_sltp_hint, dim()),
    };
    if inner.height > 3 {
        f.render_widget(
            Paragraph::new(hint),
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
        );
    }
    if inner.height > 5 {
        modal_buttons(f, inner, 5, tr.ex_btn_apply, hits);
    }
}
