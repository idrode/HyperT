use std::time::Instant;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState};

use crate::app::{App, WalletFocus};
use crate::data::types::{AccountSnapshot, FillInfo, TransferInfo};

use super::fmt::{datetime_label, fmt_px, fmt_usd, sign_color, time_label};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let tr = crate::i18n::t();
    let Some(addr) = app.wallet_addr.clone() else {
        app.wallet_rows_area = None;
        f.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::from(Span::styled(
                    tr.wa_no_address,
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::raw(tr.wa_press_e),
                Line::raw(tr.wa_read_only),
                Line::raw(tr.wa_no_keys),
            ])
            .block(Block::bordered().title(tr.wa_title)),
            area,
        );
        return;
    };

    // Layout (de arriba a abajo): resumen (win rate + PnL realizado) · cabecera
    // de cuenta · posiciones abiertas navegables · operaciones cerradas.
    let rows = Layout::vertical([
        Constraint::Length(4), // resumen histórico
        Constraint::Length(4), // cabecera de cuenta
        Constraint::Min(4),    // posiciones abiertas
        Constraint::Length(8), // wallets relacionadas (recibidos | enviados)
        Constraint::Length(8), // operaciones cerradas
    ])
    .split(area);

    draw_summary(f, app, rows[0]);

    let snap = app.wallet.clone();
    let sel = app.wallet_sel;
    // atajos propios de la vista + profundidad de la pila de pivoteo entre
    // wallets relacionadas (solo se anuncia el "atrás" si hay a dónde volver).
    let mut hint = format!("{} · {}", tr.wa_change_addr_hint, tr.wa_rel_hint);
    if !app.wallet_back.is_empty() {
        hint.push_str(
            &tr.wa_rel_back_hint
                .replacen("{}", &app.wallet_back.len().to_string(), 1),
        );
    }
    // el estado sale del App mientras se dibuja (el cierre de mark_of toma
    // prestado app en inmutable) y vuelve justo después con su offset ya
    // ajustado por el widget.
    let mut tbl_state = std::mem::take(&mut app.wallet_state);
    let mark_of = |c: &str| app.pairs.get(c).map(|x| x.mid).unwrap_or(0.0);
    let rows_area = draw_account(
        f,
        &mark_of,
        AccountView {
            addr: &addr,
            snap: snap.as_ref(),
            at: app.wallet_at,
            title: tr.wa_title,
            hint: &hint,
            table_label: tr.wa_positions,
            note: None,
            sel: Some(sel),
        },
        Some(&mut tbl_state),
        rows[1],
        rows[2],
    );
    app.wallet_state = tbl_state;
    app.wallet_rows_area = rows_area;

    draw_related(f, app, rows[3]);
    draw_closed(f, app, rows[4]);

    if app.wallet_pos_modal.is_some() {
        app.overlay_drawn.set(true);
        draw_pos_modal(f, app, area);
    } else if let Some((addr, feedback)) = app.wallet_addr_modal.clone() {
        app.overlay_drawn.set(true);
        super::whales::draw_addr_overlay(
            f,
            area,
            &addr,
            &feedback,
            tr.wa_rel_modal_title,
            tr.wa_rel_modal_hint,
        );
    }
}

/// Wallets relacionadas: dos listas lado a lado (fondos recibidos de / fondos
/// enviados a) derivadas de `userNonFundingLedgerUpdates`. Enter sobre una
/// fila abre su dirección completa, desde donde se pivota la observación.
/// Solo aparecen movimientos CON contraparte: los depósitos/retiros del
/// bridge no relacionan a la cuenta con otra wallet de Hyperliquid.
fn draw_related(f: &mut Frame, app: &mut App, area: Rect) {
    let tr = crate::i18n::t();
    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let loading = app.wallet_transfers_at.is_none();

    let inc: Vec<TransferInfo> = app.wallet_in().into_iter().cloned().collect();
    let out: Vec<TransferInfo> = app.wallet_out().into_iter().cloned().collect();

    let (in_area, in_top) = draw_transfer_list(
        f,
        cols[0],
        tr.wa_rel_in_title,
        &inc,
        app.wallet_in_sel,
        app.wallet_focus == WalletFocus::In,
        loading,
        Color::Green,
    );
    let (out_area, out_top) = draw_transfer_list(
        f,
        cols[1],
        tr.wa_rel_out_title,
        &out,
        app.wallet_out_sel,
        app.wallet_focus == WalletFocus::Out,
        loading,
        Color::Red,
    );
    app.wallet_in_area = in_area;
    app.wallet_in_top = in_top;
    app.wallet_out_area = out_area;
    app.wallet_out_top = out_top;
}

