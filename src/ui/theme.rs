//! Tema visual derivado del verde de marca de Hyperliquid (`#97FCE4`), con
//! dos variantes completas (oscura y clara) — piloto aplicado SOLO a la
//! Vista 1 (Ranking). Mismo patrón que `src/i18n.rs`: un único sitio donde
//! vive el estilo, para no volver a tener colores sueltos por las vistas.
//!
//! ## Cómo tocar los colores
//! Todo lo que se ve en la Vista 1 sale de la paleta activa ([`c()`]).
//! Cambia el valor aquí y afecta a la vista entera; no hay ningún color
//! escrito a mano en `src/ui/ranking.rs`.
//!
//! | Qué controla                     | Campo de [`Palette`] |
//! |----------------------------------|----------------------|
//! | Fondo del panel                  | `bg`                 |
//! | Texto general                    | `fg`                 |
//! | Texto realzado (nombre del par)  | `fg_strong`          |
//! | Texto apagado (índice)           | `muted`              |
//! | Bordes del panel                 | `border`             |
//! | Título del panel                 | `title`              |
//! | Fondo de la fila seleccionada    | `selection_bg`       |
//! | Cursor / marcador de selección   | `cursor`             |
//! | Positivo (ganancia, favorable)   | `positive`           |
//! | Negativo (pérdida, desfavorable) | `negative`           |
//! | Valor exactamente cero           | `neutral`            |
//! | Sin dato                         | `no_data`            |
//! | Columna de orden activa          | `header_active`      |
//! | Resto de cabeceras               | `header_idle`        |
//!
//! El GROSOR/forma del borde se elige con [`BORDER_LOOK`] (ver [`BorderLook`]).
//!
//! ## De dónde sale cada tono
//! El ancla es el verde real del logo de Hyperliquid (`#97FCE4`, confirmado
//! por el usuario). En el tema OSCURO se usa tal cual como acento POSITIVO
//! (es un tono claro y poco saturado: se lee bien sobre fondo oscuro). En el
//! CLARO, el mismo matiz (~165°) se oscurece y satura a `#0F9C7C`, porque el
//! original es ilegible sobre fondo blanco — misma identidad de color,
//! adaptada al fondo, no un verde distinto. El NEGATIVO es el complementario
//! del verde de marca (matiz ~345°, familia coral/rojo), calculado, no
//! elegido a ojo.
//!
//! Nota de precisión: los acentos están DERIVADOS del ancla, no verificados
//! pixel a pixel contra `app.hyperliquid.xyz`.
//!
//! ## Qué NO toca este módulo
//! Las otras 8 vistas siguen con los colores de antes (`fmt::sign_color` y
//! `ranking::regime_color` se dejan intactos a propósito: los comparten casi
//! todas las vistas, y cambiarlos habría repintado el TUI entero). Tampoco
//! toca los gráficos rasterizados por plotters (`src/ui/oscimg.rs`), que
//! llevan su propia paleta.

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::widgets::BorderType;

// ── variantes de tema ─────────────────────────────────────────────────────

/// Variante de tema. Mismo patrón que `i18n::Lang`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
    Dark,
    Light,
}

// 0 = Dark (por defecto), 1 = Light. Global porque los `draw` de las vistas
// no llevan el tema como parámetro y no tiene sentido enhebrarlo por todos.
static THEME: AtomicU8 = AtomicU8::new(0);

pub fn set_theme(t: Theme) {
    THEME.store(t as u8, Ordering::Relaxed);
}

pub fn theme() -> Theme {
    if THEME.load(Ordering::Relaxed) == 1 {
        Theme::Light
    } else {
        Theme::Dark
    }
}

/// Alterna oscuro ↔ claro (tecla `T`).
pub fn toggle_theme() {
    let next = if theme() == Theme::Dark {
        Theme::Light
    } else {
        Theme::Dark
    };
    set_theme(next);
}

/// `--theme=dark` / `--theme=light`. None si no reconoce.
#[allow(dead_code)]
pub fn parse_theme(s: &str) -> Option<Theme> {
    match s.trim().to_ascii_lowercase().as_str() {
        "dark" | "oscuro" => Some(Theme::Dark),
        "light" | "claro" => Some(Theme::Light),
        _ => None,
    }
}

/// Colores activos según el tema actual. Uso: `theme::c().fg`.
/// Equivalente a `i18n::t()`.
pub fn c() -> &'static Palette {
    match theme() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

// ── la paleta ─────────────────────────────────────────────────────────────

