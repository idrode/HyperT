use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};

use crate::app::{App, DepositUi, TransferUi, WithdrawUi};
use crate::exec::Hit;
use crate::wallet::walletconnect::{
    fmt_usdc, AgentStatus, DepositStatus, TransferStatus, WcStatus, WithdrawStatus,
};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // el hitmap del panel de ejecución se reconstruye en cada frame
    app.exec.hits.clear();

    // el QR necesita toda la vista; el resto de estados caben en una tira
    if let WcStatus::WaitingScan {
        uri,
        qr,
        expires_at,
    } = &app.wc
    {
        let block = Block::bordered().title(crate::i18n::t().fu_qr_title);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let left = expires_at.saturating_duration_since(std::time::Instant::now());
        draw_qr(f, inner, uri, qr, left.as_secs());
        return;
    }

    // con sesión activa: USDC on-chain (wallet) + saldo/posiciones REALES en
    // Hyperliquid (clearinghouseState) entre la tira WC y la maqueta — dos
    // custodias distintas, cada una en su bloque, sin mezclar
    let session = match &app.wc {
        WcStatus::Connected(s) => Some((s.address.clone(), s.chain.clone())),
        _ => None,
    };
    let Some((addr, chain)) = session else {
        let rows = Layout::vertical([Constraint::Length(3), Constraint::Min(8)]).split(area);
        draw_wc_strip(f, &app.wc, rows[0]);
        super::exec::draw(f, app, rows[1]);
        return;
    };

    let n_pos = app.funds.as_ref().map(|w| w.positions.len()).unwrap_or(0);
    let tbl_h = (n_pos as u16 + 3).clamp(3, 8);
    // las tiras de depósito/retiro/agent/transferencia solo ocupan sitio
    // cuando hay algo que contar; el saldo SPOT es fijo, como el de perps
    let dep_h: u16 = if app.deposit.is_some() { 3 } else { 0 };
    let wd_h: u16 = if app.withdraw.is_some() { 3 } else { 0 };
    let ag_h: u16 = if app.agent.is_some() { 3 } else { 0 };
    let xf_h: u16 = if app.transfer.is_some() { 3 } else { 0 };
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(dep_h),
        Constraint::Length(wd_h),
        Constraint::Length(ag_h),
        Constraint::Length(xf_h),
        Constraint::Length(4),
        Constraint::Length(tbl_h),
        Constraint::Min(8),
    ])
    .split(area);
    draw_wc_strip(f, &app.wc, rows[0]);
    draw_usdc_strip(f, app, &chain, rows[1]);
    draw_spot_strip(f, app, rows[2]);
    if let Some(dep) = app.deposit.clone() {
        draw_deposit_strip(f, &dep, rows[3]);
    }
    if let Some(wd) = app.withdraw.clone() {
        draw_withdraw_strip(f, &wd, rows[4]);
    }
    if let Some(ag) = app.agent.clone() {
        draw_agent_strip(f, &ag, rows[5]);
    }
    if let Some(xf) = app.transfer.clone() {
        draw_transfer_strip(f, &xf, rows[6]);
    }
    // cuenta unificada: el clearinghouseState de perps NO es significativo —
    // ni pintar su "Valor cuenta $0" ni ofrecer la transferencia `t`
    let unified = app.is_unified();
    let tr = crate::i18n::t();
    // el rótulo decía "testnet" fijo aunque el panel real ya opera también
    // contra mainnet (paso 7.5): la red sale de la sesión, no de un literal
    let abajo = if app.exec.real {
        tr.fu_below_real.replacen("{}", app.net_label, 1)
    } else {
        tr.fu_below_mock.to_string()
    };
    let hint = if unified {
        tr.fu_hint_unified.replacen("{}", &abajo, 1)
    } else {
        tr.fu_hint_standard.replacen("{}", &abajo, 1)
    };
    super::wallet::draw_account(
        f,
        &|c| app.pairs.get(c).map(|x| x.mid).unwrap_or(0.0),
        super::wallet::AccountView {
            addr: &addr,
            snap: app.funds.as_ref(),
            at: app.funds_at,
            title: if unified {
                tr.fu_perps_unified_title
            } else {
                tr.fu_perps_title
            },
            hint: &hint,
            table_label: tr.fu_real_positions,
            note: unified.then_some(tr.fu_unified_note),
            sel: None,
            positions: None,
            fresh: &[],
            opened: &[],
        },
        None,
        rows[7],
        rows[8],
    );
    super::exec::draw(f, app, rows[9]);

    // los modales van encima de todo y se quedan los clicks
    if app.deposit_ui.is_some() {
        app.overlay_drawn.set(true);
        app.exec.hits.clear();
        draw_deposit_modal(f, app);
    }
    if app.withdraw_ui.is_some() {
        app.overlay_drawn.set(true);
        app.exec.hits.clear();
        draw_withdraw_modal(f, app);
    }
    if app.agent_ui.is_some() {
        app.overlay_drawn.set(true);
        app.exec.hits.clear();
        draw_agent_modal(f, app);
    }
    if app.transfer_ui.is_some() {
        app.overlay_drawn.set(true);
        app.exec.hits.clear();
        draw_transfer_modal(f, app);
    }
}

