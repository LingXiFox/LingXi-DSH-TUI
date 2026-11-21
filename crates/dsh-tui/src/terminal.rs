//! Terminal ownership: enter the alternate screen, draw, and always restore.
//!
//! Restoration is guarded twice: `restore` runs on the normal path, and `Drop`
//! runs it again if the runtime future is cancelled or unwinds, so a panic in
//! the UI never leaves the user with a garbled shell.

use std::io::{self, Stdout};

use dsh_state::AppState;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::{Hide, Show};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// A ratatui terminal over the real stdout.
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Owns the terminal session and guarantees restoration.
pub struct TerminalSession {
    terminal: Tui,
    restored: bool,
}

impl TerminalSession {
    /// Enter raw mode and the alternate screen.
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    /// Draw one frame.
    pub fn draw(&mut self, state: &AppState) -> io::Result<()> {
        self.terminal
            .draw(|frame| crate::render::draw(frame, state))?;
        Ok(())
    }

    /// Leave the alternate screen and raw mode. Idempotent.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::warn!(%error, "terminal restore failed during drop");
        }
    }
}
#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    #[test]
    fn backend_draws_a_full_frame() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| crate::render::draw(f, &dsh_state::AppState::unversioned("t")))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!((buffer.area.width, buffer.area.height), (80, 24));
    }
}