/// Los colores de una variante. Nombres semánticos, nunca ANSI: el sitio de
/// uso pide "positivo", no "verde".
pub struct Palette {
    /// Fondo de la vista.
    pub bg: Color,
    /// Texto general.
    pub fg: Color,
    /// Texto realzado (nombre del par).
    pub fg_strong: Color,
    /// Texto apagado (índice de fila).
    pub muted: Color,
    /// Borde del panel.
    pub border: Color,
    /// Título del panel.
    pub title: Color,
    /// Fondo de la fila seleccionada.
    pub selection_bg: Color,
    /// Cursor / marcador de fila.
    pub cursor: Color,
    /// Positivo / ganancia / condición favorable (el verde de marca).
    pub positive: Color,
    /// Negativo / pérdida / condición desfavorable (su complementario).
    pub negative: Color,
    /// Cero exacto: ni bueno ni malo, pero es un dato real.
    pub neutral: Color,
    /// Sin dato. Deliberadamente distinto de `neutral`: un cero real y un
    /// hueco no se pintan igual (honestidad de datos del proyecto).
    pub no_data: Color,
    /// Cabecera de la columna por la que se está ordenando.
    pub header_active: Color,
    /// Cabeceras del resto de columnas.
    pub header_idle: Color,
    /// Tono azulado, fuera de la escala positivo/negativo (cierre de cortos)
    /// y serie %B de la Vista 3.
    pub accent_blue: Color,
    /// Cian: TRIX.
    pub accent_cyan: Color,
    /// Tono coral, fuera de la escala positivo/negativo (cierre de largos).
    pub accent_coral: Color,

    // ── Vistas 2 y 3 (velas, ejes, series de indicadores) ────────────────
    /// Ámbar: media móvil del RSI, sparkline de OI, curva de funding.
    pub accent_amber: Color,
    /// Violeta: RSI en zona intermedia (ni sobrecompra ni sobreventa).
    pub accent_magenta: Color,
    /// Vela alcista / vela bajista. Misma familia que positivo/negativo.
    pub candle_up: Color,
    pub candle_down: Color,
    /// Vela bajo el cursor: la misma, un punto más clara.
    pub candle_up_hi: Color,
    pub candle_down_hi: Color,
    /// Rejilla horizontal del gráfico de velas.
    pub grid: Color,
    /// Línea discontinua del último cierre.
    pub crosshair: Color,
    /// Etiquetas de los niveles de sobreventa/sobrecompra del eje 0-100:
    /// misma familia que positivo/negativo, apagadas para no competir con
    /// las series.
    pub positive_dim: Color,
    pub negative_dim: Color,
}

/// Tema oscuro: el verde de marca (`#97FCE4`) tal cual como positivo.
pub static DARK: Palette = Palette {
    bg: Color::Rgb(0x0B, 0x12, 0x10),
    fg: Color::Rgb(0xD8, 0xDE, 0xDA),
    fg_strong: Color::Rgb(0xF2, 0xF7, 0xF4),
    muted: Color::Rgb(0x5C, 0x6B, 0x65),
    border: Color::Rgb(0x2E, 0x3D, 0x38),
    title: Color::Rgb(0xE8, 0xC0, 0x7D),
    selection_bg: Color::Rgb(0x17, 0x23, 0x1F),
    cursor: Color::Rgb(0x97, 0xFC, 0xE4),
    positive: Color::Rgb(0x97, 0xFC, 0xE4),
    negative: Color::Rgb(0xF2, 0x63, 0x7A),
    neutral: Color::Rgb(0x9F, 0xB0, 0xAA),
    no_data: Color::Rgb(0x4A, 0x56, 0x52),
    header_active: Color::Rgb(0x97, 0xFC, 0xE4),
    header_idle: Color::Rgb(0x6B, 0x7A, 0x75),
    accent_blue: Color::Rgb(0x6F, 0x8C, 0xFF),
    accent_cyan: Color::Rgb(0x8A, 0xD8, 0xFF),
    accent_coral: Color::Rgb(0xE8, 0x9A, 0x8A),
    accent_amber: Color::Rgb(0xE8, 0xC0, 0x7D),
    accent_magenta: Color::Rgb(0xC3, 0x9B, 0xE8),
    candle_up: Color::Rgb(0x97, 0xFC, 0xE4),
    candle_down: Color::Rgb(0xF2, 0x63, 0x7A),
    candle_up_hi: Color::Rgb(0xC9, 0xFF, 0xF0),
    candle_down_hi: Color::Rgb(0xFF, 0x9F, 0xAE),
    grid: Color::Rgb(0x1E, 0x2A, 0x26),
    crosshair: Color::Rgb(0x7A, 0x6B, 0x45),
    positive_dim: Color::Rgb(0x3E, 0x7F, 0x6E),
    negative_dim: Color::Rgb(0x8A, 0x44, 0x50),
};