/// Saldo SPOT dentro de Hyperliquid — bloque propio, siempre visible con
/// sesión, separado del de perps de abajo: son dos saldos distintos (el
/// faucet de testnet y las compras spot acreditan aquí, no en perps) y
/// confundirlos ya costó un susto el 2026-07-20. En cuenta UNIFICADA este
/// bloque es LA fuente de verdad: el saldo es directamente el margen de
/// perps y la transferencia `t` no existe.
fn draw_spot_strip(f: &mut Frame, app: &App, area: Rect) {
    let tr = crate::i18n::t();
    let unified = app.is_unified();
    let block = Block::bordered().title(if unified {
        tr.fu_spot_unified_title
    } else {
        tr.fu_spot_title
    });
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = match &app.spot {
        None => Line::from(Span::styled(
            tr.fu_spot_loading,
            Style::new().fg(Color::Yellow),
        )),
        Some(s) => {
            let age = app
                .spot_at
                .map(|t| format!("{}s", t.elapsed().as_secs()))
                .unwrap_or_else(|| "—".into());
            let mut extras = String::new();
            if s.usdc_hold > 0.0 {
                extras.push_str(&tr.fu_held_in_orders.replacen(
                    "{}",
                    &format!("{:.2}", s.usdc_hold),
                    1,
                ));
            }
            if !s.others.is_empty() {
                extras.push_str(
                    &tr.fu_more_spot_tokens
                        .replacen("{}", &s.others.len().to_string(), 1),
                );
            }
            let detail = if unified {
                let avail = s
                    .usdc_avail
                    .unwrap_or_else(|| (s.usdc_total - s.usdc_hold).max(0.0));
                tr.fu_spot_detail_unified
                    .replacen("{}", &format!("{avail:.2}"), 1)
                    .replacen("{}", &extras, 1)
                    .replacen("{}", &age, 1)
            } else {
                tr.fu_spot_detail_std
                    .replacen("{}", &extras, 1)
                    .replacen("{}", &age, 1)
            };
            Line::from(vec![
                Span::styled(
                    format!("{:.2} USDC", s.usdc_total),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                dim(detail),
            ])
        }
    };
    f.render_widget(Paragraph::new(line), inner);
}

/// Fase de la transferencia interna en curso: firma EIP-712 en MetaMask →
/// aceptada → reflejada en el saldo destino (o fallo con motivo).
fn draw_transfer_strip(f: &mut Frame, xf: &TransferStatus, area: Rect) {
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_xfer_strip_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let dir = |to_perp: bool| {
        if to_perp {
            tr.fu_dir_to_perps
        } else {
            tr.fu_dir_to_spot
        }
    };
    let line = match xf {
        TransferStatus::AwaitingWallet { usdc, to_perp } => Line::from(Span::styled(
            tr.fu_xfer_awaiting
                .replacen("{}", &format!("{usdc:.2}"), 1)
                .replacen("{}", dir(*to_perp), 1),
            Style::new().fg(Color::Yellow),
        )),
        TransferStatus::Accepted { usdc, to_perp } => Line::from(vec![
            Span::styled(
                tr.fu_xfer_accepted
                    .replacen("{}", &format!("{usdc:.2}"), 1)
                    .replacen("{}", dir(*to_perp), 1),
                Style::new().fg(Color::Yellow),
            ),
            dim(tr.fu_xfer_watching),
        ]),
        TransferStatus::Arrived { usdc, to_perp } => Line::from(vec![
            Span::styled(
                tr.fu_xfer_done
                    .replacen("{}", &format!("{usdc:.2}"), 1)
                    .replacen("{}", dir(*to_perp), 1),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            dim(tr.fu_xfer_reflected),
        ]),
        TransferStatus::Failed { error } => Line::from(vec![
            Span::styled(
                tr.fu_xfer_failed,
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::new().fg(Color::Red)),
        ]),
    };
    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), inner);
}

