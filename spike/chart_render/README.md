# Spike: renderizado de osciladores vía protocolo gráfico Kitty

Comparativa aislada (CLAUDE.md → "Spike previo: renderizado real vía protocolo gráfico Kitty")
entre los dos candidatos para sustituir el Canvas Braille de los paneles RSI/ADX/DMI
(Vistas 2 y 3). Datos RSI(14) sintéticos y deterministas (`rsi_data/`), sin red.

## Candidatos

- **`img_plotters/`** (candidato A): `plotters` rasteriza el oscilador a una imagen RGB
  (supersampling 2x + Lanczos = antialiasing real) y `ratatui-image` la pinta dentro del
  layout de ratatui vía protocolo Kitty, con fallback automático a halfblocks.
- **`plt/`** (candidato B): `LinePlot` de `ratatui-plt 0.0.2`.

## Cómo verlos (en Kitty/Ghostty/WezTerm)

```sh
cargo run --bin img_plotters   # candidato A (auto-detecta el protocolo del terminal)
cargo run --bin plt            # candidato B
# fallback del A en terminales sin gráficos:
CHART_PROTO=halfblocks cargo run --bin img_plotters
```

## Veredicto: gana A (ratatui-image + plotters), B descartado

Evidencia en `captures/` (generada con `tools/`, ver abajo):

| Artefacto | Qué es |
|---|---|
| `a_kitty_img_0.png` | Lo que Kitty dibuja para A: línea ámbar antialiased + MA cian, banda 30–70, niveles discontinuos con etiqueta. Aspecto TradingView real. |
| `a_half.png` | Fallback halfblocks de A: pixelado suave pero continuo y legible como forma; funcional en cualquier terminal. |
| `b_live.png` | B en vivo: línea de **puntos braille** — el mismo aspecto "scatter" que ya se descartó en las velas. Ejes/leyenda pulidos, pero el trazo es el problema. |
| `plt_export.png` | El "modo kitty" de B: NO es render en vivo, es un export offline que pinta un rectángulo plano por celda (ni rasteriza glifos). Inutilizable como camino visual. |

Hallazgos de código fuente (verificados en el crate, no en el README):

1. `ratatui-plt` dibuja `LinePlot` con Bresenham sobre **braille** (`draw_braille_line`,
   `src/widgets/line_plot.rs`) — es render de caracteres, no de píxeles. Sus features
   `kitty`/`sixel` solo existen en `export.rs` (`buffer_to_kitty` → `buffer_to_png`, que
   convierte cada celda en un rectángulo de color). No hay camino de píxeles en vivo.
2. Ambos crates exigen **ratatui 0.30** (`ratatui-image 11` pide `^0.30.1`; producción
   está en 0.29 → adoptar A implica esa subida de versión).
3. `ratatui-image` con features por defecto arrastra `chafa-dyn` (librería C del sistema);
   con `default-features = false, features = ["crossterm"]` compila limpio.
4. `ratatui-image` usa el modo de *unicode placeholders* del protocolo Kitty (`U=1`),
   soportado por el Kitty 0.47 del usuario; transmite RGBA crudo (~3 MB por re-render a
   1080×560) — irrelevante en local para redibujos por cambio de datos/resize, a vigilar
   solo si algún día se redibuja a cadencia sub-segundo.

## Herramientas de captura (`tools/`, reusables para validar cualquier TUI)

Sin tmux/screen en la máquina; requieren venv con `pyte` + `pillow`:

- `drive.py` — conduce el binario en un pty (TIOCSWINSZ con tamaño en px), captura bytes
  crudos + pantalla final estilada (`.cells.json`).
- `decode_kitty.py` — reensambla las transmisiones APC del protocolo Kitty de una captura
  cruda y las guarda como PNG (f=32/24/100, chunks m=1, zlib opcional).
- `render_screen.py` — rasteriza `.cells.json` a PNG imitando cómo pinta Kitty
  (braille/bloques/cuadrantes procedurales, texto con JetBrains Mono).