/// Una de las dos listas de wallets relacionadas. Devuelve (rect de las filas
/// de datos, índice de la primera fila pintada) para mapear clicks con el
/// mismo desplazamiento con el que se dibujó.
#[allow(clippy::too_many_arguments)]
fn draw_transfer_list(
    f: &mut Frame,
    area: Rect,
    label: &str,
    items: &[TransferInfo],
    sel: usize,
    focused: bool,
    loading: bool,
    accent: Color,
) -> (Option<Rect>, usize) {
    let tr = crate::i18n::t();
    let header = Row::new(vec![tr.wa_col_date, "Wallet", tr.wa_col_size, tr.wa_rel_col_kind])
        .style(Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD));

    // ventana visible: mantiene la fila seleccionada a la vista sin TableState
    // (el mapeo de clicks necesita saber el desplazamiento exacto).
    let visible = area.height.saturating_sub(3) as usize;
    let top = if visible == 0 || sel < visible {
        0
    } else {
        sel + 1 - visible
    };

    let rows: Vec<Row> = items
        .iter()
        .enumerate()
        .skip(top)
        .take(visible.max(1))
        .map(|(i, t)| {
            // USDC se muestra en dólares directamente; cualquier otro token, en
            // sus propias unidades más el valor en USD que reporta la API (si
            // lo reporta — nunca se inventa una conversión).
            let amount = match (t.token.as_str(), t.usd) {
                ("USDC", _) => format!("${:.2}", t.amount),
                (tok, Some(usd)) => format!("{} {tok} ≈${usd:.2}", trim_num(t.amount)),
                (tok, None) => format!("{} {tok}", trim_num(t.amount)),
            };
            let row = Row::new(vec![
                Cell::from(time_label(t.time_ms)),
                Cell::from(short_addr(&t.counterparty))
                    .style(Style::new().fg(Color::Cyan)),
                Cell::from(amount).style(Style::new().fg(accent)),
                Cell::from(t.kind.clone()).style(Style::new().fg(Color::DarkGray)),
            ]);
            if focused && i == sel {
                row.style(
                    Style::new()
                        .bg(Color::Rgb(40, 44, 66))
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                row
            }
        })
        .collect();

    let title = if loading {
        format!(" {label} — {} ", tr.wa_rel_loading)
    } else if items.is_empty() {
        format!(" {label} — {} ", tr.wa_rel_none)
    } else {
        format!(" {label} ({}) ", items.len())
    };
    let border = if focused { Color::Cyan } else { Color::Reset };
    let widths = [
        Constraint::Length(14),
        Constraint::Length(15),
        Constraint::Length(26),
        Constraint::Min(8),
    ];
    f.render_widget(
        Table::new(rows, widths).header(header).block(
            Block::bordered()
                .title(title)
                .border_style(Style::new().fg(border)),
        ),
        area,
    );

    if items.is_empty() || visible == 0 {
        return (None, 0);
    }
    let inner = area.inner(Margin::new(1, 1));
    let rect = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1),
    );
    (Some(rect), top)
}

