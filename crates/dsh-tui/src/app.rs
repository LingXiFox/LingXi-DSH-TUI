//! The event loop: keyboard input, resize, and a clean shutdown path.
//!
//! Input is read on a dedicated blocking thread and handed to the async loop
//! over a channel: `crossterm::event::read` blocks, and blocking the runtime's
//! worker inside `select!` would stall the ticker.

use std::io;

use crossterm::event::Event;
use dsh_state::AppState;
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior, interval};

use crate::render;
use crate::terminal::TerminalSession;

const TICK: Duration = Duration::from_millis(250);

// Compile-time invariant: the channel must absorb a burst of key repeats
// without stalling the reader thread.
const _: () = assert!(INPUT_CHANNEL >= 16);
const INPUT_CHANNEL: usize = 64;

/// Run the UI until the user quits or the terminal fails.
pub async fn run(state: AppState) -> io::Result<()> {
    let mut session = TerminalSession::enter()?;
    let result = event_loop(&mut session, state).await;
    session.restore()?;
    result
}

/// Spawn the blocking reader that pumps crossterm events into `sender`.
fn spawn_input_reader(sender: mpsc::Sender<Event>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("dsh-tui-input".into())
        .spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(event) => {
                        if sender.blocking_send(event).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "input reader stopped");
                        break;
                    }
                }
            }
        })
        .expect("spawn input thread")
}

async fn event_loop(session: &mut TerminalSession, mut state: AppState) -> io::Result<()> {
    let (sender, mut receiver) = mpsc::channel::<Event>(INPUT_CHANNEL);
    let reader = spawn_input_reader(sender);
    let mut ticker = interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    session.draw(&state)?;
    while !state.should_quit {
        tokio::select! {
            maybe_event = receiver.recv() => {
                let Some(event) = maybe_event else { break };
                match event {
                    Event::Key(key) => {
                        if let Some(action) = render::keymap::action_for(key, state.help_open) {
                            state.apply(action);
                            session.draw(&state)?;
                        }
                    }
                    Event::Resize(_, _) => session.draw(&state)?,
                    _ => {}
                }
            }
            _ = ticker.tick() => {}
        }
    }
    drop(receiver);
    let _ = reader.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use dsh_state::Action;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn tick_is_short_enough_to_feel_live() {
        assert!(TICK <= Duration::from_millis(500));
    }

    #[test]
    fn quit_keys_are_mapped() {
        use crate::render::keymap::action_for;
        assert!(matches!(
            action_for(key(KeyCode::Char('q')), false),
            Some(Action::Quit)
        ));
        assert!(matches!(
            action_for(key(KeyCode::Esc), false),
            Some(Action::Quit)
        ));
        assert!(matches!(
            action_for(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false
            ),
            Some(Action::Quit)
        ));
        assert!(matches!(
            action_for(key(KeyCode::Char('Q')), false),
            Some(Action::Quit)
        ));
        assert_eq!(action_for(key(KeyCode::Enter), false), None);
        assert_eq!(
            action_for(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                false
            ),
            None
        );
    }
}
