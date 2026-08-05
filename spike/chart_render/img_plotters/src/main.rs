//! Candidato A del spike: plotters rasteriza el RSI a una imagen real
//! (con supersampling 2x como antialiasing) y ratatui-image la muestra
//! vía protocolo Kitty, con fallback automático a halfblocks.
//!
//! Env vars para conducirlo desde el driver pty (sin tty interactivo):
//!   CHART_PROTO=query|kitty|halfblocks   (default: query al terminal)
//!   CHART_FONTSIZE=WxH                   (celda en px si no hay query, default 10x20)
//!   CHART_PNG_OUT=ruta.png               (guarda el raster exacto que se transmite)

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use plotters::prelude::*;
use ratatui::layout::Size;
use ratatui::widgets::{Block, Borders};
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Image, Resize};
use std::time::Duration;

/// Factor de supersampling: se rasteriza a SSx y se reduce con Lanczos,
/// que es lo que da el borde suavizado tipo TradingView.
const SS: u32 = 2;

fn forced_fontsize() -> FontSize {
    let s = std::env::var("CHART_FONTSIZE").unwrap_or_default();
    let (w, h) = s.split_once('x').unwrap_or(("10", "20"));
    FontSize::new(w.parse().unwrap_or(10), h.parse().unwrap_or(20))
}

#[allow(deprecated)] // from_fontsize: única vía de forzar protocolo sin query al tty (modo pty)
fn make_picker() -> Picker {
    let forced = |pt: ProtocolType| {
        let mut p = Picker::from_fontsize(forced_fontsize());
        p.set_protocol_type(pt);
        p
    };
    match std::env::var("CHART_PROTO").as_deref() {
        Ok("kitty") => forced(ProtocolType::Kitty),
        Ok("halfblocks") => forced(ProtocolType::Halfblocks),
        _ => Picker::from_query_stdio().unwrap_or_else(|_| forced(ProtocolType::Kitty)),
    }
}

fn main() -> Result<()> {
    let (rsi, ma) = rsi_data::demo_rsi_ma();
    // la query al terminal debe ir ANTES de entrar en raw mode / alt screen
    let picker = make_picker();
    let png_out = std::env::var("CHART_PNG_OUT").ok();

    let mut terminal = ratatui::init();
    let mut cached: Option<(Size, Protocol)> = None;
    let mut png_written = false;

    loop {
        terminal.draw(|f| {
            let block = Block::default().borders(Borders::ALL).title(format!(
                " RSI(14) — ratatui-image + plotters [{:?}]  (q: salir) ",
                picker.protocol_type()
            ));
            let inner = block.inner(f.area());
            f.render_widget(block, f.area());
            if inner.width < 4 || inner.height < 4 {
                return;
            }
            let want = Size::new(inner.width, inner.height);
            if cached.as_ref().map(|(s, _)| *s != want).unwrap_or(true) {
                let fs = picker.font_size();
                let px_w = inner.width as u32 * fs.width as u32;
                let px_h = inner.height as u32 * fs.height as u32;
                let big = draw_chart(px_w * SS, px_h * SS, &rsi, &ma);
                let img = image::imageops::resize(&big, px_w, px_h, FilterType::Lanczos3);
                if let (Some(path), false) = (&png_out, png_written) {
                    let _ = img.save(path);
                    png_written = true;
                }
                match picker.new_protocol(DynamicImage::ImageRgb8(img), want, Resize::Fit(None)) {
                    Ok(p) => cached = Some((want, p)),
                    Err(_) => return,
                }
            }
            if let Some((_, proto)) = &cached {
                f.render_widget(Image::new(proto), inner);
            }
        })?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(k)
                    if k.code == KeyCode::Char('q')
                        || (k.code == KeyCode::Char('c')
                            && k.modifiers.contains(KeyModifiers::CONTROL)) =>
                {
                    break;
                }
                Event::Resize(_, _) => cached = None,
                _ => {}
            }
        }
    }
    ratatui::restore();
    Ok(())
}

/// Dibuja el oscilador a resolución de píxel: fondo oscuro, banda 30–70
/// sombreada, niveles 30/50/70 discontinuos, RSI en ámbar y su MA en cian.
fn draw_chart(px_w: u32, px_h: u32, rsi: &[f64], ma: &[f64]) -> RgbImage {
    let n = rsi.len() as f64;
    let mut buf = vec![0u8; (px_w * px_h * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (px_w, px_h)).into_drawing_area();
        root.fill(&RGBColor(13, 17, 23)).unwrap();

        // con raster diminuto (fallback halfblocks) no caben márgenes ni texto
        let roomy = px_w >= 320 * SS && px_h >= 160 * SS;
        let scale = if roomy { px_h as f64 / (28.0 * 20.0 * SS as f64) } else { 0.0 };

        let mut cb = ChartBuilder::on(&root);
        if roomy {
            cb.margin((6 * SS) as i32)
                .y_label_area_size((30.0 * SS as f64 * scale.max(0.6)) as u32);
        } else {
            cb.margin(1).y_label_area_size(0);
        }
        let mut chart = cb.build_cartesian_2d(0f64..(n - 1.0), 0f64..100f64).unwrap();

        // banda 30–70 (misma convención visual que TradingView)
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(0.0, 30.0), (n - 1.0, 70.0)],
                RGBColor(23, 29, 41).filled(),
            )))
            .unwrap();

        // niveles de referencia discontinuos
        for (y, col) in [
            (70.0, RGBColor(96, 106, 120)),
            (50.0, RGBColor(58, 66, 80)),
            (30.0, RGBColor(96, 106, 120)),
        ] {
            chart
                .draw_series(DashedLineSeries::new(
                    [(0.0, y), (n - 1.0, y)],
                    (4 * SS) as u32,
                    (4 * SS) as u32,
                    col.into(),
                ))
                .unwrap();
        }

        // MA fina debajo, RSI grueso encima
        chart
            .draw_series(LineSeries::new(
                ma.iter().enumerate().map(|(i, v)| (i as f64, *v)),
                RGBColor(86, 182, 194).stroke_width(SS),
            ))
            .unwrap();
        chart
            .draw_series(LineSeries::new(
                rsi.iter().enumerate().map(|(i, v)| (i as f64, *v)),
                RGBColor(247, 193, 80).stroke_width(2 * SS),
            ))
            .unwrap();

        // etiquetas 30/50/70 (si falla la fuente del sistema, simplemente no salen)
        if roomy {
            let style = TextStyle::from(("sans-serif", 13.0 * SS as f64 * scale.max(0.6)))
                .color(&RGBColor(139, 148, 158));
            for y in [30.0, 50.0, 70.0] {
                if let Some((bx, by)) = Some(chart.backend_coord(&(0.0, y))) {
                    let _ = root.draw_text(
                        &format!("{y:.0}"),
                        &style,
                        (bx - (24 * SS as i32), by - (7 * SS as i32)),
                    );
                }
            }
        }
        root.present().unwrap();
    }
    RgbImage::from_raw(px_w, px_h, buf).expect("buffer RGB consistente")
}
