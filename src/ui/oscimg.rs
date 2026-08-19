//! Oscilador → imagen real compartido por los paneles de indicadores (sub-panel
//! de la Vista 2 y panel whales+RSI de la Vista 3): plotters rasteriza las
//! series a píxeles (supersampling 2x + reducción Lanczos = antialiasing real)
//! y ratatui-image las muestra vía protocolo gráfico Kitty, con fallback
//! automático a halfblocks en terminales sin gráficos. Ganador del
//! spike/chart_render frente a ratatui-plt (ver CLAUDE.md).
//!
//! La imagen solo se re-rasteriza y retransmite cuando cambian los datos
//! (`fetched` de las velas, ≤1/min por EXTRA_TTL) o el tamaño del panel —
//! nunca por tick de render ni por movimiento del ratón: el hover vive en el
//! borde del Block (texto), fuera de la imagen.
//!
//! Env vars (mismas que el driver pty de spike/chart_render/tools):
//!   CHART_PROTO=kitty|halfblocks   fuerza protocolo sin query al tty
//!   CHART_FONTSIZE=WxH             celda en px cuando no hay query (def. 10x20)

use std::time::Instant;

use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use plotters::coord::types::RangedCoordf64;
use plotters::prelude::*;
use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FontSize, Image, Resize};

/// Factor de supersampling: se rasteriza a SSx y se reduce con Lanczos.
const SS: u32 = 2;

// Paleta del raster: fondo/banda del spike; series en los tonos que ya usa la
// paleta ANSI del TUI (verde/rojo/amarillo/azul/magenta/gris de siempre).
const BG: RGBColor = RGBColor(13, 17, 23);
const BAND: RGBColor = RGBColor(23, 29, 41);
const LEVEL_LO: RGBColor = RGBColor(52, 112, 70);
const LEVEL_MID: RGBColor = RGBColor(66, 72, 84);
const LEVEL_HI: RGBColor = RGBColor(132, 58, 66);
pub(super) const GREEN: RGBColor = RGBColor(63, 185, 106);
pub(super) const RED: RGBColor = RGBColor(229, 83, 90);
pub(super) const YELLOW: RGBColor = RGBColor(229, 192, 90);
pub(super) const BLUE: RGBColor = RGBColor(88, 166, 255);
pub(super) const MAGENTA: RGBColor = RGBColor(198, 120, 221);
pub(super) const GRAY: RGBColor = RGBColor(139, 148, 158);
pub(super) const DIM_GREEN: RGBColor = RGBColor(52, 112, 70);
pub(super) const BAR_BUY: RGBColor = RGBColor(0, 110, 60);
pub(super) const BAR_SELL: RGBColor = RGBColor(150, 35, 45);
const MARK_BUY: RGBColor = RGBColor(86, 211, 128);
const MARK_SELL: RGBColor = RGBColor(248, 106, 113);

/// Protocolo gráfico + caché de imagen por panel. Vive en `App` (como los
/// `TableState`): estado de UI que persiste entre frames.
pub struct Gfx {
    picker: Picker,
    pair_ta: Option<Cached>,
    whalersi: Option<Cached>,
    /// Delta por vela (Vista 2): se invalida por clave (trades en vivo), no por
    /// `stamp` de velas como los osciladores.
    pair_delta: Option<DeltaCached>,
    /// Frame de limpieza: pinta los paneles en blanco en vez de la imagen. Lo
    /// activa `Tui::run` durante un único frame al cerrarse un overlay, para
    /// que el diff repinte las celdas (los placeholders de Kitty las marcan
    /// como `Skip` y si no el resto del modal se queda pegado a la gráfica).
    pub blank_once: bool,
}

struct Cached {
    size: Size,
    stamp: Instant,
    proto: Protocol,
}

struct DeltaCached {
    size: Size,
    key: u64,
    proto: Protocol,
}

/// Caché destino: cada panel invalida el suyo sin pisar al otro.
#[derive(Clone, Copy)]
pub(super) enum OscSlot {
    PairTa,
    WhaleRsi,
}