/// Cantidad de un token con hasta 4 decimales, sin ceros de relleno (los
/// tokens de este ledger van de 0.001 a decenas de millones).
fn trim_num(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn short_addr(a: &str) -> String {
    if a.len() > 14 {
        format!("{}…{}", &a[..7], &a[a.len() - 4..])
    } else {
        a.to_string()
    }
}

/// Bloque de resumen histórico de la dirección: win rate (con etiqueta
/// ganadora/perdedora) y PnL realizado acumulado, derivados de `userFills`.
fn draw_summary(f: &mut Frame, app: &App, area: Rect) {
    let tr = crate::i18n::t();
    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
    let s = summarize_fills(&app.wallet_fills);
    let lines = if app.wallet_fills_at.is_none() {
        vec![
            Line::from(dim(tr.wa_hist_summary.into())),
            Line::from(dim(tr.wa_hist_loading.into())),
        ]
    } else if app.wallet_fills.is_empty() {
        vec![
            Line::from(dim(tr.wa_hist_summary.into())),
            Line::from(dim(tr.wa_hist_empty.into())),
        ]
    } else {
        let (wr_span, label) = match s.win_rate() {
            Some(wr) => {
                let (txt, col) = if wr > 50.0 {
                    (tr.wa_ganadora, Color::Green)
                } else if wr < 50.0 {
                    (tr.wa_perdedora, Color::Red)
                } else {
                    (tr.wa_neutra, Color::Gray)
                };
                (
                    Span::styled(format!("{wr:.1}%"), Style::new().fg(col).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" {txt}"), Style::new().fg(col).add_modifier(Modifier::BOLD)),
                )
            }
            None => (dim("—".into()), Span::raw("")),
        };
        vec![
            Line::from(vec![
                dim(tr.wa_win_rate.into()),
                wr_span,
                label,
                dim(format!("  ({}✓ / {}✗ {}", s.wins, s.losses, tr.wa_closed_ops_count)),
            ]),
            Line::from(vec![
                dim(tr.wa_realized_pnl.into()),
                Span::styled(
                    fmt_signed_usd(s.realized_pnl),
                    Style::new()
                        .fg(sign_color(Some(s.realized_pnl), false))
                        .add_modifier(Modifier::BOLD),
                ),
                dim(format!(
                    "   {} ${:.2}   ·  {} fills",
                    tr.wa_fees_fills,
                    s.total_fees,
                    app.wallet_fills.len()
                )),
            ]),
        ]
    };
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(tr.wa_hist_perf_title)),
        area,
    );
}

/// Sección SEPARADA de operaciones cerradas: fills con PnL realizado ≠ 0, más
/// reciente primero, con fecha y retorno. Nota: `userFills` no trae el
/// apalancamiento de cada operación, así que el % mostrado es retorno sobre el
/// NOTIONAL del fill de cierre (proxy honesto de ROE — el ROE sobre margen
/// exacto requeriría el leverage histórico, que la API no expone aquí).
fn draw_closed(f: &mut Frame, app: &App, area: Rect) {
    let tr = crate::i18n::t();
    let header = Row::new(vec![
        tr.wa_col_date,
        tr.rk_col_pair,
        tr.wa_col_direction,
        tr.wa_col_size,
        tr.wa_col_pnl,
        "ret%·ntl",
    ])
    .style(Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD));

    let closed: Vec<&FillInfo> = app
        .wallet_fills
        .iter()
        .filter(|f| f.closed_pnl.abs() > 1e-9)
        .collect();

    let rows: Vec<Row> = closed
        .iter()
        .take(200)
        .map(|f| {
            let ntl = (f.px * f.sz).abs();
            let ret = if ntl > 0.0 {
                Some(f.closed_pnl / ntl * 100.0)
            } else {
                None
            };
            Row::new(vec![
                Cell::from(time_label(f.time_ms)),
                Cell::from(f.coin.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(f.dir.clone()),
                Cell::from(format!("{:.4}", f.sz)),
                Cell::from(fmt_signed_usd(f.closed_pnl))
                    .style(Style::new().fg(sign_color(Some(f.closed_pnl), false))),
                Cell::from(match ret {
                    Some(r) => format!("{r:+.2}"),
                    None => "—".into(),
                })
                .style(Style::new().fg(sign_color(ret, false))),
            ])
        })
        .collect();

    let title = if app.wallet_fills_at.is_none() {
        tr.wa_closed_loading.to_string()
    } else if closed.is_empty() {
        tr.wa_closed_none.to_string()
    } else {
        format!(" {} ({} · ret%/notional) ", tr.wa_closed_title, closed.len())
    };
    let widths = [
        Constraint::Length(14),
        Constraint::Length(8),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(11),
        Constraint::Length(9),
    ];
    f.render_widget(
        Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(title)),
        area,
    );
}