/// Modal de la transferencia spot⇄perps: sentido (alternable con Tab/←→ o
/// click) + cantidad → resumen de confirmación. El dinero nunca sale de la
/// cuenta: solo cambia de lado dentro de Hyperliquid.
fn draw_transfer_modal(f: &mut Frame, app: &mut App) {
    let Some(ui) = app.transfer_ui.clone() else {
        return;
    };
    let Some(route) = app.transfer_route() else {
        return;
    };
    let tr = crate::i18n::t();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let red = Style::new().fg(Color::Red);
    let cyan = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let dir_label = |to_perp: bool| {
        if to_perp {
            tr.fu_xfer_opt_to_perps
        } else {
            tr.fu_xfer_opt_to_spot
        }
    };
    match ui {
        TransferUi::Amount { to_perp, buf, err } => {
            let area = super::exec::centered(70, 11, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_xfer_modal_title.replacen("{}", route.hl_chain, 1))
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let avail = if to_perp {
                app.spot
                    .as_ref()
                    .map(|s| (s.usdc_total - s.usdc_hold).max(0.0))
            } else {
                app.funds.as_ref().map(|w| w.withdrawable)
            };
            let avail_txt = avail
                .map(|a| format!("{a:.2} USDC"))
                .unwrap_or_else(|| tr.fu_not_read_yet.into());
            let lines = vec![
                Line::raw(""),
                Line::from(vec![
                    Span::styled(tr.fu_direction_lbl, bold),
                    Span::styled(dir_label(to_perp), cyan),
                    dim(tr.fu_tab_changes),
                ]),
                Line::from(vec![
                    Span::styled(tr.fu_amount_usdc, bold),
                    Span::styled(format!("{buf}█"), cyan),
                ]),
                Line::from(dim(
                    tr.fu_avail_source_side.replacen("{}", &avail_txt, 1),
                )),
                match err {
                    Some(e) => Line::from(Span::styled(
                        format!("  ✗ {e}"),
                        red.add_modifier(Modifier::BOLD),
                    )),
                    None => Line::raw(""),
                },
            ];
            f.render_widget(Paragraph::new(lines), inner);
            // la línea del sentido es clicable (fila 1 del inner)
            hits.push((
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
                Hit::XferDir,
            ));
            super::exec::modal_buttons(f, inner, 7, tr.fu_btn_continue, &mut hits);
        }
        TransferUi::Confirm {
            to_perp,
            usdc: _,
            units,
        } => {
            let area = super::exec::centered(74, 12, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_xfer_confirm_title)
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let kv = |k: &str, v: String, st: Style| {
                Line::from(vec![dim(format!("  {k:<9}")), Span::styled(v, st)])
            };
            let lines = vec![
                Line::raw(""),
                kv(
                    tr.fu_kv_amount,
                    tr.fu_amount_on_net
                        .replacen("{}", &fmt_usdc(units), 1)
                        .replacen("{}", route.hl_chain, 1),
                    bold,
                ),
                kv(
                    tr.fu_kv_direction,
                    dir_label(to_perp).to_string(),
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Line::from(dim(tr.fu_xfer_note1)),
                Line::from(dim(tr.fu_xfer_note2)),
                Line::raw(""),
                Line::from(dim(tr.fu_gasless1)),
                Line::from(dim(tr.fu_gasless_xfer2)),
            ];
            f.render_widget(Paragraph::new(lines), inner);
            super::exec::modal_buttons(f, inner, 9, tr.fu_btn_sign, &mut hits);
        }
    }
    app.exec.hits.extend(hits);
}

/// Saldo USDC on-chain de la wallet (Pieza 1 del depósito) — deliberadamente
/// en su propio bloque, separado del saldo DENTRO de Hyperliquid de abajo:
/// son dos custodias distintas y no deben mezclarse visualmente.
fn draw_usdc_strip(f: &mut Frame, app: &App, chain: &str, area: Rect) {
    let net = match chain_label(chain) {
        "" => chain,
        l => l,
    };
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_onchain_title.replacen("{}", net, 1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = match app.usdc {
        None => Line::from(Span::styled(
            tr.fu_rpc_loading,
            Style::new().fg(Color::Yellow),
        )),
        Some(None) => Line::from(dim(tr.fu_no_usdc_read)),
        Some(Some(v)) => {
            let age = app
                .usdc_at
                .map(|t| format!("{}s", t.elapsed().as_secs()))
                .unwrap_or_else(|| "—".into());
            let dep_hint = if crate::data::deposit_route(chain).is_some() {
                tr.fu_dep_hint_ok
            } else {
                tr.fu_dep_hint_mainnet_only
            };
            Line::from(vec![
                Span::styled(
                    format!("{v:.2} USDC"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                dim(tr
                    .fu_onchain_detail
                    .replacen("{}", &age, 1)
                    .replacen("{}", dep_hint, 1)),
            ])
        }
    };
    f.render_widget(Paragraph::new(line), inner);
}

/// Fase del depósito real en curso: firma en MetaMask → tx en vuelo →
/// confirmada on-chain (o fallo). Queda visible hasta el siguiente depósito.
fn draw_deposit_strip(f: &mut Frame, dep: &DepositStatus, area: Rect) {
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_dep_strip_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = match dep {
        DepositStatus::AwaitingWallet { usdc } => Line::from(Span::styled(
            tr.fu_dep_awaiting.replacen("{}", &format!("{usdc:.2}"), 1),
            Style::new().fg(Color::Yellow),
        )),
        DepositStatus::Submitted { usdc, tx } => Line::from(vec![
            Span::styled(
                tr.fu_dep_signed.replacen("{}", &format!("{usdc:.2}"), 1),
                Style::new().fg(Color::Yellow),
            ),
            dim(tr.fu_dep_waiting_chain.replacen("{}", tx, 1)),
        ]),
        DepositStatus::Confirmed { usdc, tx } => Line::from(vec![
            Span::styled(
                tr.fu_dep_confirmed.replacen("{}", &format!("{usdc:.2}"), 1),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            dim(tr.fu_dep_credits.replacen("{}", tx, 1)),
        ]),
        DepositStatus::Failed { error } => Line::from(vec![
            Span::styled(
                tr.fu_dep_failed,
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::new().fg(Color::Red)),
        ]),
    };
    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), inner);
}

/// Fase del retiro real en curso: firma EIP-712 en MetaMask → aceptado por
/// Hyperliquid → USDC llegado a la wallet (o fallo con motivo).
fn draw_withdraw_strip(f: &mut Frame, wd: &WithdrawStatus, area: Rect) {
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_wd_strip_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = match wd {
        WithdrawStatus::AwaitingWallet { usdc } => Line::from(Span::styled(
            tr.fu_wd_awaiting.replacen("{}", &format!("{usdc:.2}"), 1),
            Style::new().fg(Color::Yellow),
        )),
        WithdrawStatus::Accepted { usdc } => Line::from(vec![
            Span::styled(
                tr.fu_wd_accepted.replacen("{}", &format!("{usdc:.2}"), 1),
                Style::new().fg(Color::Yellow),
            ),
            dim(tr.fu_wd_processing),
        ]),
        WithdrawStatus::Arrived { usdc } => Line::from(vec![
            Span::styled(
                tr.fu_wd_done,
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            dim(tr
                .fu_wd_net_detail
                .replacen("{}", &format!("{usdc:.2}"), 1)
                .replacen("{}", &format!("{:.2}", usdc - 1.0), 1)),
        ]),
        WithdrawStatus::Failed { error } => Line::from(vec![
            Span::styled(
                tr.fu_wd_failed,
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::new().fg(Color::Red)),
        ]),
    };
    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), inner);
}

/// Fase de la autorización del agent (paso 6): firma EIP-712 en MetaMask →
/// aceptada + clave guardada → verificada en extraAgents (o fallo).
fn draw_agent_strip(f: &mut Frame, ag: &AgentStatus, area: Rect) {
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_ag_strip_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let short = |a: &str| {
        if a.len() > 14 {
            format!("{}…{}", &a[..8], &a[a.len() - 4..])
        } else {
            a.to_string()
        }
    };
    let line = match ag {
        AgentStatus::AwaitingWallet { agent } => Line::from(Span::styled(
            tr.fu_ag_awaiting.replacen("{}", &short(agent), 1),
            Style::new().fg(Color::Yellow),
        )),
        AgentStatus::Accepted { agent, path } => Line::from(vec![
            Span::styled(
                tr.fu_ag_authorized.replacen("{}", &short(agent), 1),
                Style::new().fg(Color::Yellow),
            ),
            dim(tr.fu_ag_checking.replacen("{}", path, 1)),
        ]),
        AgentStatus::Verified { agent, path } => Line::from(vec![
            Span::styled(
                tr.fu_ag_verified.replacen("{}", &short(agent), 1),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            dim(tr.fu_ag_verified_detail.replacen("{}", path, 1)),
        ]),
        AgentStatus::Unlisted { agent, path } => Line::from(vec![
            Span::styled(
                tr.fu_ag_ok_exchange.replacen("{}", &short(agent), 1),
                Style::new().fg(Color::Yellow),
            ),
            dim(tr.fu_ag_not_listed.replacen("{}", path, 1)),
        ]),
        AgentStatus::Failed { error } => Line::from(vec![
            Span::styled(
                tr.fu_ag_failed,
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::new().fg(Color::Red)),
        ]),
    };
    f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), inner);
}

/// Modal de la autorización del agent (paso 6): un único resumen con la
/// dirección EXACTA del agent nuevo, qué permisos recibe (y cuáles NO), y
/// dónde quedará la clave — todo ANTES de pedir la firma a la maestra.
fn draw_agent_modal(f: &mut Frame, app: &mut App) {
    let Some(ui) = app.agent_ui.clone() else {
        return;
    };
    let Some((route, _)) = app.withdraw_route() else {
        return;
    };
    let master = match &app.wc {
        WcStatus::Connected(s) => s.address.clone(),
        _ => return,
    };
    let tr = crate::i18n::t();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let area = super::exec::centered(78, 16, f.area());
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .title(tr.fu_ag_modal_title.replacen("{}", route.hl_chain, 1))
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let kv = |k: &str, v: String, st: Style| {
        Line::from(vec![dim(format!("  {k:<9}")), Span::styled(v, st)])
    };
    let replaces = match &ui.replaces {
        Some(prev) => Line::from(Span::styled(
            tr.fu_ag_invalidates.replacen("{}", prev, 1),
            Style::new().fg(Color::Yellow),
        )),
        None => Line::from(dim(tr.fu_ag_no_previous)),
    };
    let lines = vec![
        Line::raw(""),
        kv(
            tr.fu_kv_agent,
            ui.agent_addr.clone(),
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Line::from(dim(tr.fu_ag_note1)),
        Line::from(dim(tr.fu_ag_note2)),
        kv(tr.fu_kv_master, master, Style::new().fg(Color::Cyan)),
        kv(tr.fu_kv_perms, tr.fu_ag_perms_val.into(), bold),
        kv(
            tr.fu_kv_key,
            tr.fu_ag_key_val.replacen(
                "{}",
                &crate::wallet::agent::key_path(route.hl_chain)
                    .display()
                    .to_string(),
                1,
            ),
            Style::new().fg(Color::Gray),
        ),
        replaces,
        Line::raw(""),
        Line::from(dim(tr.fu_gasless1)),
        Line::from(dim(tr.fu_ag_gasless2)),
    ];
    f.render_widget(Paragraph::new(lines), inner);
    super::exec::modal_buttons(f, inner, 13, tr.fu_btn_sign, &mut hits);
    app.exec.hits.extend(hits);
}

/// Modal del retiro real (paso 5): cantidad → resumen con la dirección de
/// destino EXACTA (la propia maestra) antes de pedir la firma EIP-712.
fn draw_withdraw_modal(f: &mut Frame, app: &mut App) {
    let Some(ui) = app.withdraw_ui.clone() else {
        return;
    };
    let Some((route, avail)) = app.withdraw_route() else {
        return;
    };
    let dest = match &app.wc {
        WcStatus::Connected(s) => s.address.clone(),
        _ => return,
    };
    let tr = crate::i18n::t();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let red = Style::new().fg(Color::Red);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    match ui {
        WithdrawUi::Amount { buf, err } => {
            let area = super::exec::centered(66, 10, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_wd_modal_title.replacen("{}", route.hl_chain, 1))
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let lines = vec![
                Line::raw(""),
                Line::from(vec![
                    Span::styled(tr.fu_amount_usdc, bold),
                    Span::styled(
                        format!("{buf}█"),
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(dim(
                    tr.fu_wd_withdrawable
                        .replacen("{}", &format!("{avail:.2}"), 1),
                )),
                Line::from(Span::styled(tr.fu_wd_fee_note, red)),
                match err {
                    Some(e) => Line::from(Span::styled(
                        format!("  ✗ {e}"),
                        red.add_modifier(Modifier::BOLD),
                    )),
                    None => Line::raw(""),
                },
            ];
            f.render_widget(Paragraph::new(lines), inner);
            super::exec::modal_buttons(f, inner, 6, tr.fu_btn_continue, &mut hits);
        }
        WithdrawUi::Confirm { units, .. } => {
            let area = super::exec::centered(78, 14, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_wd_confirm_title)
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let kv = |k: &str, v: String, st: Style| {
                Line::from(vec![dim(format!("  {k:<10}")), Span::styled(v, st)])
            };
            let net = match chain_label(&format!("eip155:{}", route.chain_id)) {
                "" => route.hl_chain.to_string(),
                l => l.to_string(),
            };
            let recibes = fmt_usdc(units.saturating_sub(crate::data::WITHDRAW_FEE_UNITS));
            let lines = vec![
                Line::raw(""),
                kv(
                    tr.fu_kv_amount,
                    tr.fu_wd_amount_val
                        .replacen("{}", &fmt_usdc(units), 1)
                        .replacen("{}", route.hl_chain, 1)
                        .replacen("{}", &net, 1),
                    bold,
                ),
                kv(
                    tr.fu_kv_receive,
                    tr.fu_wd_receive_val.replacen("{}", &recibes, 1),
                    bold,
                ),
                kv(
                    tr.fu_kv_dest,
                    dest,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Line::from(dim(tr.fu_wd_dest_note1)),
                Line::from(dim(tr.fu_wd_dest_note2)),
                Line::raw(""),
                Line::from(dim(tr.fu_gasless1)),
                Line::from(dim(tr.fu_wd_gasless2)),
            ];
            f.render_widget(Paragraph::new(lines), inner);
            super::exec::modal_buttons(f, inner, 11, tr.fu_btn_sign, &mut hits);
        }
    }
    app.exec.hits.extend(hits);
}

/// Modal del depósito real (Pieza 2): cantidad → resumen con la dirección de
/// destino EXACTA antes de pedir la firma. Teclado y ratón equivalentes.
fn draw_deposit_modal(f: &mut Frame, app: &mut App) {
    let Some(ui) = app.deposit_ui.clone() else {
        return;
    };
    let Some((route, bal)) = app.deposit_route() else {
        return;
    };
    let from = match &app.wc {
        WcStatus::Connected(s) => s.address.clone(),
        _ => return,
    };
    // checksummed EIP-55, el mismo formato en que MetaMask la mostrará
    let bridge = route
        .bridge
        .parse::<alloy_primitives::Address>()
        .map(|a| format!("{a}"))
        .unwrap_or_else(|_| route.bridge.to_string());
    let tr = crate::i18n::t();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let red = Style::new().fg(Color::Red);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    match ui {
        DepositUi::Amount { buf, err } => {
            let area = super::exec::centered(66, 10, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_dep_modal_title)
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let lines = vec![
                Line::raw(""),
                Line::from(vec![
                    Span::styled(tr.fu_amount_usdc, bold),
                    Span::styled(
                        format!("{buf}█"),
                        Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(dim(
                    tr.fu_dep_onchain_avail
                        .replacen("{}", &format!("{bal:.2}"), 1),
                )),
                Line::from(Span::styled(tr.fu_dep_min_note, red)),
                match err {
                    Some(e) => Line::from(Span::styled(
                        format!("  ✗ {e}"),
                        red.add_modifier(Modifier::BOLD),
                    )),
                    None => Line::raw(""),
                },
            ];
            f.render_widget(Paragraph::new(lines), inner);
            super::exec::modal_buttons(f, inner, 6, tr.fu_btn_continue, &mut hits);
        }
        DepositUi::Confirm { units, .. } => {
            let area = super::exec::centered(78, 13, f.area());
            f.render_widget(Clear, area);
            let block = Block::bordered()
                .title(tr.fu_dep_confirm_title)
                .border_style(Style::new().fg(Color::Yellow));
            let inner = block.inner(area);
            f.render_widget(block, area);
            let kv = |k: &str, v: String, st: Style| {
                Line::from(vec![dim(format!("  {k:<9}")), Span::styled(v, st)])
            };
            let lines = vec![
                Line::raw(""),
                kv(
                    tr.fu_kv_amount,
                    tr.fu_dep_amount_val.replacen("{}", &fmt_usdc(units), 1),
                    bold,
                ),
                kv(tr.fu_kv_from, from, Style::new().fg(Color::Cyan)),
                Line::from(dim(tr.fu_dep_from_note)),
                kv(
                    tr.fu_kv_dest,
                    bridge,
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ),
                Line::from(dim(tr.fu_dep_dest_note1)),
                Line::from(dim(tr.fu_dep_dest_note2)),
                Line::raw(""),
                Line::from(dim(tr.fu_dep_note1)),
                Line::from(dim(tr.fu_dep_note2)),
            ];
            f.render_widget(Paragraph::new(lines), inner);
            super::exec::modal_buttons(f, inner, 10, tr.fu_btn_sign, &mut hits);
        }
    }
    app.exec.hits.extend(hits);
}

fn dim(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), Style::new().fg(Color::DarkGray))
}

/// Estado de la cuenta maestra en una línea: la Vista 8 ahora la ocupa el
/// panel de ejecución (maqueta) y la conexión WC queda como cabecera.
fn draw_wc_strip(f: &mut Frame, wc: &WcStatus, area: Rect) {
    let tr = crate::i18n::t();
    let block = Block::bordered().title(tr.fu_wc_strip_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = match wc {
        WcStatus::Idle => Line::from(vec![
            Span::styled(tr.fu_wc_disconnected, Style::new().fg(Color::Gray)),
            dim(tr.fu_wc_connect_hint),
        ]),
        WcStatus::Connecting => Line::from(Span::styled(
            tr.fu_wc_connecting,
            Style::new().fg(Color::Yellow),
        )),
        WcStatus::WaitingSettle => Line::from(Span::styled(
            tr.fu_wc_establishing,
            Style::new().fg(Color::Yellow),
        )),
        WcStatus::Connected(s) => {
            let mins = s.since.elapsed().as_secs() / 60;
            let topic_short = if s.session_topic.len() > 12 {
                format!(
                    "{}…{}",
                    &s.session_topic[..6],
                    &s.session_topic[s.session_topic.len() - 6..]
                )
            } else {
                s.session_topic.clone()
            };
            let peer = s.peer.clone().unwrap_or_else(|| "—".into());
            let mins = mins.to_string();
            Line::from(vec![
                Span::styled(
                    tr.fu_wc_connected,
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(s.address.clone(), Style::new().fg(Color::Cyan)),
                dim(tr
                    .fu_wc_session_detail
                    .replacen("{}", chain_label(&s.chain), 1)
                    .replacen("{}", &peer, 1)
                    .replacen("{}", &topic_short, 1)
                    .replacen("{}", &mins, 1)),
            ])
        }
        WcStatus::Failed { error } => Line::from(vec![
            Span::styled(
                tr.fu_wc_failed,
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::new().fg(Color::Red)),
            dim(tr.fu_wc_retry),
        ]),
        // WaitingScan se pinta a pantalla completa antes de llegar aquí
        WcStatus::WaitingScan { .. } => Line::raw(""),
    };
    f.render_widget(Paragraph::new(line), inner);
}

fn chain_label(chain: &str) -> &'static str {
    match chain {
        "eip155:42161" => crate::i18n::t().fu_chain_arb,
        "eip155:421614" => crate::i18n::t().fu_chain_arb_sepolia,
        _ => "",
    }
}

fn draw_qr(f: &mut Frame, area: Rect, uri: &str, qr: &str, secs_left: u64) {
    let tr = crate::i18n::t();
    let qr_lines: Vec<&str> = qr.lines().collect();
    let qr_h = qr_lines.len() as u16;
    let qr_w = qr_lines
        .first()
        .map(|l| l.chars().count() as u16)
        .unwrap_or(0);

    // sin sitio para el QR: al menos dejar la URI copiable
    if area.height < qr_h + 3 || area.width < qr_w {
        let lines = vec![
            Line::raw(""),
            Line::from(Span::styled(
                tr.fu_qr_too_small
                    .replacen("{}", &qr_w.to_string(), 1)
                    .replacen("{}", &(qr_h + 3).to_string(), 1)
                    .replacen("{}", &area.width.to_string(), 1)
                    .replacen("{}", &area.height.to_string(), 1),
                Style::new().fg(Color::Yellow),
            )),
            Line::raw(tr.fu_qr_enlarge),
            Line::raw(""),
            Line::from(Span::styled(
                format!("  {uri}"),
                Style::new().fg(Color::Cyan),
            )),
            Line::raw(""),
            Line::from(dim(
                tr.fu_qr_expires.replacen("{}", &secs_left.to_string(), 1),
            )),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(qr_h),
        Constraint::Min(1),
    ])
    .split(area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                tr.fu_qr_scan_with,
                Style::new().add_modifier(Modifier::BOLD),
            ),
            dim(tr.fu_qr_scan_icon.replacen("{}", &secs_left.to_string(), 1)),
        ]))
        .alignment(Alignment::Center),
        rows[0],
    );

    // blanco sobre negro explícito para que el QR no dependa del tema del terminal
    let x = area.x + (area.width - qr_w) / 2;
    let qr_area = Rect::new(x, rows[1].y, qr_w, qr_h);
    let lines: Vec<Line> = qr_lines
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::new().fg(Color::White).bg(Color::Black),
            ))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), qr_area);

    let mut foot = vec![Line::from(dim(tr.fu_qr_foot))];
    if rows[2].height >= 3 {
        foot.push(Line::raw(""));
        foot.push(Line::from(Span::styled(
            format!("URI: {uri}"),
            Style::new().fg(Color::DarkGray),
        )));
    }
    f.render_widget(
        Paragraph::new(foot)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        rows[2],
    );
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::{mpsc, watch};

    use crate::app::{App, DepositUi};
    use crate::data::types::DataMsg;
    use crate::ui::oscimg::Gfx;
    use crate::wallet::walletconnect::{DepositStatus, WcSession, WcStatus};

    fn app_conectada() -> App {
        std::env::set_var("CHART_PROTO", "halfblocks");
        let (extra_tx, _extra) = mpsc::channel(8);
        let (wallet_tx, _wallet) = watch::channel(Vec::new());
        let (usdc_tx, _usdc) = watch::channel(None);
        let (coin_tx, _coin) = watch::channel(None);
        let (wc_tx, _wc) = mpsc::unbounded_channel();
        let mut app = App::new(extra_tx, wallet_tx, usdc_tx, coin_tx, wc_tx, "test", Gfx::new());
        app.apply_msg(DataMsg::Wc(WcStatus::Connected(WcSession {
            address: "0x000000000000000000000000000000000000dEaD".into(),
            chain: "eip155:42161".into(),
            peer: None,
            since: std::time::Instant::now(),
            session_topic: "topic".into(),
        })));
        let master = app
            .deposit_route()
            .map(|_| unreachable!("sin saldo aún no hay ruta"))
            .unwrap_or_else(|| "0x000000000000000000000000000000000000dEaD".to_string());
        app.apply_msg(DataMsg::UsdcBalance {
            addr: master,
            usdc: Some(20.0),
        });
        app
    }

    fn pantalla(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(100, 42)).unwrap();
        term.draw(|f| super::draw(f, app, f.area())).unwrap();
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

    /// El resumen de confirmación muestra la dirección EXACTA del bridge
    /// (checksummed, como la mostrará MetaMask) y la cantidad exacta.
    #[test]
    fn modal_muestra_destino_y_cantidad_exactos() {
        let mut app = app_conectada();
        app.deposit_ui = Some(DepositUi::Confirm {
            usdc: 7.5,
            units: 7_500_000,
        });
        let s = pantalla(&mut app);
        assert!(
            s.contains("0x2Df1c51E09aECF9cacB7bc98cB1742757f163dF7"),
            "falta la dirección checksummed del bridge:\n{s}"
        );
        assert!(s.contains("7.5 USDC"), "falta la cantidad exacta:\n{s}");
        assert!(s.contains("Sign in MetaMask"), "falta el botón de firma:\n{s}");
    }

    /// El resumen del retiro muestra la dirección de destino EXACTA (la
    /// propia maestra), lo que llega tras la comisión, y que es una firma
    /// gasless — todo ANTES de firmar.
    #[test]
    fn modal_de_retiro_muestra_destino_y_neto() {
        use crate::app::WithdrawUi;
        use crate::data::types::AccountSnapshot;

        let mut app = app_conectada();
        // withdrawable real: sin él la ruta del retiro no existe. La dirección
        // es la checksummed de la sesión del fixture (formato canónico).
        let master = "0x000000000000000000000000000000000000dEaD".to_string();
        app.apply_msg(DataMsg::WalletState(AccountSnapshot {
            addr: master.clone(),
            account_value: 1000.0,
            withdrawable: 1000.0,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: Vec::new(),
        }));
        app.withdraw_ui = Some(WithdrawUi::Confirm {
            usdc: 10.0,
            units: 10_000_000,
        });
        let s = pantalla(&mut app);
        assert!(
            s.contains(&master),
            "falta la dirección de destino (la maestra):\n{s}"
        );
        assert!(s.contains("9 USDC"), "falta el neto tras la comisión:\n{s}");
        assert!(s.contains("Signature request"), "falta el aviso gasless:\n{s}");
        assert!(s.contains("Sign in MetaMask"), "falta el botón de firma:\n{s}");

        // y las fases del retiro se pintan en su tira
        app.withdraw_ui = None;
        app.apply_msg(DataMsg::Withdraw(
            crate::wallet::walletconnect::WithdrawStatus::Accepted { usdc: 10.0 },
        ));
        let s = pantalla(&mut app);
        assert!(
            s.contains("accepted by Hyperliquid"),
            "falta la fase aceptada:\n{s}"
        );
    }

    /// El resumen del agent (abierto por el camino real: tecla `a`) muestra
    /// la dirección nueva completa, los permisos (sin retiro), la ruta de la
    /// clave y el aviso de la Signature request; la tira pinta las fases.
    #[test]
    fn modal_de_agent_muestra_direccion_y_permisos() {
        use crate::app::View;
        use crate::data::types::AccountSnapshot;
        use crate::wallet::walletconnect::AgentStatus;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = app_conectada();
        let master = "0x000000000000000000000000000000000000dEaD".to_string();
        app.apply_msg(DataMsg::WalletState(AccountSnapshot {
            addr: master,
            account_value: 100.0,
            withdrawable: 100.0,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: Vec::new(),
        }));
        app.view = View::Funds;
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        let agent_addr = app
            .agent_ui
            .as_ref()
            .expect("la tecla a debe abrir el modal")
            .agent_addr
            .clone();

        let s = pantalla(&mut app);
        assert!(
            s.contains(&agent_addr),
            "falta la dirección completa del agent nuevo:\n{s}"
        );
        assert!(
            s.contains("NOT withdraw"),
            "falta el límite de permisos:\n{s}"
        );
        assert!(
            s.contains("agent_mainnet.json"),
            "falta la ruta de la clave (sesión mainnet):\n{s}"
        );
        assert!(
            s.contains("ApproveAgent"),
            "falta el nombre del typed data a comparar en MetaMask:\n{s}"
        );
        assert!(s.contains("Sign in MetaMask"), "falta el botón de firma:\n{s}");

        // fases en la tira: verificado con su ruta
        app.agent_ui = None;
        app.apply_msg(DataMsg::Agent(AgentStatus::Verified {
            agent: agent_addr,
            path: "secrets/agent_mainnet.json".into(),
        }));
        let s = pantalla(&mut app);
        assert!(
            s.contains("authorized and verified"),
            "falta la fase verificada:\n{s}"
        );
    }

    /// El saldo SPOT tiene su bloque propio, separado del de PERPS, con
    /// ambos visibles a la vez — la confusión de saldos no puede repetirse.
    /// Y el modal de transferencia muestra sentido y disponible del origen.
    #[test]
    fn spot_y_perps_separados_y_modal_de_transferencia() {
        use crate::app::{TransferUi, View};
        use crate::data::types::{AccountSnapshot, SpotSnapshot};
        use crate::wallet::walletconnect::TransferStatus;
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = app_conectada();
        let master = "0x000000000000000000000000000000000000dEaD".to_string();
        app.apply_msg(DataMsg::WalletState(AccountSnapshot {
            addr: master.clone(),
            account_value: 5.0,
            withdrawable: 5.0,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: Vec::new(),
        }));
        app.apply_msg(DataMsg::SpotState(SpotSnapshot {
            addr: master,
            usdc_total: 999.0,
            usdc_hold: 0.0,
            usdc_avail: None,
            others: vec![("HORSE".into(), 12.0)],
        }));

        let s = pantalla(&mut app);
        assert!(
            s.contains("SPOT USDC inside Hyperliquid"),
            "falta el bloque de saldo spot:\n{s}"
        );
        assert!(
            s.contains("999.00 USDC"),
            "falta la cantidad spot:\n{s}"
        );
        assert!(
            s.contains("PERPS balance inside Hyperliquid"),
            "falta el bloque de perps renombrado:\n{s}"
        );
        assert!(
            s.contains("+1 spot tokens with balance"),
            "faltan los otros tokens spot:\n{s}"
        );

        // modal por el camino real (tecla t): sentido por defecto spot→perps
        app.view = View::Funds;
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        let s = pantalla(&mut app);
        assert!(
            s.contains("Spot → Perps"),
            "falta el sentido por defecto:\n{s}"
        );
        assert!(
            s.contains("available on the source side: 999.00 USDC"),
            "falta el disponible del origen:\n{s}"
        );

        // resumen de confirmación: aviso de interna + typed data a comparar
        app.transfer_ui = Some(TransferUi::Confirm {
            to_perp: true,
            usdc: 10.0,
            units: 10_000_000,
        });
        let s = pantalla(&mut app);
        assert!(
            s.contains("does NOT leave your Hyperliquid account"),
            "falta el aviso de transferencia interna:\n{s}"
        );
        assert!(
            s.contains("UsdClassTransfer"),
            "falta el nombre del typed data:\n{s}"
        );
        assert!(s.contains("Sign in MetaMask"), "falta el botón:\n{s}");

        // y las fases en su tira
        app.transfer_ui = None;
        app.apply_msg(DataMsg::Transfer(TransferStatus::Arrived {
            usdc: 10.0,
            to_perp: true,
        }));
        let s = pantalla(&mut app);
        assert!(
            s.contains("spot → perps — completed"),
            "falta la fase completada:\n{s}"
        );
    }

    /// Cuenta UNIFICADA (el caso real del usuario, verificado en vivo en
    /// mainnet y testnet): el bloque spot pasa a ser LA fuente de verdad (sin
    /// ofrecer `t`), el bloque de perps deja de pintar el "Valor cuenta $0"
    /// confuso, y el panel de ejecución muestra el margen disponible de spot.
    #[test]
    fn cuenta_unificada_saldos_y_margen_de_spot() {
        use crate::app::View;
        use crate::data::types::{AccountMode, AccountSnapshot, SpotSnapshot};
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = app_conectada();
        let master = "0x000000000000000000000000000000000000dEaD".to_string();
        app.apply_msg(DataMsg::AccountMode {
            addr: master.clone(),
            mode: AccountMode::Unified,
        });
        // perps a 0 (respuesta real de una cuenta unificada) + spot con el
        // disponible tras mantenimiento
        app.apply_msg(DataMsg::WalletState(AccountSnapshot {
            addr: master.clone(),
            account_value: 0.0,
            withdrawable: 0.0,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: Vec::new(),
        }));
        app.apply_msg(DataMsg::SpotState(SpotSnapshot {
            addr: master,
            usdc_total: 5.000708,
            usdc_hold: 0.0,
            usdc_avail: Some(5.000708),
            others: Vec::new(),
        }));

        let s = pantalla(&mut app);
        assert!(
            s.contains("UNIFIED account (spot+perps, single margin)"),
            "falta el título unificado del bloque spot:\n{s}"
        );
        assert!(
            s.contains("= perps margin (unified, avail. 5.00"),
            "falta la aclaración de margen unificado:\n{s}"
        );
        assert!(
            s.contains("t does not apply"),
            "el bloque spot no debe ofrecer t:\n{s}"
        );
        assert!(
            !s.contains("t transfers"),
            "no debe quedar ningún hint de t:\n{s}"
        );
        assert!(
            !s.contains("t ⇄ spot"),
            "el hint de perps no debe ofrecer t:\n{s}"
        );
        assert!(
            s.contains("UNIFIED mode: spot and perps share margin"),
            "falta la nota del bloque perps:\n{s}"
        );
        assert!(
            !s.contains("Account value"),
            "el Valor cuenta $0 confuso no debe pintarse:\n{s}"
        );
        assert!(
            s.contains("Margin avail."),
            "falta el margen disponible del panel de ejecución:\n{s}"
        );
        assert!(
            s.contains("$5.00") && s.contains("(unified: spot)"),
            "el margen disponible debe salir de spot:\n{s}"
        );

        // `t` por el camino real: mensaje claro en su tira, sin modal
        app.view = View::Funds;
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert!(app.transfer_ui.is_none());
        let s = pantalla(&mut app);
        assert!(
            s.contains("does not apply: UNIFIED account"),
            "falta el mensaje claro al pulsar t:\n{s}"
        );
    }

    /// Panel de ejecución REAL armado (paso 7, testnet): el panel se rotula
    /// REAL de arriba a abajo — sin sesión WC siquiera — y las filas vienen
    /// de la cuenta de verdad, no de la siembra demo.
    #[test]
    fn panel_real_se_rotula_y_pinta_cuenta_real() {
        use crate::data::types::{AccountSnapshot, PosInfo};
        use tokio::sync::mpsc;

        std::env::set_var("CHART_PROTO", "halfblocks");
        let (extra_tx, _extra) = mpsc::channel(8);
        let (wallet_tx, _wallet) = tokio::sync::watch::channel(Vec::new());
        let (usdc_tx, _usdc) = tokio::sync::watch::channel(None);
        let (coin_tx, _coin) = tokio::sync::watch::channel(None);
        let (wc_tx, _wc) = mpsc::unbounded_channel();
        let mut app = App::new(extra_tx, wallet_tx, usdc_tx, coin_tx, wc_tx, "testnet", Gfx::new());
        let (trade_tx, _trade_rx) = mpsc::unbounded_channel();
        app.arm_trading(
            "0x000000000000000000000000000000000000dEaD".parse().unwrap(),
            "0xAGENTEagenteAGENTEagente".into(),
            trade_tx,
        );
        let master = app.trade.as_ref().unwrap().master_fmt.clone();
        app.apply_msg(DataMsg::WalletState(AccountSnapshot {
            addr: master,
            account_value: 500.0,
            withdrawable: 500.0,
            total_margin_used: 0.0,
            total_ntl_pos: 0.0,
            positions: vec![PosInfo {
                coin: "BTC".into(),
                szi: 0.01,
                entry_px: Some(100_000.0),
                position_value: 1_000.0,
                unrealized_pnl: 0.0,
                roe: 0.0,
                leverage: 10,
                is_cross: false,
                liq_px: Some(90_500.0),
                since_open_funding: 0.0,
            }],
        }));

        // pantalla ancha: con 100 columnas la tabla recorta la columna Liq
        let mut term = Terminal::new(TestBackend::new(160, 42)).unwrap();
        term.draw(|f| super::draw(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
            }
            s.push('\n');
        }
        assert!(
            s.contains("Order — REAL (testnet"),
            "falta el título REAL del formulario:\n{s}"
        );
        assert!(
            s.contains("Positions — REAL (1)"),
            "faltan las posiciones reales:\n{s}"
        );
        assert!(
            s.contains("Open orders — REAL"),
            "falta el rótulo real de órdenes:\n{s}"
        );
        assert!(
            !s.contains("mock"),
            "en modo real no debe quedar rastro de maqueta:\n{s}"
        );
        assert!(
            s.contains("agent signs 0xAGENTE"),
            "falta el agent en la línea de estado:\n{s}"
        );
        // la posición real pintada con su liq exacta de la API
        assert!(s.contains("90500"), "falta la liq real:\n{s}");
    }

    /// El paso de cantidad avisa del mínimo del bridge, y las fases del
    /// depósito se pintan en su tira.
    #[test]
    fn modal_de_cantidad_y_tira_de_estado() {
        let mut app = app_conectada();
        app.deposit_ui = Some(DepositUi::Amount {
            buf: "7.5".into(),
            err: None,
        });
        let s = pantalla(&mut app);
        assert!(s.contains("minimum 5 USDC"), "falta el aviso del mínimo:\n{s}");

        app.deposit_ui = None;
        app.deposit = Some(DepositStatus::Confirmed {
            usdc: 7.5,
            tx: "0xabc123".into(),
        });
        let s = pantalla(&mut app);
        assert!(
            s.contains("7.50 USDC confirmed on-chain"),
            "falta la fase confirmada:\n{s}"
        );
    }
}
