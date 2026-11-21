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
        let terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let mut session = Self {
            terminal,
            restored: false,
        };
        enable_raw_mode()?;
        execute!(session.terminal.backend_mut(), EnterAlternateScreen, Hide)?;
        Ok(session)
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
        let raw_mode = disable_raw_mode();
        let screen = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let cursor = execute!(self.terminal.backend_mut(), Show);
        let result = raw_mode.and(screen).and(cursor);
        self.restored = result.is_ok();
        result
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
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("LingXi DSH-TUI"));
        assert!(text.contains("detached / idle"));
    }
}