impl Gfx {
    /// La detección de protocolo y tamaño de celda interroga al tty: llamar
    /// ANTES de entrar en raw mode / alternate screen (ver main).
    #[allow(deprecated)] // from_fontsize: única vía de forzar protocolo sin tty (driver pty)
    pub fn new() -> Self {
        let forced = |pt: ProtocolType| {
            let s = std::env::var("CHART_FONTSIZE").unwrap_or_default();
            let (w, h) = s.split_once('x').unwrap_or(("10", "20"));
            let mut p = Picker::from_fontsize(FontSize::new(
                w.parse().unwrap_or(10),
                h.parse().unwrap_or(20),
            ));
            p.set_protocol_type(pt);
            p
        };
        let picker = match std::env::var("CHART_PROTO").as_deref() {
            Ok("kitty") => forced(ProtocolType::Kitty),
            Ok("halfblocks") => forced(ProtocolType::Halfblocks),
            // sin query posible (tty mudo): halfblocks funciona en cualquier
            // terminal; forzar Kitty a ciegas pintaría escapes como basura
            _ => Picker::from_query_stdio().unwrap_or_else(|_| forced(ProtocolType::Halfblocks)),
        };
        Self {
            picker,
            pair_ta: None,
            whalersi: None,
            pair_delta: None,
            blank_once: false,
        }
    }
}

/// Color RGB del RSI por zona — gemelo raster de `whalersi::rsi_zone_color`
/// (aquel devuelve `ratatui::Color` para los textos; este alimenta la imagen).
pub(super) fn rsi_zone_rgb(v: f64, wp: &crate::signals::WhaleParams) -> RGBColor {
    if v >= wp.overbought {
        RED
    } else if v <= wp.oversold {
        GREEN
    } else {
        MAGENTA
    }
}

/// Serie a trazar, en orden de pintado (la última queda encima).
pub(super) struct OscLine<'a> {
    pub vals: &'a [f64],
    /// Grosor en px lógicos (1 = fino, 2 = destacado).
    pub width: u32,
    pub color: LineColor<'a>,
}

pub(super) enum LineColor<'a> {
    Fixed(RGBColor),
    /// Color por valor (zonas del RSI, como el rsiColor del Pine).
    ByValue(&'a dyn Fn(f64) -> RGBColor),
}

/// Contenido del panel: ventana visible de las series + extras de la Vista 3.
pub(super) struct OscSpec<'a> {
    /// Ventana visible: índices [start, start+len) de las series.
    pub start: usize,
    pub len: usize,
    /// Columnas de texto por punto. En la Vista 2 es CANDLE_CELLS (2 —
    /// vela+hueco) y el punto i cae centrado en el cuerpo de su vela, así el
    /// hover por columna de celda (taplot::hover_idx) mapea igual que con el
    /// dibujo manual anterior. En la Vista 3 es fraccionario: la ventana
    /// compartida con la Vista 2 se reparte por el ancho de su panel.
    pub cols_per_pt: f64,
    /// Medio ancho del punto en columnas: el punto i se centra en
    /// `i·cols_per_pt + half_cols` y las columnas/marcas ▲▼ abarcan
    /// ±half_cols (0.5 en Vista 2 = el cuerpo de la vela; cols_per_pt/2 en
    /// Vista 3 = su hueco escalado).
    pub half_cols: f64,
    pub oversold: f64,
    pub overbought: f64,
    pub lines: Vec<OscLine<'a>>,
    /// Columnas de intensidad ballena: (índice absoluto, altura 0-100, color).
    pub bars: Vec<(usize, f64, RGBColor)>,
    /// Marcas ▲▼: (índice absoluto, es compra). Compra abajo, venta arriba.
    pub marks: Vec<(usize, bool)>,
}

/// Rasteriza (solo si cambió `stamp` o el tamaño) y pinta el panel en `area`.
pub(super) fn draw_into(
    f: &mut Frame,
    area: Rect,
    gfx: &mut Gfx,
    slot: OscSlot,
    stamp: Instant,
    spec: OscSpec,
) {
    if gfx.blank_once {
        f.render_widget(ratatui::widgets::Clear, area);
        return;
    }
    let Gfx {
        picker,
        pair_ta,
        whalersi,
        ..
    } = gfx;
    let cache = match slot {
        OscSlot::PairTa => pair_ta,
        OscSlot::WhaleRsi => whalersi,
    };
    let size = Size::new(area.width, area.height);
    if !cache
        .as_ref()
        .is_some_and(|c| c.size == size && c.stamp == stamp)
    {
        let fs = picker.font_size();
        let (pw, ph) = (
            area.width as u32 * fs.width as u32,
            area.height as u32 * fs.height as u32,
        );
        if pw == 0 || ph == 0 {
            *cache = None;
            return;
        }
        let img = raster(pw, ph, fs.width as u32, &spec);
        match picker.new_protocol(DynamicImage::ImageRgb8(img), size, Resize::Fit(None)) {
            Ok(proto) => {
                *cache = Some(Cached { size, stamp, proto });
            }
            Err(_) => {
                *cache = None;
                return;
            }
        }
    }
    if let Some(c) = cache {
        f.render_widget(Image::new(&c.proto), area);
    }
}