/// Tema claro: el mismo matiz de verde, oscurecido para que se lea sobre
/// fondo claro.
pub static LIGHT: Palette = Palette {
    bg: Color::Rgb(0xF5, 0xFA, 0xF8),
    fg: Color::Rgb(0x16, 0x21, 0x1D),
    fg_strong: Color::Rgb(0x0A, 0x10, 0x0D),
    muted: Color::Rgb(0x6B, 0x7A, 0x75),
    border: Color::Rgb(0xB8, 0xCA, 0xC4),
    title: Color::Rgb(0xB8, 0x84, 0x3A),
    selection_bg: Color::Rgb(0xDC, 0xF5, 0xEE),
    cursor: Color::Rgb(0x0F, 0x9C, 0x7C),
    positive: Color::Rgb(0x0F, 0x9C, 0x7C),
    negative: Color::Rgb(0xC2, 0x3B, 0x52),
    neutral: Color::Rgb(0x5A, 0x6B, 0x65),
    no_data: Color::Rgb(0xA8, 0xB5, 0xB0),
    header_active: Color::Rgb(0x0F, 0x9C, 0x7C),
    header_idle: Color::Rgb(0x8A, 0x97, 0x91),
    accent_blue: Color::Rgb(0x3A, 0x54, 0xC8),
    accent_cyan: Color::Rgb(0x1B, 0x7F, 0xA8),
    accent_coral: Color::Rgb(0xA8, 0x5A, 0x46),
    accent_amber: Color::Rgb(0xB8, 0x84, 0x3A),
    accent_magenta: Color::Rgb(0x7A, 0x4F, 0xB0),
    candle_up: Color::Rgb(0x0F, 0x9C, 0x7C),
    candle_down: Color::Rgb(0xC2, 0x3B, 0x52),
    candle_up_hi: Color::Rgb(0x0B, 0xBF, 0x95),
    candle_down_hi: Color::Rgb(0xE1, 0x4A, 0x63),
    grid: Color::Rgb(0xDC, 0xE7, 0xE3),
    crosshair: Color::Rgb(0xC9, 0xB4, 0x8A),
    positive_dim: Color::Rgb(0x4E, 0x9E, 0x88),
    negative_dim: Color::Rgb(0xB3, 0x70, 0x7E),
};

// ── paleta del raster (plotters / oscimg) ─────────────────────────────────
// Los paneles de RSI/ADX/DMI/TRIX y la barra de delta no son texto: se
// rasterizan con plotters. Sus tonos viven aquí igual que los de ratatui,
// para que el toggle de tema recolore TAMBIÉN las imágenes y no solo el
// texto de alrededor. Se guardan como RGB crudo para no meter plotters
// como dependencia de este módulo; `oscimg` los convierte.

/// Tonos del raster de los paneles de indicadores.
pub struct Raster {
    /// Fondo de la imagen: el mismo `bg` del tema, para que el panel no
    /// tenga un rectángulo de otro color dentro del marco.
    pub bg: [u8; 3],
    /// Banda sobreventa–sobrecompra.
    pub band: [u8; 3],
    /// Niveles discontinuos: sobreventa, 50, sobrecompra.
    pub level_lo: [u8; 3],
    pub level_mid: [u8; 3],
    pub level_hi: [u8; 3],
    /// Serie alcista/positiva (+DI): el verde de marca.
    pub green: [u8; 3],
    /// Serie bajista/negativa (−DI): el coral complementario.
    pub red: [u8; 3],
    /// Media móvil del RSI.
    pub yellow: [u8; 3],
    /// %B (RSI modificado) de la Vista 3.
    pub blue: [u8; 3],
    /// RSI en zona intermedia.
    pub magenta: [u8; 3],
    /// ADX (serie neutra, sin dirección).
    pub gray: [u8; 3],
    /// Bandas de Bollinger del RSI (Vista 3), apagadas.
    pub dim_green: [u8; 3],
    /// TRIX.
    pub cyan: [u8; 3],
    /// Columnas de delta/intensidad: compra agresora y venta agresora.
    pub bar_buy: [u8; 3],
    pub bar_sell: [u8; 3],
    /// Marcas ▲▼ de disparo de ballena.
    pub mark_buy: [u8; 3],
    pub mark_sell: [u8; 3],
}

/// Tonos del raster según el tema activo. Gemelo de [`c()`] para la capa de
/// imagen.
pub fn raster() -> &'static Raster {
    match theme() {
        Theme::Dark => &RASTER_DARK,
        Theme::Light => &RASTER_LIGHT,
    }
}

