use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::App;
use crate::data::types::DataMsg;
use crate::ui;

pub struct Tui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        let backend = CrosstermBackend::new(stdout);
        Ok(Self {
            terminal: Terminal::new(backend)?,
        })
    }

    pub async fn run(&mut self, app: &mut App, rx: &mut UnboundedReceiver<DataMsg>) -> Result<()> {
        let tick = Duration::from_millis(250);
        let mut had_overlay = false;
        loop {
            while let Ok(msg) = rx.try_recv() {
                app.apply_msg(msg);
            }
            app.tick_refresh();
            self.terminal.draw(|f| ui::draw(f, app))?;
            // Artefacto Kitty al cerrar un overlay sobre los paneles de imagen
            // (Vistas 2 y 3): ratatui-image mete toda la fila de placeholders
            // en la PRIMERA celda y marca el resto como `Skip`, así que el
            // diff solo reemite la fila si cambia esa primera celda. Un modal
            // centrado (ayuda, buscador…) no llega a la columna 0, luego al
            // cerrarlo el diff ve la fila igual que antes y no repinta nada:
            // el texto del modal se queda incrustado sobre la gráfica.
            // Arreglo: al cerrarse el overlay se dibuja un frame intermedio con
            // los paneles de imagen EN BLANCO (celdas normales, sin Skip), que
            // el diff sí repinta entero y borra el resto del modal; el frame
            // siguiente vuelve a poner los placeholders, que ahora difieren del
            // blanco y se reemiten. La imagen reaparece sola porque sigue
            // residente en el terminal.
            // Descartado emitir el borrado de imágenes de Kitty (`_Ga=d`):
            // liberaría los datos de la imagen, que ratatui-image transmite una
            // sola vez por protocolo, dejando el panel vacío hasta el siguiente
            // re-raster. Descartado también `Terminal::clear`: interroga la
            // posición del cursor (DSR) contra el tty en cada cierre de modal,
            // round-trip innecesario que compite con la lectura de eventos.
            let overlay = app.overlay_drawn.get();
            if had_overlay && !overlay {
                app.gfx.blank_once = true;
                self.terminal.draw(|f| ui::draw(f, app))?;
                app.gfx.blank_once = false;
                self.terminal.draw(|f| ui::draw(f, app))?;
            }
            had_overlay = overlay;
            if event::poll(tick)? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        app.handle_key(key);
                    }
                    Event::Mouse(me) => app.handle_mouse(me),
                    Event::Paste(data) => app.handle_input_paste(&data),
                    _ => {}
                }
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}
