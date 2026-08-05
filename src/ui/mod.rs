mod exec;
mod flow;
pub mod fmt;
mod fondos;
mod heatmap;
mod help;
mod liq;
pub(crate) mod oscimg;
mod pair;
mod ranking;
mod search;
mod taplot;
mod wallet;
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
    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(46)]).split(area);
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::new().fg(Color::DarkGray))),
        cols[0],
    );
    if let Some(err) = &app.last_err {
        let mut msg: String = err.chars().take(44).collect();
        if err.chars().count() > 44 {
            msg.push('…');
        }
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::new().fg(Color::Red)))
                .alignment(Alignment::Right),
            cols[1],
        );
    }
}
