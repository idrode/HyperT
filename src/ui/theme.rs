//! Tema visual "Midsummer Night" — piloto aplicado SOLO a la Vista 1
//! (Ranking). Mismo patrón que `src/i18n.rs`: un único sitio donde vive el
//! estilo, para no volver a tener colores sueltos repartidos por las vistas.
//!
//! ## Cómo tocar los colores
//! Todo lo que se ve en la Vista 1 sale de las constantes de este archivo.
//! Cambia el valor aquí y afecta a la vista entera; no hay ningún color
//! escrito a mano en `src/ui/ranking.rs`.
//!
//! | Qué controla                     | Constante            |
//! |----------------------------------|----------------------|
//! | Fondo del panel                  | [`BG`]               |
//! | Texto general                    | [`FG`]               |
//! | Texto realzado (nombre del par)  | [`FG_STRONG`]        |
//! | Texto apagado (índice, sin dato) | [`MUTED`]            |
//! | Bordes del panel                 | [`BORDER`]           |
//! | Título del panel                 | [`TITLE`]            |
//! | Fondo de la fila seleccionada    | [`SELECTION_BG`]     |
//! | Cursor / marcador de selección   | [`CURSOR`]           |
//! | Positivo (ganancia, favorable)   | [`POSITIVE`]         |
//! | Negativo (pérdida, desfavorable) | [`NEGATIVE`]         |
//! | Valor exactamente cero           | [`NEUTRAL`]          |
//! | Columna de orden activa          | [`HEADER_ACTIVE`]    |
//! | Resto de cabeceras               | [`HEADER_IDLE`]      |
//!
//! El GROSOR/forma del borde se elige con [`BORDER_LOOK`] (ver [`BorderLook`]).
//!
//! ## Nombres semánticos, no ANSI
//! Los nombres ANSI de esta paleta engañan: su "green" (`color2`) es en
//! realidad un teal `#2DCBBE`, y su "yellow" (`color3`) es un naranja
//! `#E5A382`. Por eso aquí nada se llama `GREEN` ni `YELLOW` — cada constante
//! se nombra por su función o por su tono real.
//!
//! ## Qué NO toca este módulo
//! Las otras 8 vistas siguen con los colores de antes (`fmt::sign_color` y
//! `ranking::regime_color` se dejan intactos a propósito: los comparten casi
//! todas las vistas, y cambiarlos habría repintado el TUI entero). Tampoco
//! toca los gráficos rasterizados por plotters (`src/ui/oscimg.rs`), que
//! llevan su propia paleta.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::widgets::BorderType;

// ── paleta cruda (Midsummer Night, formato Kitty → RGB) ───────────────────

/// `background` — fondo de la vista.
pub const BG: Color = Color::Rgb(0x1C, 0x1E, 0x26);
/// `foreground` — texto general.
pub const FG: Color = Color::Rgb(0xC6, 0xB8, 0xB1);
/// `color7`/`color15` (white) — texto realzado, más frío que el `FG`.
pub const FG_STRONG: Color = Color::Rgb(0xB1, 0xC7, 0xC9);
/// `color8` (bright black) — texto apagado y "sin dato".
pub const MUTED: Color = Color::Rgb(0x66, 0x66, 0x66);
/// `selection_background` — fondo de la fila seleccionada.
pub const SELECTION_BG: Color = Color::Rgb(0x2E, 0x30, 0x3E);
/// `cursor` — coral/rosa del cursor y del marcador de fila.
pub const CURSOR: Color = Color::Rgb(0xD3, 0x4C, 0x6B);

/// `color2`/`color6` — OJO: la paleta lo llama "green"/"cyan", pero es un
/// verde-azulado (teal), no un verde puro.
pub const ACCENT_TEAL: Color = Color::Rgb(0x2D, 0xCB, 0xBE);
/// `color3` — la paleta lo llama "yellow", pero es un naranja/melocotón.
pub const ACCENT_ORANGE: Color = Color::Rgb(0xE5, 0xA3, 0x82);
/// `color4` / `url_color` — azul-cian.
pub const ACCENT_BLUE: Color = Color::Rgb(0x35, 0xA5, 0xBB);
/// `color5` (magenta) — coral, el tono "fuera de la escala positivo/negativo".
pub const ACCENT_CORAL: Color = Color::Rgb(0xD3, 0x4C, 0x68);
/// `color1` (red) — familia roja de la paleta.
pub const ACCENT_RED: Color = Color::Rgb(0xD8, 0x50, 0x69);

// ── roles semánticos ──────────────────────────────────────────────────────
// Preservan el significado YA establecido en el proyecto; solo cambia el tono
// exacto, nunca a qué concepto corresponde cada familia de color.

/// Positivo / ganancia / condición favorable. Sigue siendo la familia "verde"
/// de la paleta, que aquí es el teal.
pub const POSITIVE: Color = ACCENT_TEAL;
/// Negativo / pérdida / condición desfavorable. Familia roja.
pub const NEGATIVE: Color = ACCENT_RED;
/// Cero exacto: ni bueno ni malo, pero es un dato real (distinto de "sin dato").
pub const NEUTRAL: Color = FG_STRONG;
/// Sin dato. Deliberadamente distinto de `NEUTRAL`: un cero real y un hueco
/// no se pintan igual (criterio de honestidad de datos del proyecto).
pub const NO_DATA: Color = MUTED;

