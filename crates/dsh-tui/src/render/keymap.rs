//! Keyboard mapping: the only place raw key events become `Action`s.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dsh_state::Action;

/// Translate one key event into an action, honouring the help overlay.
///
/// While help is open only `?`, `Esc` and `q` are accepted, so a stray `f`
/// cannot silently fold the transcript behind the modal.
pub fn action_for(key: KeyEvent, help_open: bool) -> Option<Action> {
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    if help_open {
        return match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Some(Action::Quit),
            KeyCode::Char('?') | KeyCode::Char('x') => Some(Action::CloseHelp),
            _ => None,
        };
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('f') | KeyCode::Char('F') => Some(Action::ToggleFold),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn release_events_are_ignored() {
        let mut key = press(KeyCode::Char('q'));
        key.kind = KeyEventKind::Release;
        assert_eq!(action_for(key, false), None);
    }

    #[test]
    fn alt_and_ctrl_chords_are_not_actions() {
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT);
        assert_eq!(action_for(key, false), None);
        let key = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(action_for(key, false), None);
    }

    #[test]
    fn help_modal_blocks_fold() {
        assert_eq!(action_for(press(KeyCode::Char('f')), true), None);
        assert_eq!(
            action_for(press(KeyCode::Char('f')), false),
            Some(Action::ToggleFold)
        );
        assert_eq!(
            action_for(press(KeyCode::Char('?')), true),
            Some(Action::CloseHelp)
        );
    }
}