pub static RASTER_DARK: Raster = Raster {
    bg: [0x0B, 0x12, 0x10],
    band: [0x12, 0x20, 0x1C],
    level_lo: [0x2E, 0x5C, 0x50],
    level_mid: [0x33, 0x40, 0x3B],
    level_hi: [0x7A, 0x3A, 0x46],
    green: [0x97, 0xFC, 0xE4],
    red: [0xF2, 0x63, 0x7A],
    yellow: [0xE8, 0xC0, 0x7D],
    blue: [0x6F, 0x8C, 0xFF],
    magenta: [0xC3, 0x9B, 0xE8],
    gray: [0x9F, 0xB0, 0xAA],
    dim_green: [0x2E, 0x5C, 0x50],
    cyan: [0x8A, 0xD8, 0xFF],
    bar_buy: [0x12, 0x51, 0x3F],
    bar_sell: [0x5C, 0x1F, 0x2A],
    mark_buy: [0x97, 0xFC, 0xE4],
    mark_sell: [0xF2, 0x63, 0x7A],
};

pub static RASTER_LIGHT: Raster = Raster {
    bg: [0xF5, 0xFA, 0xF8],
    band: [0xE4, 0xF1, 0xEC],
    level_lo: [0x9C, 0xCF, 0xBE],
    level_mid: [0xC2, 0xCE, 0xC9],
    level_hi: [0xE0, 0xA9, 0xB2],
    green: [0x0F, 0x9C, 0x7C],
    red: [0xC2, 0x3B, 0x52],
    yellow: [0xB8, 0x84, 0x3A],
    blue: [0x3A, 0x54, 0xC8],
    magenta: [0x7A, 0x4F, 0xB0],
    gray: [0x5A, 0x6B, 0x65],
    dim_green: [0x7F, 0xBF, 0xA8],
    cyan: [0x1B, 0x7F, 0xA8],
    bar_buy: [0x7F, 0xD9, 0xBE],
    bar_sell: [0xE9, 0xA0, 0xAC],
    mark_buy: [0x0F, 0x9C, 0x7C],
    mark_sell: [0xC2, 0x3B, 0x52],
};

/// El tema es estado global: los tests que lo cambian se serializan con este
/// candado para no pisarse entre sí.
#[cfg(test)]
pub(super) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── gradientes (Vistas 4 y 5) ─────────────────────────────────────────────
// Los heatmaps necesitan más de dos tonos: pasos intermedios entre el fondo
// del tema y el acento semántico. En vez de una segunda paleta a ojo, se
// generan mezclando el acento con el fondo ACTIVO — así el gradiente entero
// (y no solo sus extremos) sigue al tema, y el extremo saturado sigue siendo
// exactamente el verde de marca / el coral complementario.

fn parts(c: Color) -> [f64; 3] {
    match c {
        Color::Rgb(r, g, b) => [r as f64, g as f64, b as f64],
        // la paleta es RGB por construcción (hay un test que lo fija)
        _ => [0.0, 0.0, 0.0],
    }
}

/// Mezcla `from`→`to` con factor `t` (0 = from, 1 = to).
fn mix(from: Color, to: Color, t: f64) -> Color {
    let (a, b) = (parts(from), parts(to));
    let t = t.clamp(0.0, 1.0);
    let ch = |i: usize| (a[i] + (b[i] - a[i]) * t).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(ch(0), ch(1), ch(2))
}

/// Hasta dónde puede llegar un FONDO de celda hacia el acento sin dejar de
/// ser legible con el texto del tema encima: en oscuro el acento es muy
/// claro (el verde de marca), así que se queda a media mezcla; en claro se
/// puede ir más lejos porque el texto es oscuro.
fn heat_ceiling() -> f64 {
    match theme() {
        Theme::Dark => 0.45,
        Theme::Light => 0.65,
    }
}

/// Fondo de celda del heatmap top-OI (Vista 4). `t` ∈ [-1, 1]: positivo tira
/// al verde de marca, negativo al coral; 0 es el fondo del tema.
pub fn heat_bg(t: f64) -> Color {
    let t = t.clamp(-1.0, 1.0);
    let p = c();
    let accent = if t >= 0.0 { p.positive } else { p.negative };
    mix(p.bg, accent, t.abs() * heat_ceiling())
}

/// Texto legible sobre [`heat_bg`]: el propio texto del tema, que es lo que
/// contrasta con el fondo en ambas variantes (claro sobre oscuro y viceversa).
pub fn heat_fg() -> Color {
    c().fg
}

/// Barra del heatmap de liquidaciones (Vista 5). `long` elige la familia
/// (combustible de longs = positivo/verde de marca; de shorts =
/// negativo/coral) y `frac` ∈ [0, 1] es el notional relativo del bucket: más
/// notional = más saturado, hasta el acento puro.
pub fn liq_bar(long: bool, frac: f64) -> Color {
    let p = c();
    let accent = if long { p.positive } else { p.negative };
    // arranca ya teñido (una barra corta debe leerse como su lado, no como
    // fondo) y sube hasta el acento puro
    mix(p.bg, accent, 0.40 + 0.60 * frac.clamp(0.0, 1.0))
}