/// Barra de delta por vela: una columna por vela visible, verde arriba si
/// domina la compra agresora, roja abajo si domina la venta. Mismo pipeline
/// raster (plotters vía Kitty, fallback halfblocks) que los osciladores.
pub(super) struct DeltaSpec<'a> {
    /// Delta neto (compra − venta, USD) por vela visible; `None` = sin datos
    /// (warmup), no se pinta barra. Índice i = vela i de la ventana.
    pub vals: &'a [Option<f64>],
    /// Columnas de celda por vela (CANDLE_CELLS) — mismo eje X que las velas.
    pub cols_per_pt: f64,
    /// Medio ancho de la barra en columnas (0.5 = el cuerpo de la vela).
    pub half_cols: f64,
}

/// Rasteriza (solo si cambió `key` o el tamaño) y pinta la barra de delta.
pub(super) fn draw_delta_into(f: &mut Frame, area: Rect, gfx: &mut Gfx, key: u64, spec: DeltaSpec) {
    if gfx.blank_once {
        f.render_widget(ratatui::widgets::Clear, area);
        return;
    }
    let Gfx {
        picker, pair_delta, ..
    } = gfx;
    let size = Size::new(area.width, area.height);
    if !pair_delta
        .as_ref()
        .is_some_and(|c| c.size == size && c.key == key)
    {
        let fs = picker.font_size();
        let (pw, ph) = (
            area.width as u32 * fs.width as u32,
            area.height as u32 * fs.height as u32,
        );
        if pw == 0 || ph == 0 {
            *pair_delta = None;
            return;
        }
        let img = raster_delta(pw, ph, fs.width as u32, &spec);
        match picker.new_protocol(DynamicImage::ImageRgb8(img), size, Resize::Fit(None)) {
            Ok(proto) => *pair_delta = Some(DeltaCached { size, key, proto }),
            Err(_) => {
                *pair_delta = None;
                return;
            }
        }
    }
    if let Some(c) = pair_delta {
        f.render_widget(Image::new(&c.proto), area);
    }
}

/// Barras centradas en la línea cero: altura proporcional al delta neto de la
/// vela, escalada al mayor delta absoluto visible.
fn raster_delta(pw: u32, ph: u32, cell_w: u32, spec: &DeltaSpec) -> RgbImage {
    let (bw, bh) = (pw * SS, ph * SS);
    let mut buf = vec![0u8; (bw * bh * 3) as usize];
    {
        let root = BitMapBackend::with_buffer(&mut buf, (bw, bh)).into_drawing_area();
        root.fill(&BG).unwrap();
        let w = pw as f64;
        // eje Y simétrico -1..1 (delta normalizado); cero en el centro
        let mut chart = ChartBuilder::on(&root)
            .margin(0)
            .build_cartesian_2d(0f64..w, -1f64..1f64)
            .unwrap();
        let x_of = |i: usize| (i as f64 * spec.cols_per_pt + spec.half_cols) * cell_w as f64;
        let half = spec.half_cols * cell_w as f64;

        // línea cero de referencia
        chart
            .draw_series(std::iter::once(PathElement::new(
                [(0.0, 0.0), (w, 0.0)],
                LEVEL_MID.stroke_width(SS),
            )))
            .unwrap();

        let max = spec
            .vals
            .iter()
            .flatten()
            .fold(0.0_f64, |m, v| m.max(v.abs()))
            .max(1e-9);
        for (i, v) in spec.vals.iter().enumerate() {
            let Some(v) = v else { continue };
            if *v == 0.0 {
                continue;
            }
            let x = x_of(i);
            let h = (v / max).clamp(-1.0, 1.0);
            let col = if *v >= 0.0 { BAR_BUY } else { BAR_SELL };
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(x - half, 0.0), (x + half, h)],
                    col.filled(),
                )))
                .unwrap();
        }
        root.present().unwrap();
    }
    let big = RgbImage::from_raw(bw, bh, buf).expect("buffer RGB consistente");
    image::imageops::resize(&big, pw, ph, FilterType::Lanczos3)
}

type OscChart<'a, 'b> =
    ChartContext<'a, BitMapBackend<'b>, Cartesian2d<RangedCoordf64, RangedCoordf64>>;

