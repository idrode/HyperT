mod exec;
mod flow;
pub mod fmt;
mod fondos;
mod heatmap;
mod help;
mod indsel;
mod liq;
pub(crate) mod oscimg;
mod pair;
mod ranking;
mod search;
mod taplot;
pub mod theme;
pub mod wallet;
mod whalersi;
mod whales;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::app::{App, View};

pub fn draw(f: &mut Frame, app: &mut App) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(f.area());

    // Lo marcan los sitios de dibujo de overlays de este frame (ver App).
    app.overlay_drawn.set(false);
    draw_header(f, app, rows[0]);
    match app.view {
        View::Ranking => ranking::draw(f, app, rows[1]),
        View::Pair => pair::draw(f, app, rows[1]),
        View::Heatmap => heatmap::draw(f, app, rows[1]),
        View::Whales => whales::draw(f, app, rows[1]),
        View::Wallet => wallet::draw(f, app, rows[1]),
        View::Liq => liq::draw(f, app, rows[1]),
        View::WhaleRsi => whalersi::draw(f, app, rows[1]),
        View::Funds => fondos::draw(f, app, rows[1]),
        View::Flow => flow::draw(f, app, rows[1]),
    }
    draw_footer(f, app, rows[2]);
    if app.search.active {
        app.overlay_drawn.set(true);
        search::draw(f, app);
    }
    if app.show_help {
        app.overlay_drawn.set(true);
        help::draw(f);
    }
    if app.ind_ui.is_some() && matches!(app.view, View::Pair | View::WhaleRsi) {
        app.overlay_drawn.set(true);
        indsel::draw(f, app);
    }
    if app.input_mode {
        app.overlay_drawn.set(true);
        wallet::draw_input(f, app);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(30)]).split(area);

    let tab = |label: &str, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(Color::Gray))
        }
    };
    let s = crate::i18n::t();
    let left = Line::from(vec![
        Span::styled(
            " hyperT ",
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        tab(&format!("1 {}", s.tab_ranking), app.view == View::Ranking),
        tab(&format!("2 {}", s.tab_pair), app.view == View::Pair),
        tab(&format!("3 {}", s.tab_brsi), app.view == View::WhaleRsi),
        tab(&format!("4 {}", s.tab_heatmap), app.view == View::Heatmap),
        tab(&format!("5 {}", s.tab_liqs), app.view == View::Liq),
        tab(&format!("6 {}", s.tab_flow), app.view == View::Flow),
        tab(&format!("7 {}", s.tab_whales), app.view == View::Whales),
        tab(&format!("8 {}", s.tab_funds), app.view == View::Funds),
        tab(&format!("9 {}", s.tab_wallet), app.view == View::Wallet),
    ]);
    f.render_widget(Paragraph::new(left), cols[0]);

    // ● solo si además de conectado hay mensajes recientes: una suscripción
    // perdida en silencio debe verse como ○ aunque el socket siga abierto
    let ws = if app.ws_ok && app.ws_fresh() {
        Span::styled("WS ●", Style::new().fg(Color::Green))
    } else {
        Span::styled("WS ○", Style::new().fg(Color::Red))
    };
    let ws_age = match app.last_ws_at {
        Some(t) => format!(" {}s", t.elapsed().as_secs()),
        None => " —".to_string(),
    };
    let age = match app.last_ctx_at {
        Some(t) => format!("  ctx {}s ", t.elapsed().as_secs()),
        None => "  ctx — ".to_string(),
    };
    let right = Line::from(vec![
        Span::styled(app.net_label, Style::new().fg(Color::Yellow)),
        Span::raw("  "),
        ws,
        Span::styled(ws_age, Style::new().fg(Color::DarkGray)),
        Span::styled(age, Style::new().fg(Color::DarkGray)),
    ]);
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let s = crate::i18n::t();
    let hint = if app.search.active {
        s.foot_search
    } else {
        match app.view {
            View::Ranking => s.foot_ranking,
            View::Pair | View::WhaleRsi => s.foot_pair,
            View::Heatmap => s.foot_heatmap,
            View::Whales => s.foot_whales,
            View::Wallet => s.foot_wallet,
            View::Liq => s.foot_liq,
            View::Funds => s.foot_funds,
            View::Flow => s.foot_flow,
        }
    };
    // el hueco del error se adapta al ancho real de la terminal: con 46
    // columnas fijas y un corte a 44 caracteres, un mensaje con detalle
    // (p. ej. el diagnóstico del escaneo de whales, que adjunta dirección y
    // error real) se cortaba SIEMPRE justo antes de la parte útil, dejando
    // solo el conteo. El hint de teclas conserva su sitio mínimo.
    let err_w = if app.last_err.is_some() {
        let hint_w = hint.chars().count() as u16 + 2;
        (area.width.saturating_sub(hint_w)).clamp(46, 200).min(area.width)
    } else {
        46
    };
    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(err_w)]).split(area);
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))),
        cols[0],
    );
    if let Some(err) = &app.last_err {
        let cap = cols[1].width.saturating_sub(2) as usize;
        let mut msg: String = err.chars().take(cap).collect();
        if err.chars().count() > cap {
            msg.push('…');
        }
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::new().fg(Color::Red)))
                .alignment(Alignment::Right),
            cols[1],
        );
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use tokio::sync::{mpsc, watch};

    use crate::app::App;
    use crate::data::types::DataMsg;
    use crate::ui::oscimg::Gfx;

    fn app_con_error(err: &str) -> App {
        std::env::set_var("CHART_PROTO", "halfblocks");
        let (extra_tx, _e) = mpsc::channel(8);
        let (wallet_tx, _w) = watch::channel(Vec::new());
        let (usdc_tx, _u) = watch::channel(None);
        let (coin_tx, _c) = watch::channel(None);
        let (wc_tx, _wc) = mpsc::unbounded_channel();
        let mut app = App::new(extra_tx, wallet_tx, usdc_tx, coin_tx, wc_tx, "test", Gfx::new());
        app.apply_msg(DataMsg::RestError(err.to_string()));
        app
    }

    /// El diagnóstico del escaneo de whales adjunta dirección + error real,
    /// pero el pie truncaba SIEMPRE a 44 caracteres con un hueco fijo de 46
    /// columnas: el prefijo ("whales: 27/100 cuentas fallaron · ") ya gasta
    /// 34, así que del detalle solo sobrevivían 10 caracteres — justo la
    /// parte que hace falta para diagnosticar. En una terminal ancha el
    /// detalle llega entero; en una estrecha se recorta sin romper el hint.
    #[test]
    fn el_pie_no_se_come_el_detalle_del_error_en_terminal_ancha() {
        let err = "whales: 27/100 cuentas fallaron · \
                   0xf8E3128CDE1234567890123456789012345678: error sending request";
        let render = |w: u16| {
            let mut app = app_con_error(err);
            let mut term = Terminal::new(TestBackend::new(w, 3)).unwrap();
            term.draw(|f| {
                let area = super::Rect::new(0, 2, w, 1);
                super::draw_footer(f, &app, area);
            })
            .unwrap();
            app.last_err = None; // silencia el aviso de no-usado tras el draw
            let b = term.backend().buffer().clone();
            (0..b.area.width)
                .map(|x| b.cell((x, 2)).map(|c| c.symbol()).unwrap_or(" ").to_string())
                .collect::<String>()
        };
        // terminal ancha: la dirección completa y el error real sobreviven
        let wide = render(200);
        assert!(
            wide.contains("0xf8E3128CDE1234567890123456789012345678"),
            "la dirección se pierde:\n{wide}"
        );
        assert!(wide.contains("error sending request"), "{wide}");
        // terminal estrecha: se recorta, pero el conteo sigue visible
        let narrow = render(80);
        assert!(narrow.contains("whales: 27/100"), "{narrow}");
    }
}