/// Color de TEXTO por sesgo direccional con intensidad (Vistas 6 y 7): `t` ∈
/// [-1, 1], positivo = alcista (verde de marca), negativo = bajista (coral),
/// y |t| = cuánto de marcado está el sesgo. A diferencia de [`heat_bg`] mezcla
/// desde el gris neutro, no desde el fondo: un sesgo débil debe leerse como
/// "casi neutro", no como texto medio borrado.
pub fn bias_fg(t: f64) -> Color {
    let t = t.clamp(-1.0, 1.0);
    let p = c();
    let accent = if t >= 0.0 { p.positive } else { p.negative };
    mix(p.neutral, accent, 0.35 + 0.65 * t.abs())
}

/// Color de la sombra de los paneles flotantes (ver `super::shadow`).
/// Derivado del fondo ACTIVO: en oscuro se hunde hacia el negro, en claro se
/// oscurece lo justo para leerse como sombra y no como un agujero.
pub fn shadow_bg() -> Color {
    let p = c();
    let negro = Color::Rgb(0, 0, 0);
    match theme() {
        Theme::Dark => mix(p.bg, negro, 0.55),
        Theme::Light => mix(p.bg, negro, 0.30),
    }
}

// ── forma del borde ───────────────────────────────────────────────────────

/// Aspecto del marco. El usuario busca algo "cuadrado, de terminal vieja";
/// hay varias formas de conseguirlo y la elección es suya — cambia
/// [`BORDER_LOOK`] y relanza para comparar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// las variantes que no sean la activa quedan "sin construir" por definición:
// son justamente las alternativas entre las que se elige cambiando BORDER_LOOK
#[allow(dead_code)]
pub enum BorderLook {
    /// Línea gruesa (`┏━┓`). Lo más cercano a las referencias de multiplexor
    /// retro sin dejar de ser un marco de una sola celda.
    Thick,
    /// Doble línea (`╔═╗`), el look Midnight Commander clásico.
    Double,
    /// Marco de bloques sólidos (`▛▀▜`): el más "cuadrado" y macizo de los
    /// tres, ocupa la celda entera.
    Block,
    /// Línea fina de toda la vida (`┌─┐`), el de antes del tema.
    Plain,
}

/// Aspecto de borde activo. **Cámbialo aquí** para probar los otros.
pub const BORDER_LOOK: BorderLook = BorderLook::Thick;

/// Marco de bloques sólidos, para [`BorderLook::Block`]. No existe en
/// `ratatui::symbols::border`, así que se define aquí.
const BLOCK_SET: border::Set<'static> = border::Set {
    top_left: "▛",
    top_right: "▜",
    bottom_left: "▙",
    bottom_right: "▟",
    vertical_left: "▌",
    vertical_right: "▐",
    horizontal_top: "▀",
    horizontal_bottom: "▄",
};

/// Símbolos del marco según [`BORDER_LOOK`].
pub fn border_set() -> border::Set<'static> {
    match BORDER_LOOK {
        BorderLook::Thick => BorderType::Thick.to_border_set(),
        BorderLook::Double => BorderType::Double.to_border_set(),
        BorderLook::Block => BLOCK_SET,
        BorderLook::Plain => BorderType::Plain.to_border_set(),
    }
}

// ── estilos compuestos ────────────────────────────────────────────────────

/// Marco ya vestido con el tema (forma de borde, color de borde, título y
/// fondo). Atajo para que las vistas no repitan las cuatro llamadas.
pub fn block<'a>() -> ratatui::widgets::Block<'a> {
    ratatui::widgets::Block::bordered()
        .border_set(border_set())
        .border_style(border_style())
        .title_style(title_style())
        .style(base())
}

/// Estilo base de la vista: fondo y texto del tema.
pub fn base() -> Style {
    Style::new().bg(c().bg).fg(c().fg)
}

/// Estilo del borde (sobre el fondo del tema, para que el marco no herede el
/// fondo del terminal en la celda que ocupa).
pub fn border_style() -> Style {
    Style::new().fg(c().border).bg(c().bg)
}

/// Estilo del título del panel.
pub fn title_style() -> Style {
    Style::new()
        .fg(c().title)
        .bg(c().bg)
        .add_modifier(Modifier::BOLD)
}

/// Cabecera de columna: resaltada si es la columna de orden activa.
pub fn header_style(active: bool) -> Style {
    let p = c();
    Style::new()
        .fg(if active {
            p.header_active
        } else {
            p.header_idle
        })
        .bg(p.bg)
        .add_modifier(Modifier::BOLD)
}

/// Fila seleccionada.
pub fn row_highlight() -> Style {
    Style::new()
        .bg(c().selection_bg)
        .add_modifier(Modifier::BOLD)
}

