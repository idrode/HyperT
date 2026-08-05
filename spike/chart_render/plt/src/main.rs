//! Candidato B del spike: LinePlot de ratatui-plt 0.0.2.
//!
//! Importante (leído del código fuente del crate): el widget en vivo dibuja la
//! línea con Bresenham sobre caracteres braille — es render de celdas, NO
//! píxeles. Su soporte "kitty" es un export offline (`buffer_to_kitty`) que
//! pinta cada celda como un rectángulo de color sólido (ni siquiera rasteriza
//! los glifos). Con `--export` generamos ambos artefactos para verlo.

use anyhow::{anyhow, Result};
use crossterm::event::{self, Event, KeyCode};
use ratatui_plt::prelude::*;
use std::time::Duration;

fn build_plot(rsi: &[f64], ma: &[f64]) -> LinePlot {
    let rsi_s = Series::new("RSI(14)")
        .data(rsi.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect())
        .color(Color::Rgb(247, 193, 80));
    let ma_s = Series::new("MA(14)")
        .data(ma.iter().enumerate().map(|(i, v)| (i as f64, *v)).collect())
        .color(Color::Rgb(86, 182, 194));
    LinePlot::new()
        .series(rsi_s)
        .series(ma_s)
        .title("RSI(14) — ratatui-plt LinePlot  (q: salir)")
        .x_axis(Axis::new().label("vela"))
        .y_axis(Axis::new().label("RSI").bounds(Bounds::Manual(0.0, 100.0)))
        .reference_lines(vec![
            ReferenceLine::hline(70.0, Color::Rgb(96, 106, 120)),
            ReferenceLine::hline(50.0, Color::Rgb(58, 66, 80)),
            ReferenceLine::hline(30.0, Color::Rgb(96, 106, 120)),
        ])
        .theme(Theme::dark())
        .show_legend(true)
}

fn main() -> Result<()> {
    let (rsi, ma) = rsi_data::demo_rsi_ma();
    let plot = build_plot(&rsi, &ma);

    // export offline: el único camino "kitty" que ofrece este crate
    if std::env::args().any(|a| a == "--export") {
        let buf = render_to_buffer(&plot, 120, 32);
        let opts = ExportOptions::new().cell_width(10).cell_height(20);
        let png = buffer_to_png(&buf, &opts).map_err(|e| anyhow!("{e:?}"))?;
        let kitty = buffer_to_kitty(&buf, &opts).map_err(|e| anyhow!("{e:?}"))?;
        std::fs::write("plt_export.png", png)?;
        std::fs::write("plt_export.kitty", kitty)?;
        println!("export ok: plt_export.png / plt_export.kitty");
        return Ok(());
    }

    let mut terminal = ratatui::init();
    loop {
        terminal.draw(|f| f.render_widget(&plot, f.area()))?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
            }
        }
    }
    ratatui::restore();
    Ok(())
}