/// Borde del panel.
pub const BORDER: Color = ACCENT_BLUE;
/// Título del panel.
pub const TITLE: Color = ACCENT_ORANGE;
/// Cabecera de la columna por la que se está ordenando.
pub const HEADER_ACTIVE: Color = ACCENT_TEAL;
/// Cabeceras del resto de columnas.
pub const HEADER_IDLE: Color = Color::Rgb(0x87, 0x95, 0x96);

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

/// Estilo base de la vista: fondo y texto del tema.
pub fn base() -> Style {
    Style::new().bg(BG).fg(FG)
}

/// Estilo del borde (sobre el fondo del tema, para que el marco no herede el
/// fondo del terminal en la celda que ocupa).
pub fn border_style() -> Style {
    Style::new().fg(BORDER).bg(BG)
}

/// Estilo del título del panel.
pub fn title_style() -> Style {
    Style::new()
        .fg(TITLE)
        .bg(BG)
        .add_modifier(Modifier::BOLD)
}

/// Cabecera de columna: resaltada si es la columna de orden activa.
pub fn header_style(active: bool) -> Style {
    Style::new()
        .fg(if active { HEADER_ACTIVE } else { HEADER_IDLE })
        .bg(BG)
        .add_modifier(Modifier::BOLD)
}

/// Fila seleccionada.
pub fn row_highlight() -> Style {
    Style::new()
        .bg(SELECTION_BG)
        .add_modifier(Modifier::BOLD)
}

/// Color por signo, con los tonos del tema. Espeja exactamente la semántica
/// de `fmt::sign_color` (incluido `invert`, para métricas donde positivo es
/// "caliente" — funding positivo = los longs pagan = desfavorable); solo
/// cambian los tonos. Se define aquí en vez de tocar `fmt::sign_color` porque
/// esa la comparten casi todas las vistas y el piloto es solo la Vista 1.
pub fn sign_color(v: Option<f64>, invert: bool) -> Color {
    match v {
        None => NO_DATA,
        Some(0.0) => NEUTRAL,
        Some(x) => {
            let good = if invert { x < 0.0 } else { x > 0.0 };
            if good {
                POSITIVE
            } else {
                NEGATIVE
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
    match r {
        Regime::LongBuild => POSITIVE,
        Regime::ShortBuild => NEGATIVE,
        Regime::ShortCover => ACCENT_BLUE,
        Regime::LongUnwind => ACCENT_CORAL,
        Regime::Flat => MUTED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Los valores de la paleta son EXACTAMENTE los del esquema Midsummer
    /// Night. Si alguien retoca un tono, que sea a propósito y no por un dedo
    /// resbalado en un dígito hex.
    #[test]
    fn la_paleta_tiene_los_valores_exactos() {
        let hex = |c: Color| match c {
            Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
            other => panic!("el tema debe ser RGB, no {other:?}"),
        };
        assert_eq!(hex(BG), "#1C1E26");
        assert_eq!(hex(FG), "#C6B8B1");
        assert_eq!(hex(FG_STRONG), "#B1C7C9");
        assert_eq!(hex(MUTED), "#666666");
        assert_eq!(hex(SELECTION_BG), "#2E303E");
        assert_eq!(hex(CURSOR), "#D34C6B");
        assert_eq!(hex(ACCENT_TEAL), "#2DCBBE");
        assert_eq!(hex(ACCENT_ORANGE), "#E5A382");
        assert_eq!(hex(ACCENT_BLUE), "#35A5BB");
        assert_eq!(hex(ACCENT_CORAL), "#D34C68");
        assert_eq!(hex(ACCENT_RED), "#D85069");
        assert_eq!(hex(HEADER_IDLE), "#879596");
        // el teal y el coral son tonos DISTINTOS pese a parecerse de nombre
        assert_ne!(ACCENT_CORAL, ACCENT_RED, "coral #D34C68 ≠ rojo #D85069");
        assert_ne!(ACCENT_CORAL, CURSOR, "coral #D34C68 ≠ cursor #D34C6B");
    }

    /// El significado semántico no se reasigna: positivo sigue en la familia
    /// "verde" de la paleta (que aquí es teal) y negativo en la roja, igual
    /// que `fmt::sign_color`. Solo cambia el tono, nunca el concepto.
    #[test]
    fn el_significado_de_positivo_y_negativo_se_conserva() {
        // mismo criterio que fmt::sign_color, tono a tono
        for (v, invert) in [(Some(1.0), false), (Some(-1.0), true)] {
            assert_eq!(sign_color(v, invert), POSITIVE);
            assert_eq!(super::super::fmt::sign_color(v, invert), Color::Green);
        }
        for (v, invert) in [(Some(-1.0), false), (Some(1.0), true)] {
            assert_eq!(sign_color(v, invert), NEGATIVE);
            assert_eq!(super::super::fmt::sign_color(v, invert), Color::Red);
        }
        // cero real y "sin dato" siguen siendo distinguibles
        assert_eq!(sign_color(Some(0.0), false), NEUTRAL);
        assert_eq!(sign_color(None, false), NO_DATA);
        assert_ne!(NEUTRAL, NO_DATA);
        assert_eq!(POSITIVE, ACCENT_TEAL);
        assert_eq!(NEGATIVE, ACCENT_RED);
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
        assert_eq!(border_set().top_left, BorderType::Thick.to_border_set().top_left);
    }
}