/// Color por signo. `invert` para métricas donde positivo es "caliente"
/// (funding positivo = los longs pagan = desfavorable). Nació como gemelo
/// temático de `fmt::sign_color` durante el piloto de la Vista 1; ahora que
/// las 9 vistas usan el tema, aquella se quedó sin usuarios y esta es la
/// única implementación.
pub fn sign_color(v: Option<f64>, invert: bool) -> Color {
    let p = c();
    match v {
        None => p.no_data,
        Some(0.0) => p.neutral,
        Some(x) => {
            let good = if invert { x < 0.0 } else { x > 0.0 };
            if good {
                p.positive
            } else {
                p.negative
            }
        }
    }
}

/// Clasificación de flujo con los tonos del tema. Misma correspondencia
/// conceptual que `ranking::regime_color`: acumulación larga = positivo,
/// acumulación corta = negativo, y las dos fases de cierre en tonos aparte
/// (azul y coral) para que no se confundan con la dirección de la posición.
pub fn regime_color(r: crate::signals::Regime) -> Color {
    use crate::signals::Regime;
    let p = c();
    match r {
        Regime::LongBuild => p.positive,
        Regime::ShortBuild => p.negative,
        Regime::ShortCover => p.accent_blue,
        Regime::LongUnwind => p.accent_coral,
        Regime::Flat => p.muted,
    }
}

#[cfg(test)]
mod tests {
    use super::TEST_LOCK as EXCLUSIVA;
    use super::*;

    fn hex(c: Color) -> String {
        match c {
            Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
            other => panic!("el tema debe ser RGB, no {other:?}"),
        }
    }

    /// Los valores de ambas variantes son EXACTAMENTE los acordados. Si
    /// alguien retoca un tono, que sea a propósito y no por un dedo resbalado
    /// en un dígito hex.
    #[test]
    fn la_paleta_tiene_los_valores_exactos() {
        assert_eq!(hex(DARK.bg), "#0B1210");
        assert_eq!(hex(DARK.fg), "#D8DEDA");
        assert_eq!(hex(DARK.fg_strong), "#F2F7F4");
        assert_eq!(hex(DARK.muted), "#5C6B65");
        assert_eq!(hex(DARK.border), "#2E3D38");
        assert_eq!(hex(DARK.title), "#E8C07D");
        assert_eq!(hex(DARK.selection_bg), "#17231F");
        assert_eq!(hex(DARK.cursor), "#97FCE4");
        assert_eq!(hex(DARK.positive), "#97FCE4");
        assert_eq!(hex(DARK.negative), "#F2637A");
        assert_eq!(hex(DARK.neutral), "#9FB0AA");
        assert_eq!(hex(DARK.no_data), "#4A5652");
        assert_eq!(hex(DARK.header_active), "#97FCE4");
        assert_eq!(hex(DARK.header_idle), "#6B7A75");

        assert_eq!(hex(LIGHT.bg), "#F5FAF8");
        assert_eq!(hex(LIGHT.fg), "#16211D");
        assert_eq!(hex(LIGHT.fg_strong), "#0A100D");
        assert_eq!(hex(LIGHT.muted), "#6B7A75");
        assert_eq!(hex(LIGHT.border), "#B8CAC4");
        assert_eq!(hex(LIGHT.title), "#B8843A");
        assert_eq!(hex(LIGHT.selection_bg), "#DCF5EE");
        assert_eq!(hex(LIGHT.cursor), "#0F9C7C");
        assert_eq!(hex(LIGHT.positive), "#0F9C7C");
        assert_eq!(hex(LIGHT.negative), "#C23B52");
        assert_eq!(hex(LIGHT.neutral), "#5A6B65");
        assert_eq!(hex(LIGHT.no_data), "#A8B5B0");
        assert_eq!(hex(LIGHT.header_active), "#0F9C7C");
        assert_eq!(hex(LIGHT.header_idle), "#8A9791");
    }

    /// El verde de marca es el ancla del tema oscuro, tal cual.
    #[test]
    fn el_ancla_es_el_verde_de_marca() {
        assert_eq!(hex(DARK.positive), "#97FCE4");
        // en claro NO se usa tal cual (sería ilegible sobre fondo claro)
        assert_ne!(LIGHT.positive, DARK.positive);
    }

