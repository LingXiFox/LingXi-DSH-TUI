//! Frame-level tests: draw into a `TestBackend` and assert on the buffer.

use dsh_state::{Action, AppState, TransportState};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use super::*;

fn render(state: &AppState, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn state() -> AppState {
    let lock = dsh_protocol::lock::RuntimeLock::parse(dsh_protocol::lock::SAMPLE_LOCK).unwrap();
    AppState::new(&lock, "test-build")
}

#[test]
fn renders_project_name_versions_and_status() {
    let text = render(&state(), 100, 30);
    assert!(text.contains("LingXi DSH-TUI"), "missing title:\n{text}");
    assert!(text.contains("0.2.2-rc.5"), "missing version:\n{text}");
    assert!(text.contains("dsh-v0.2.2-rc.5"), "missing tag:\n{text}");
    assert!(text.contains("9c2670d"), "missing commit:\n{text}");
    assert!(text.contains("detached / idle"), "missing status:\n{text}");
    assert!(text.contains("test-build"), "missing build id:\n{text}");
}

#[test]
fn renders_scope_note_for_round_one() {
    let text = render(&state(), 100, 30);
    assert!(
        text.contains("Round 1 runs the UI skeleton only"),
        "missing scope note:\n{text}"
    );
}

#[test]
fn header_colour_tracks_transport_state() {
    let mut state = state();
    state.apply(Action::Transport(TransportState::Failed("boom".into())));
    let text = render(&state, 100, 30);
    assert!(text.contains("failed / idle"));

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw(frame, &state)).unwrap();
    let buffer = terminal.backend().buffer();
    let failed_style = buffer[(0, 0)].style();
    assert_eq!(
        failed_style.fg,
        Some(Color::DarkGray),
        "border keeps neutral colour"
    );
}

#[test]
fn help_overlay_covers_the_screen_centre() {
    let mut state = state();
    state.apply(Action::ToggleHelp);
    let text = render(&state, 100, 30);
    assert!(
        text.contains("LingXi DSH-TUI keymap"),
        "missing help body:\n{text}"
    );
    assert!(text.contains("quit"), "missing key list:\n{text}");
    assert!(text.contains(" fold or expand "));
}

#[test]
fn degraded_state_renders_without_lock_versions() {
    let text = render(&AppState::unversioned("dev"), 90, 24);
    assert!(text.contains("unknown"));
    assert!(
        text.contains("wrn"),
        "warning badge should be visible:\n{text}"
    );
    assert!(
        text.contains("runtime lock unavailable"),
        "warning row should be visible"
    );
}

#[test]
fn small_terminal_does_not_panic() {
    let state = state();
    for (w, h) in [(20u16, 6u16), (40, 10), (60, 16)] {
        let text = render(&state, w, h);
        assert!(!text.is_empty());
    }
}

#[test]
fn footer_hint_mentions_fold_and_help() {
    let text = render(&state(), 100, 30);
    assert!(text.contains("? help"));
    assert!(text.contains("f fold"));
    let mut state = state();
    state.apply(Action::ToggleHelp);
    assert!(render(&state, 100, 30).contains("close help"));
}

#[test]
fn fold_policy_hides_foldable_rows() {
    let mut state = state();
    state.apply(Action::Transcript(dsh_state::TranscriptEntry::new(
        "tool",
        "read_file main.rs",
        true,
    )));
    state.apply(Action::Transcript(dsh_state::TranscriptEntry::new(
        "agent",
        "done reading",
        false,
    )));

    let expanded = render(&state, 120, 30);
    assert!(
        expanded.contains("read_file main.rs"),
        "expanded shows tool row:\n{expanded}"
    );
    assert!(expanded.contains("done reading"));

    state.apply(Action::ToggleFold);
    let collapsed = render(&state, 120, 30);
    assert!(
        !collapsed.contains("read_file main.rs"),
        "collapsed hides foldable row:\n{collapsed}"
    );
    assert!(
        collapsed.contains("done reading"),
        "collapsed keeps plain rows"
    );
    assert!(
        collapsed.contains("1 folded"),
        "collapsed reports hidden count:\n{collapsed}"
    );
}