/// Píxeles lógicos `pw`×`ph` (celdas × fuente); el backend trabaja a SSx.
/// El eje X está en px lógicos para que cada punto caiga exactamente en las
/// columnas de celda de su vela; el eje Y es la escala 0-100 del oscilador.
fn raster(pw: u32, ph: u32, cell_w: u32, spec: &OscSpec) -> RgbImage {
    let (bw, bh) = (pw * SS, ph * SS);
    let mut buf = vec![0u8; (bw * bh * 3) as usize];
    {
        // backend en memoria: los draw solo fallan por E/S, aquí imposible
        let root = BitMapBackend::with_buffer(&mut buf, (bw, bh)).into_drawing_area();
        root.fill(&BG).unwrap();
        let w = pw as f64;
        let mut chart = ChartBuilder::on(&root)
            .margin(0)
            .build_cartesian_2d(0f64..w, 0f64..100f64)
            .unwrap();
        let x_of = |i: usize| (i as f64 * spec.cols_per_pt + spec.half_cols) * cell_w as f64;

        // banda sobreventa–sobrecompra + niveles discontinuos, como TradingView
        chart
            .draw_series(std::iter::once(Rectangle::new(
                [(0.0, spec.oversold), (w, spec.overbought)],
                BAND.filled(),
            )))
            .unwrap();
        for (lvl, col) in [
            (spec.oversold, LEVEL_LO),
            (50.0, LEVEL_MID),
            (spec.overbought, LEVEL_HI),
        ] {
            chart
                .draw_series(DashedLineSeries::new(
                    [(0.0, lvl), (w, lvl)],
                    4 * SS,
                    4 * SS,
                    col.stroke_width(SS),
                ))
                .unwrap();
        }

        // columnas de intensidad ballena, debajo de las líneas
        let visible = |idx: usize| idx >= spec.start && idx - spec.start < spec.len;
        let half = spec.half_cols * cell_w as f64;
        for &(idx, h, col) in spec.bars.iter().filter(|(i, ..)| visible(*i)) {
            let x = x_of(idx - spec.start);
            chart
                .draw_series(std::iter::once(Rectangle::new(
                    [(x - half, 0.0), (x + half, h)],
                    col.filled(),
                )))
                .unwrap();
        }

        for line in &spec.lines {
            draw_line(&mut chart, spec, line, &x_of);
        }

        // marcas ▲ (compra, abajo) / ▼ (venta, arriba), encima del trazo
        for &(idx, buy) in spec.marks.iter().filter(|(i, _)| visible(*i)) {
            let x = x_of(idx - spec.start);
            let (pts, col) = if buy {
                (vec![(x, 8.0), (x - half, 2.0), (x + half, 2.0)], MARK_BUY)
            } else {
                (
                    vec![(x, 92.0), (x - half, 98.0), (x + half, 98.0)],
                    MARK_SELL,
                )
            };
            chart
                .draw_series(std::iter::once(Polygon::new(pts, col.filled())))
                .unwrap();
        }
        root.present().unwrap();
    }
    let big = RgbImage::from_raw(bw, bh, buf).expect("buffer RGB consistente");
    image::imageops::resize(&big, pw, ph, FilterType::Lanczos3)
}

/// Traza una serie como polilínea segmentada: NaN corta el trazo (warmup) y
/// un cambio de color de zona cierra el tramo y abre otro, como el dibujo
/// manual anterior. Un punto aislado entre NaN queda como punto suelto.
fn draw_line(chart: &mut OscChart, spec: &OscSpec, line: &OscLine, x_of: &dyn Fn(usize) -> f64) {
    let end = (spec.start + spec.len).min(line.vals.len());
    let color_at = |v: f64| match line.color {
        LineColor::Fixed(c) => c,
        LineColor::ByValue(f) => f(v),
    };
    let stroke = line.width * SS;
    let mut run: Vec<(f64, f64)> = Vec::new();
    let mut run_color = BG;
    let flush = |run: &mut Vec<(f64, f64)>, col: RGBColor, chart: &mut OscChart| {
        match run.len() {
            0 => {}
            1 => {
                chart
                    .draw_series(std::iter::once(Circle::new(
                        run[0],
                        stroke as i32,
                        col.filled(),
                    )))
                    .unwrap();
            }
            _ => {
                chart
                    .draw_series(LineSeries::new(
                        run.iter().copied(),
                        col.stroke_width(stroke),
                    ))
                    .unwrap();
            }
        }
        run.clear();
    };
    for i in spec.start..end {
        let v = line.vals[i];
        if !v.is_finite() {
            flush(&mut run, run_color, chart);
            continue;
        }
        let pt = (x_of(i - spec.start), v);
        let col = color_at(v);
        if col != run_color && !run.is_empty() {
            // el tramo nuevo arranca en el último punto del anterior
            let seed = *run.last().unwrap();
            flush(&mut run, run_color, chart);
            run.push(seed);
        }
        run_color = col;
        run.push(pt);
    }
    flush(&mut run, run_color, chart);
}