    /// El significado semántico no se reasigna en ninguna variante: positivo
    /// sigue en la familia verde y negativo en la roja, igual que
    /// `fmt::sign_color`. Solo cambia el tono, nunca el concepto.
    #[test]
    fn el_significado_de_positivo_y_negativo_se_conserva() {
        let _g = EXCLUSIVA.lock().unwrap_or_else(|e| e.into_inner());
        for th in [Theme::Dark, Theme::Light] {
            set_theme(th);
            let p = c();
            // positivo (o negativo con `invert`, como el funding) = familia
            // verde; lo contrario = familia roja
            for (v, invert) in [(Some(1.0), false), (Some(-1.0), true)] {
                assert_eq!(sign_color(v, invert), p.positive);
            }
            for (v, invert) in [(Some(-1.0), false), (Some(1.0), true)] {
                assert_eq!(sign_color(v, invert), p.negative);
            }
            // cero real y "sin dato" siguen siendo distinguibles
            assert_eq!(sign_color(Some(0.0), false), p.neutral);
            assert_eq!(sign_color(None, false), p.no_data);
            assert_ne!(p.neutral, p.no_data);
        }
        set_theme(Theme::Dark);
    }

    /// El toggle va y vuelve, y `c()` sigue al estado global.
    #[test]
    fn el_toggle_alterna_las_dos_variantes() {
        let _g = EXCLUSIVA.lock().unwrap_or_else(|e| e.into_inner());
        set_theme(Theme::Dark);
        assert_eq!(c().bg, DARK.bg);
        toggle_theme();
        assert_eq!(theme(), Theme::Light);
        assert_eq!(c().bg, LIGHT.bg);
        toggle_theme();
        assert_eq!(theme(), Theme::Dark);
        assert_eq!(c().bg, DARK.bg);
        assert_eq!(parse_theme("light"), Some(Theme::Light));
        assert_eq!(parse_theme("oscuro"), Some(Theme::Dark));
        assert_eq!(parse_theme("verde"), None);
    }

    /// La capa de imagen (plotters) usa los MISMOS tonos semánticos que el
    /// texto: verde de marca para alcista/positivo, coral para bajista, y el
    /// fondo del tema activo — y cambia con el tema, no se queda fija.
    #[test]
    fn el_raster_sigue_al_tema_y_a_la_semantica() {
        let rgb = |c: Color| match c {
            Color::Rgb(r, g, b) => [r, g, b],
            other => panic!("{other:?}"),
        };
        for (th, p, r) in [
            (Theme::Dark, &DARK, &RASTER_DARK),
            (Theme::Light, &LIGHT, &RASTER_LIGHT),
        ] {
            assert_eq!(r.bg, rgb(p.bg), "el panel no puede tener otro fondo");
            assert_eq!(r.green, rgb(p.positive));
            assert_eq!(r.red, rgb(p.negative));
            assert_eq!(r.mark_buy, rgb(p.positive), "▲ = alcista");
            assert_eq!(r.mark_sell, rgb(p.negative), "▼ = bajista");
            assert_eq!(r.yellow, rgb(p.accent_amber));
            assert_eq!(r.cyan, rgb(p.accent_cyan), "TRIX igual en texto e imagen");
            assert_eq!(r.blue, rgb(p.accent_blue));
            assert_eq!(r.magenta, rgb(p.accent_magenta));
            let _ = th;
        }
        // el ancla de marca llega hasta el raster del tema oscuro
        assert_eq!(RASTER_DARK.green, [0x97, 0xFC, 0xE4]);
        assert_ne!(RASTER_DARK.bg, RASTER_LIGHT.bg);

        let _g = EXCLUSIVA.lock().unwrap_or_else(|e| e.into_inner());
        set_theme(Theme::Light);
        assert_eq!(raster().bg, RASTER_LIGHT.bg);
        set_theme(Theme::Dark);
        assert_eq!(raster().bg, RASTER_DARK.bg);
    }