/// Modal de detalle de una posición abierta: fecha de apertura (heurística
/// cruzando `userFills`) y funding acumulado desde la apertura.
fn draw_pos_modal(f: &mut Frame, app: &App, area: Rect) {
    let Some(coin) = &app.wallet_pos_modal else {
        return;
    };
    let pos = app
        .wallet
        .as_ref()
        .and_then(|w| w.positions.iter().find(|p| &p.coin == coin));
    let Some(p) = pos else {
        return;
    };

    let tr = crate::i18n::t();
    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
    let side = if p.szi >= 0.0 { "LONG" } else { "SHORT" };
    let side_col = if p.szi >= 0.0 { Color::Green } else { Color::Red };

    let open_line = match position_open_time(&app.wallet_fills, coin, p.szi) {
        Some((t, exact)) => {
            let prefix = if exact { "" } else { "≥ " };
            Line::from(vec![
                dim(tr.wa_opened.into()),
                Span::styled(
                    format!("{prefix}{}", datetime_label(t)),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                dim(format!("   ({})", super::fmt::age_label(t))),
            ])
        }
        None => Line::from(dim(tr.wa_open_unknown.into())),
    };
    // Convención de cumFunding.sinceOpen VERIFICADA empíricamente (2026-07-23,
    // cuenta de una sola posición con historia completa de userFunding): el
    // valor coincide al céntimo con −Σ(userFunding.usdc), y usdc>0 = la cuenta
    // recibe. Por tanto sinceOpen POSITIVO = el trader PAGÓ funding (coste),
    // NEGATIVO = lo COBRÓ. Las docs/SDK oficiales no lo documentan.
    let funding = p.since_open_funding;
    let (f_txt, f_col) = if funding > 0.0 {
        (format!("{} ${funding:.4}", tr.wa_paid), Color::Red)
    } else if funding < 0.0 {
        (format!("{} ${:.4}", tr.wa_received, -funding), Color::Green)
    } else {
        ("$0.0000".to_string(), Color::Gray)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(coin.clone(), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(side, Style::new().fg(side_col).add_modifier(Modifier::BOLD)),
            dim(format!("  {:+.4}  @ {}", p.szi, p.entry_px.map(fmt_px).unwrap_or_else(|| "—".into()))),
        ]),
        Line::raw(""),
        open_line,
        Line::from(vec![
            dim(tr.wa_funding_since_open.into()),
            Span::styled(f_txt, Style::new().fg(f_col).add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
        Line::from(dim(tr.wa_pos_modal_hint.into())),
    ];
    let w = 62u16.min(area.width);
    let h = 9u16.min(area.height);
    let r = Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(tr.wa_pos_modal_title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        r,
    );
}

fn fmt_signed_usd(v: f64) -> String {
    let sign = if v >= 0.0 { "+" } else { "-" };
    format!("{sign}${}", fmt_usd(v.abs()))
}

/// Resumen de rendimiento derivado del historial de fills.
pub struct FillsSummary {
    pub realized_pnl: f64,
    pub wins: usize,
    pub losses: usize,
    pub total_fees: f64,
}

impl FillsSummary {
    /// % de operaciones cerradas ganadoras; None si no hay ninguna cerrada.
    pub fn win_rate(&self) -> Option<f64> {
        let n = self.wins + self.losses;
        (n > 0).then(|| self.wins as f64 / n as f64 * 100.0)
    }
}

/// Agrega los fills: PnL realizado total, comisiones, y conteo de operaciones
/// cerradas ganadoras/perdedoras (fills con `closedPnl` ≠ 0).
pub fn summarize_fills(fills: &[FillInfo]) -> FillsSummary {
    let mut s = FillsSummary {
        realized_pnl: 0.0,
        wins: 0,
        losses: 0,
        total_fees: 0.0,
    };
    for f in fills {
        s.realized_pnl += f.closed_pnl;
        s.total_fees += f.fee;
        if f.closed_pnl > 1e-9 {
            s.wins += 1;
        } else if f.closed_pnl < -1e-9 {
            s.losses += 1;
        }
    }
    s
}

/// Heurística: timestamp (ms) en que se abrió el TRAMO actual de la posición de
/// `coin`, y si es exacto (`true`) o solo un límite inferior (`false`, la
/// posición es más antigua que la ventana de fills disponible).
///
/// `fills` viene más reciente primero. El tramo actual empezó en el fill más
/// reciente cuyo `startPosition` estaba plano (≈0) o en el lado opuesto al
/// actual — todo lo posterior mantuvo el lado. Si no se encuentra tal fill, la
/// posición se abrió antes de lo que abarca el historial: se devuelve el fill
/// más antiguo de ese par como cota inferior. Una posición puede haberse
/// ampliado/reducido con varios fills; esto marca el inicio del tramo, no un
/// único trade de apertura (documentado por ser aproximación, no dato exacto).
pub fn position_open_time(fills: &[FillInfo], coin: &str, cur_szi: f64) -> Option<(u64, bool)> {
    let coin_fills: Vec<&FillInfo> = fills.iter().filter(|f| f.coin == coin).collect();
    let cur_long = cur_szi > 0.0;
    for f in &coin_fills {
        let flat_before = f.start_position.abs() < 1e-9;
        let opp_before = !flat_before && (f.start_position > 0.0) != cur_long;
        if flat_before || opp_before {
            return Some((f.time_ms, true));
        }
    }
    coin_fills.last().map(|f| (f.time_ms, false))
}

/// Contenido del panel de cuenta compartido entre la Vista 9 (watch-only) y
/// la Vista 8 (cuenta maestra WalletConnect): misma fuente clearinghouseState,
/// distinta dirección y rotulación.
pub(super) struct AccountView<'a> {
    pub addr: &'a str,
    pub snap: Option<&'a AccountSnapshot>,
    /// Momento del último snapshot, para la edad del refresco.
    pub at: Option<Instant>,
    /// Título del bloque de cabecera.
    pub title: &'a str,
    /// Nota/atajos propios de la vista, a continuación de la edad del refresco.
    pub hint: &'a str,
    /// Sustantivo de la tabla ("posiciones" / "posiciones reales").
    pub table_label: &'a str,
    /// Si Some, SUSTITUYE la línea de valor/retirable/margen: para cuentas
    /// unificadas, donde el clearinghouseState no es significativo y pintar
    /// "Valor cuenta $0" sería el dato confuso que ya costó un susto.
    pub note: Option<&'a str>,
    /// Si Some(i), resalta la fila i de la tabla de posiciones (Vista 9,
    /// navegable). None = sin selección (Vista 8, tabla no interactiva).
    pub sel: Option<usize>,
}

/// Cabecera de cuenta (valor/retirable/margen) + tabla de posiciones con mark
/// en vivo del propio TUI y distancia a liquidación.
/// Devuelve el Rect de la zona de datos de la tabla (bajo el borde + cabecera)
/// para que la Vista 9 pueda mapear clicks de ratón a filas; None si no hay
/// posiciones que clicar.
pub(super) fn draw_account(
    f: &mut Frame,
    mark_of: &dyn Fn(&str) -> f64,
    v: AccountView,
    // Estado de la tabla de posiciones (Vista 9): lo lleva el widget, que es
    // quien mantiene la ventana visible pegada a la fila seleccionada. None en
    // la Vista 8, donde la tabla no es navegable.
    state: Option<&mut TableState>,
    hdr_area: Rect,
    tbl_area: Rect,
) -> Option<Rect> {
    let tr = crate::i18n::t();
    let dim = |s: String| Span::styled(s, Style::new().fg(Color::DarkGray));
    let header_lines = match v.snap {
        Some(w) => {
            let implied_lev = if w.account_value > 0.0 {
                w.total_ntl_pos / w.account_value
            } else {
                0.0
            };
            let margin_pct = if w.account_value > 0.0 {
                w.total_margin_used / w.account_value * 100.0
            } else {
                0.0
            };
            let age = v
                .at
                .map(|t| format!("{}s", t.elapsed().as_secs()))
                .unwrap_or_else(|| "—".into());
            let balances = match v.note {
                Some(n) => Line::from(Span::styled(
                    n.to_string(),
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
                None => Line::from(vec![
                    dim(tr.wa_acct_value.into()),
                    Span::styled(
                        fmt_usd(w.account_value),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    dim(tr.wa_withdrawable.into()),
                    Span::raw(fmt_usd(w.withdrawable)),
                    dim(tr.wa_margin_used.into()),
                    Span::raw(format!(
                        "{} ({margin_pct:.1}%)",
                        fmt_usd(w.total_margin_used)
                    )),
                    dim(tr.wa_ntl_pos.into()),
                    Span::raw(fmt_usd(w.total_ntl_pos)),
                    dim(tr.wa_implied_lev.into()),
                    Span::raw(format!("{implied_lev:.1}×")),
                ]),
            };
            vec![
                Line::from(vec![
                    Span::styled(v.addr.to_string(), Style::new().fg(Color::Cyan)),
                    dim(format!("{}{age}{}", tr.wa_refresh_ago, v.hint)),
                ]),
                balances,
            ]
        }
        None => vec![
            Line::from(Span::styled(
                v.addr.to_string(),
                Style::new().fg(Color::Cyan),
            )),
            Line::from(dim(tr.wa_querying_chs.into())),
        ],
    };
    f.render_widget(
        Paragraph::new(header_lines).block(Block::bordered().title(v.title.to_string())),
        hdr_area,
    );

    // tabla de posiciones con mark en vivo del propio TUI y distancia a liquidación
    let header = Row::new(vec![
        tr.rk_col_pair,
        tr.wh_col_side,
        tr.wa_col_size,
        "Ntl $",
        tr.wa_col_entry,
        "Mark",
        "Liq",
        "dist%",
        "Lev",
        "uPnL $",
        "ROE%",
    ])
    .style(Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD));

    let empty = Vec::new();
    let positions = v.snap.map(|w| &w.positions).unwrap_or(&empty);
    let rows: Vec<Row> = positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let long = p.szi >= 0.0;
            let side = if long { "LONG" } else { "SHORT" };
            let side_color = if long { Color::Green } else { Color::Red };
            let mark = mark_of(&p.coin);
            let (liq_txt, dist_txt, dist_color) = match p.liq_px {
                Some(liq) if mark > 0.0 => {
                    let d = (liq / mark - 1.0) * 100.0;
                    let c = if d.abs() < 5.0 {
                        Color::Red
                    } else if d.abs() < 15.0 {
                        Color::Yellow
                    } else {
                        Color::Gray
                    };
                    (fmt_px(liq), format!("{d:+.1}"), c)
                }
                Some(liq) => (fmt_px(liq), "—".into(), Color::Gray),
                None => ("—".into(), "—".into(), Color::DarkGray),
            };
            let row = Row::new(vec![
                Cell::from(p.coin.clone()).style(Style::new().add_modifier(Modifier::BOLD)),
                Cell::from(side).style(Style::new().fg(side_color).add_modifier(Modifier::BOLD)),
                Cell::from(format!("{:+.4}", p.szi)),
                Cell::from(fmt_usd(p.position_value)),
                Cell::from(p.entry_px.map(fmt_px).unwrap_or_else(|| "—".into())),
                Cell::from(fmt_px(mark)),
                Cell::from(liq_txt).style(Style::new().fg(Color::Yellow)),
                Cell::from(dist_txt).style(Style::new().fg(dist_color)),
                Cell::from(format!(
                    "{}×{}",
                    p.leverage,
                    if p.is_cross { "c" } else { "i" }
                )),
                Cell::from(fmt_usd(p.unrealized_pnl))
                    .style(Style::new().fg(sign_color(Some(p.unrealized_pnl), false))),
                Cell::from(format!("{:+.1}", p.roe * 100.0))
                    .style(Style::new().fg(sign_color(Some(p.roe), false))),
            ]);
            // resalte de la fila seleccionada (solo Vista 9, v.sel = Some)
            if v.sel == Some(i) {
                row.style(Style::new().bg(Color::Rgb(40, 44, 66)).add_modifier(Modifier::BOLD))
            } else {
                row
            }
        })
        .collect();

    let title = if positions.is_empty() && v.snap.is_some() {
        tr.wa_no_positions.replacen("{}", v.table_label, 1)
    } else {
        format!(" {} {} ", positions.len(), v.table_label)
    };
    let widths = [
        Constraint::Length(7),
        Constraint::Length(6),
        Constraint::Length(12),
        Constraint::Length(9),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(11),
        Constraint::Length(7),
        Constraint::Length(5),
        Constraint::Length(9),
        Constraint::Length(7),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::bordered().title(title));
    match state {
        // Con estado, el propio widget desplaza la ventana visible para que la
        // fila seleccionada siempre quede a la vista (mismo mecanismo que
        // Ranking y Flujo), y su `offset()` es lo que usa el mapeo de clicks.
        Some(st) => {
            st.select(Some(v.sel.unwrap_or(0).min(positions.len().saturating_sub(1))));
            f.render_stateful_widget(table, tbl_area, st);
        }
        None => f.render_widget(table, tbl_area),
    }

    // Zona de datos (dentro del borde + fila de cabecera) para mapear clicks.
    if v.sel.is_some() && !positions.is_empty() {
        let inner = tbl_area.inner(Margin::new(1, 1));
        Some(Rect::new(
            inner.x,
            inner.y.saturating_add(1),
            inner.width,
            inner.height.saturating_sub(1),
        ))
    } else {
        None
    }
}

/// Overlay de entrada de dirección (se dibuja encima de cualquier vista).
pub fn draw_input(f: &mut Frame, app: &App) {
    let w = 52u16.min(f.area().width);
    let h = 5u16.min(f.area().height);
    let r = f.area();
    let area = Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h);
    f.render_widget(Clear, area);
    let input_span = if app.input_buf.is_empty() {
        Span::styled("0x…", Style::new().fg(Color::DarkGray))
    } else {
        Span::styled(
            app.input_buf.as_str(),
            Style::new().add_modifier(Modifier::BOLD),
        )
    };
    let mut lines = vec![Line::from(vec![
        Span::raw(" "),
        input_span,
        Span::styled("▏", Style::new().fg(Color::Cyan)),
    ])];
    match &app.input_err {
        Some(e) => lines.push(Line::from(Span::styled(
            format!(" {e}"),
            Style::new().fg(Color::Red),
        ))),
        None => lines.push(Line::from(Span::styled(
            crate::i18n::t().wa_input_confirm,
            Style::new().fg(Color::DarkGray),
        ))),
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .title(crate::i18n::t().wa_input_title)
                .border_style(Style::new().fg(Color::Cyan)),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(coin: &str, dir: &str, px: f64, sz: f64, start: f64, pnl: f64, t: u64) -> FillInfo {
        FillInfo {
            coin: coin.into(),
            dir: dir.into(),
            px,
            sz,
            start_position: start,
            closed_pnl: pnl,
            fee: 0.1,
            time_ms: t,
        }
    }

    #[test]
    fn resumen_win_rate_y_pnl() {
        // 3 cierres: +10, -4, +6 (2 ganadoras, 1 perdedora) + un fill de apertura (pnl 0)
        let fills = vec![
            fill("BTC", "Close Long", 100.0, 1.0, 1.0, 10.0, 500),
            fill("BTC", "Close Long", 100.0, 1.0, 1.0, -4.0, 400),
            fill("ETH", "Close Short", 50.0, 2.0, -2.0, 6.0, 300),
            fill("ETH", "Open Short", 50.0, 2.0, 0.0, 0.0, 200),
        ];
        let s = summarize_fills(&fills);
        assert_eq!(s.wins, 2);
        assert_eq!(s.losses, 1);
        assert!((s.realized_pnl - 12.0).abs() < 1e-9);
        assert!((s.win_rate().unwrap() - 66.6666).abs() < 1e-3);
    }

    #[test]
    fn win_rate_sin_cierres_es_none() {
        let fills = vec![fill("BTC", "Open Long", 100.0, 1.0, 0.0, 0.0, 100)];
        assert!(summarize_fills(&fills).win_rate().is_none());
    }

    #[test]
    fn apertura_exacta_desde_plano() {
        // más reciente primero: se amplió (start 1.0) tras abrir desde plano (start 0.0)
        let fills = vec![
            fill("BTC", "Buy", 100.0, 0.5, 1.0, 0.0, 900),
            fill("BTC", "Buy", 100.0, 1.0, 0.0, 0.0, 800),
        ];
        let (t, exact) = position_open_time(&fills, "BTC", 1.5).unwrap();
        assert_eq!(t, 800);
        assert!(exact);
    }

    #[test]
    fn apertura_es_cota_inferior_si_no_hay_cruce_por_cero() {
        // el tramo actual (long) nunca vuelve a plano dentro de la ventana
        let fills = vec![
            fill("BTC", "Buy", 100.0, 0.5, 2.0, 0.0, 900),
            fill("BTC", "Buy", 100.0, 0.5, 1.5, 0.0, 800),
        ];
        let (t, exact) = position_open_time(&fills, "BTC", 2.5).unwrap();
        assert_eq!(t, 800); // el más antiguo disponible
        assert!(!exact);
    }

    #[test]
    fn apertura_detecta_flip_de_lado() {
        // ahora long; el fill más reciente con start_position corto marca el flip
        let fills = vec![
            fill("BTC", "Long > Short", 100.0, 2.0, -1.0, 5.0, 900),
        ];
        let (t, exact) = position_open_time(&fills, "BTC", 1.0).unwrap();
        assert_eq!(t, 900);
        assert!(exact);
    }
}
