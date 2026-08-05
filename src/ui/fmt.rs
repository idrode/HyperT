use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::Color;

/// Hora local del cierre de una vela (dd/mm HH:MM), para los hovers.
pub fn time_label(t_ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(t_ms as i64) {
        chrono::LocalResult::Single(dt) => dt.format("%d/%m %H:%M").to_string(),
        _ => "—".to_string(),
    }
}

/// Fecha y hora local con año (dd/mm/yy HH:MM), para fechas de operaciones
/// que pueden ser de hace meses (historial de fills, Vista 9).
pub fn datetime_label(t_ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(t_ms as i64) {
        chrono::LocalResult::Single(dt) => dt.format("%d/%m/%y %H:%M").to_string(),
        _ => "—".to_string(),
    }
}

/// Antigüedad del cierre de una vela respecto a ahora.
pub fn age_label(t_close_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if t_close_ms > now {
        return "en curso".to_string();
    }
    let s = (now - t_close_ms) / 1000;
    if s < 3600 {
        format!("cerró hace {}m", s / 60)
    } else if s < 48 * 3600 {
        format!("cerró hace {}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("cerró hace {}d", s / 86_400)
    }
}

/// Precio con decimales adaptativos según magnitud.
pub fn fmt_px(v: f64) -> String {
    if v <= 0.0 {
        return "—".to_string();
    }
    let a = v.abs();
    let dec = if a >= 100_000.0 {
        0
    } else if a >= 1_000.0 {
        1
    } else if a >= 100.0 {
        2
    } else if a >= 1.0 {
        3
    } else if a >= 0.01 {
        5
    } else {
        7
    };
    format!("{v:.dec$}")
}

/// Notional compacto: 1.23B / 45.6M / 789K.
pub fn fmt_usd(v: f64) -> String {
    let a = v.abs();
    if a >= 1e9 {
        format!("{:.2}B", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e3 {
        format!("{:.0}K", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

pub fn fmt_opt_pct(v: Option<f64>, dec: usize) -> String {
    match v {
        Some(x) => format!("{x:+.dec$}%"),
        None => "—".to_string(),
    }
}

pub fn fmt_opt(v: Option<f64>, dec: usize) -> String {
    match v {
        Some(x) => format!("{x:+.dec$}"),
        None => "—".to_string(),
    }
}

/// Color por signo. `invert` para métricas donde positivo es "caliente"
/// (p. ej. funding positivo = longs pagan = rojo).
pub fn sign_color(v: Option<f64>, invert: bool) -> Color {
    match v {
        None => Color::DarkGray,
        Some(0.0) => Color::Gray,
        Some(x) => {
            let good = if invert { x < 0.0 } else { x > 0.0 };
            if good {
                Color::Green
            } else {
                Color::Red
            }
        }
    }
}