    /// Los gradientes de las Vistas 4 y 5 salen del acento semántico y del
    /// fondo ACTIVO: extremos anclados, pasos intermedios monótonos, y nunca
    /// se cruzan las dos familias.
    #[test]
    fn los_gradientes_siguen_al_tema_y_a_la_semantica() {
        let _g = EXCLUSIVA.lock().unwrap_or_else(|e| e.into_inner());
        let lum = |c: Color| match c {
            Color::Rgb(r, g, b) => r as i32 + g as i32 + b as i32,
            other => panic!("{other:?}"),
        };
        for th in [Theme::Dark, Theme::Light] {
            set_theme(th);
            let p = c();

            // Vista 4: t = 0 es el fondo del tema; los extremos tiran de la
            // familia correcta y el paso es monótono
            assert_eq!(heat_bg(0.0), p.bg, "sin señal = fondo del tema");
            let mut prev = lum(heat_bg(0.0));
            for k in 1..=10 {
                let cur = lum(heat_bg(k as f64 / 10.0));
                assert_ne!(cur, prev, "cada paso del gradiente debe notarse");
                prev = cur;
            }
            // el extremo positivo se acerca al verde de marca y el negativo
            // al coral, cada uno más que el otro
            // "tirar del verde" o "del coral" se mide por el balance
            // verde−rojo del tono, no por distancia RGB (el coral es más
            // luminoso que el fondo y la distancia euclídea engaña)
            let balance = |col: Color| {
                let x = parts(col);
                x[1] - x[0]
            };
            assert!(
                balance(heat_bg(1.0)) > balance(p.bg),
                "el extremo positivo tira del verde de marca"
            );
            assert!(
                balance(heat_bg(-1.0)) < balance(p.bg),
                "el extremo negativo tira del coral"
            );

            // Vista 5: el extremo saturado ES el acento puro, y más notional
            // = más cerca del acento
            assert_eq!(liq_bar(true, 1.0), p.positive, "longs = verde de marca");
            assert_eq!(liq_bar(false, 1.0), p.negative, "shorts = coral");
            let dist = |a: Color, b: Color| {
                let (x, y) = (parts(a), parts(b));
                (0..3).map(|i| (x[i] - y[i]).abs()).sum::<f64>()
            };
            assert!(dist(liq_bar(true, 0.1), p.positive) > dist(liq_bar(true, 0.9), p.positive));
            assert_ne!(liq_bar(true, 0.3), liq_bar(false, 0.3), "lados distintos");
            // una barra corta ya se lee como su lado, no como fondo
            assert_ne!(liq_bar(true, 0.0), p.bg);

            // los rombos ◆ (liquidaciones REALES de whales) no se confunden
            // con ningún paso del gradiente de barras
            for k in 0..=10 {
                let f = k as f64 / 10.0;
                assert_ne!(p.fg_strong, liq_bar(true, f));
                assert_ne!(p.fg_strong, liq_bar(false, f));
                assert!(
                    (lum(p.fg_strong) - lum(liq_bar(true, f))).abs() > 60,
                    "el ◆ debe destacar sobre la barra de longs"
                );
                assert!(
                    (lum(p.fg_strong) - lum(liq_bar(false, f))).abs() > 60,
                    "el ◆ debe destacar sobre la barra de shorts"
                );
            }
        }
        // el mismo gradiente cambia con el tema, no está fijado a uno
        set_theme(Theme::Dark);
        let d = heat_bg(0.6);
        set_theme(Theme::Light);
        assert_ne!(d, heat_bg(0.6));
        set_theme(Theme::Dark);
    }

    /// El gradiente de sesgo (score compuesto, skew de whales) es continuo,
    /// arranca en el gris neutro y no cruza las familias.
    #[test]
    fn el_gradiente_de_sesgo_es_continuo_y_no_cruza_familias() {
        let _g = EXCLUSIVA.lock().unwrap_or_else(|e| e.into_inner());
        for th in [Theme::Dark, Theme::Light] {
            set_theme(th);
            let p = c();
            let bal = |col: Color| {
                let x = parts(col);
                x[1] - x[0]
            };
            assert_eq!(bias_fg(1.0), p.positive, "sesgo alcista pleno");
            assert_eq!(bias_fg(-1.0), p.negative, "sesgo bajista pleno");
            // más margen = más lejos del neutro, sin saltos de familia
            let dist = |a: Color, b: Color| {
                let (x, y) = (parts(a), parts(b));
                (0..3).map(|i| (x[i] - y[i]).abs()).sum::<f64>()
            };
            let mut prev = dist(bias_fg(0.1), p.neutral);
            for k in 2..=10 {
                let cur = dist(bias_fg(k as f64 / 10.0), p.neutral);
                assert!(cur > prev, "el sesgo debe saturarse al crecer");
                prev = cur;
            }
            assert!(bal(bias_fg(0.5)) > bal(bias_fg(-0.5)), "familias separadas");
        }
        set_theme(Theme::Dark);
    }

    /// Las cuatro formas de borde dan marcos distintos y ninguna se queda a
    /// medias (todas las esquinas definidas).
    #[test]
    fn las_formas_de_borde_estan_completas_y_son_distintas() {
        let sets = [
            BorderType::Thick.to_border_set(),
            BorderType::Double.to_border_set(),
            BLOCK_SET,
            BorderType::Plain.to_border_set(),
        ];
        for s in sets {
            for sym in [
                s.top_left,
                s.top_right,
                s.bottom_left,
                s.bottom_right,
                s.vertical_left,
                s.vertical_right,
                s.horizontal_top,
                s.horizontal_bottom,
            ] {
                assert!(!sym.is_empty(), "símbolo de borde vacío");
            }
        }
        assert_ne!(sets[0].top_left, sets[1].top_left);
        assert_ne!(sets[1].top_left, sets[2].top_left);
        assert_ne!(sets[2].top_left, sets[3].top_left);
        // el aspecto activo es el que se dibuja de verdad
        assert_eq!(
            border_set().top_left,
            BorderType::Thick.to_border_set().top_left
        );
    }
}
