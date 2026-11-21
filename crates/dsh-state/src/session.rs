//! The immutable-to-renderer application state and its pure transitions.
//!
//! `AppState` is what the TUI draws. It is built from `Action`s the event loop
//! produces, which keeps rendering a pure function of state and makes every
//! transition testable without a terminal.

use dsh_protocol::lock::RuntimeLock;

use crate::activity::{ActivityKind, ActivityLog};

/// How the runtime subprocess is currently doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TransportState {
    /// Nothing spawned yet; this round always runs here.
    #[default]
    Detached,
    /// A subprocess exists and the handshake is in flight.
    Connecting,
    /// Handshake completed against the reported runtime identity.
    Ready {
        /// Server name reported by `initialize`.
        server_name: String,
        /// Server version reported by `initialize`, when present.
        server_version: Option<String>,
    },
    /// The last attempt failed; the summary is user-facing.
    Failed(String),
}

impl TransportState {
    /// One-word status rendered in the header.
    pub fn label(&self) -> &str {
        match self {
            Self::Detached => "detached",
            Self::Connecting => "connecting",
            Self::Ready { .. } => "ready",
            Self::Failed(_) => "failed",
        }
    }
}

/// Whole-agent run status, mirroring the runtime's own idle/running distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunStatus {
    /// No turn is active.
    #[default]
    Idle,
    /// A turn is being worked on.
    Running,
    /// A turn finished and its summary is unread.
    Attention,
}

impl RunStatus {
    /// One-word status rendered in the header.
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Attention => "attention",
        }
    }
}

/// Which transcript rows are collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldPolicy {
    /// Every tool call and reasoning row is shown.
    #[default]
    Expanded,
    /// Tool calls and reasoning collapse into a one-line summary.
    Collapsed,
}

/// One transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// Speaker label rendered in the gutter, e.g. `user` or `tool`.
    pub speaker: String,
    /// Body text.
    pub body: String,
    /// Tools and reasoning rows may be folded by policy.
    pub foldable: bool,
}

impl TranscriptEntry {
    /// Build a row.
    pub fn new(speaker: impl Into<String>, body: impl Into<String>, foldable: bool) -> Self {
        Self {
            speaker: speaker.into(),
            body: body.into(),
            foldable,
        }
    }
}

/// Everything the renderer needs, and nothing else.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Product version; identical to the bundled Agent version.
    pub tui_version: String,
    /// Agent version derived from the runtime lock.
    pub agent_version: String,
    /// Upstream tag of the bundled Agent.
    pub agent_tag: String,
    /// Short commit of the bundled Agent.
    pub agent_commit_short: String,
    /// Build identifier distinguishing UI-only revisions of the same Agent.
    pub build_id: String,
    /// Transport status.
    pub transport: TransportState,
    /// Run status.
    pub status: RunStatus,
    /// Fold policy for tool/reasoning rows.
    pub fold: FoldPolicy,
    /// Activity log, folded and bounded.
    pub activity: ActivityLog,
    /// Transcript rows.
    pub transcript: Vec<TranscriptEntry>,
    /// Whether the help overlay is open.
    pub help_open: bool,
    /// Whether quit was requested.
    pub should_quit: bool,
}

impl AppState {
    /// Create the startup state for a given pin and build identity.
    pub fn new(lock: &RuntimeLock, build_id: impl Into<String>) -> Self {
        let short = lock.runtime.commit.chars().take(7).collect::<String>();
        let mut state = Self {
            tui_version: lock.product_version().to_owned(),
            agent_version: lock.product_version().to_owned(),
            agent_tag: lock.runtime.tag.clone(),
            agent_commit_short: if short.is_empty() {
                "unknown".into()
            } else {
                short
            },
            build_id: build_id.into(),
            transport: TransportState::Detached,
            status: RunStatus::Idle,
            fold: FoldPolicy::default(),
            activity: ActivityLog::default(),
            transcript: Vec::new(),
            help_open: false,
            should_quit: false,
        };
        state.note(ActivityKind::Lifecycle, "TUI booted");
        state.note(
            ActivityKind::Runtime,
            format!(
                "bundled agent {} @ {} (runtime not spawned this round)",
                state.agent_tag, state.agent_commit_short
            ),
        );
        state
    }

    /// A state with no lock file, used when startup cannot read the pin.
    pub fn unversioned(build_id: impl Into<String>) -> Self {
        let mut state = Self {
            tui_version: "unknown".into(),
            agent_version: "unknown".into(),
            agent_tag: "unknown".into(),
            agent_commit_short: "unknown".into(),
            build_id: build_id.into(),
            transport: TransportState::Detached,
            status: RunStatus::Idle,
            fold: FoldPolicy::default(),
            activity: ActivityLog::default(),
            transcript: Vec::new(),
            help_open: false,
            should_quit: false,
        };
        state.note(
            ActivityKind::Warning,
            "runtime lock unavailable; versions unknown",
        );
        state
    }

    /// Append an activity line.
    pub fn note(&mut self, kind: ActivityKind, message: impl Into<String>) {
        self.activity.push(kind, message);
    }

    /// Header status line shown to the user.
    pub fn header_status(&self) -> String {
        format!("{} / {}", self.transport.label(), self.status.label())
    }

    /// Compact identity line: TUI version, agent version, commit, build id.
    pub fn identity_line(&self) -> String {
        format!(
            "tui {} | agent {} @ {} | build {}",
            self.tui_version, self.agent_version, self.agent_commit_short, self.build_id
        )
    }

    /// Apply one action.
    pub fn apply(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::ToggleHelp => self.help_open = !self.help_open,
            Action::CloseHelp => self.help_open = false,
            Action::ToggleFold => {
                self.fold = match self.fold {
                    FoldPolicy::Expanded => FoldPolicy::Collapsed,
                    FoldPolicy::Collapsed => FoldPolicy::Expanded,
                };
                let label = match self.fold {
                    FoldPolicy::Expanded => "transcript expanded",
                    FoldPolicy::Collapsed => "transcript collapsed",
                };
                self.note(ActivityKind::Lifecycle, label);
            }
            Action::Transport(state) => {
                self.note(
                    ActivityKind::Transport,
                    format!("transport -> {}", state.label()),
                );
                self.transport = state;
            }
            Action::Status(status) => self.status = status,
            Action::Activity(kind, message) => self.note(kind, message),
            Action::Transcript(entry) => self.transcript.push(entry),
        }
    }

    /// Rows that should be drawn given the current fold policy.
    pub fn visible_transcript(&self) -> Vec<&TranscriptEntry> {
        let collapse = self.fold == FoldPolicy::Collapsed;
        self.transcript
            .iter()
            .filter(|entry| !(collapse && entry.foldable))
            .collect()
    }
}

/// Every way the UI state may change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Quit the application.
    Quit,
    /// Open the key help overlay.
    ToggleHelp,
    /// Close the key help overlay.
    CloseHelp,
    /// Flip tool/reasoning folding.
    ToggleFold,
    /// Update transport status.
    Transport(TransportState),
    /// Update run status.
    Status(RunStatus),
    /// Append an activity line.
    Activity(ActivityKind, String),
    /// Append a transcript row.
    Transcript(TranscriptEntry),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(&RuntimeLock::parse(&lock_text()).unwrap(), "test-build")
    }

    fn lock_text() -> String {
        std::fs::read_to_string(workspace_lock())
            .unwrap_or_else(|_| dsh_protocol::lock::SAMPLE_LOCK.to_owned())
    }

    fn workspace_lock() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../runtime/upstream.lock.toml")
    }

    #[test]
    fn startup_records_identity_and_boot_notes() {
        let state = state();
        assert_eq!(state.tui_version, "0.2.2-rc.5");
        assert_eq!(state.agent_version, "0.2.2-rc.5");
        assert_eq!(state.agent_commit_short.len(), 7);
        assert_eq!(state.activity.len(), 2);
        assert!(!state.should_quit);
    }

    #[test]
    fn identity_line_contains_all_identity_fields() {
        let line = state().identity_line();
        assert!(line.contains("tui 0.2.2-rc.5"));
        assert!(line.contains("agent 0.2.2-rc.5"));
        assert!(line.contains("@ 9c2670d"));
        assert!(line.contains("build test-build"));
    }

    #[test]
    fn quit_is_sticky() {
        let mut state = state();
        state.apply(Action::Quit);
        assert!(state.should_quit);
    }

    #[test]
    fn fold_toggles_and_hides_foldable_rows() {
        let mut state = state();
        state.apply(Action::Transcript(TranscriptEntry::new(
            "user", "hello", false,
        )));
        state.apply(Action::Transcript(TranscriptEntry::new(
            "tool", "bash ls", true,
        )));
        assert_eq!(state.visible_transcript().len(), 2);
        state.apply(Action::ToggleFold);
        assert_eq!(state.fold, FoldPolicy::Collapsed);
        assert_eq!(state.visible_transcript().len(), 1);
        state.apply(Action::ToggleFold);
        assert_eq!(state.visible_transcript().len(), 2);
    }

    #[test]
    fn transport_transitions_update_header() {
        let mut state = state();
        assert_eq!(state.header_status(), "detached / idle");
        state.apply(Action::Transport(TransportState::Connecting));
        assert_eq!(state.header_status(), "connecting / idle");
        state.apply(Action::Transport(TransportState::Ready {
            server_name: "deepseek-harness-sdk-runtime".into(),
            server_version: None,
        }));
        assert_eq!(state.header_status(), "ready / idle");
        state.apply(Action::Status(RunStatus::Running));
        assert_eq!(state.header_status(), "ready / running");
        state.apply(Action::Transport(TransportState::Failed(
            "spawn failed".into(),
        )));
        assert_eq!(state.header_status(), "failed / running");
    }

    #[test]
    fn help_overlay_opens_and_closes() {
        let mut state = state();
        assert!(!state.help_open);
        state.apply(Action::ToggleHelp);
        assert!(state.help_open);
        state.apply(Action::CloseHelp);
        assert!(!state.help_open);
    }

    #[test]
    fn unversioned_state_warns_instead_of_panicking() {
        let state = AppState::unversioned("dev");
        assert_eq!(state.tui_version, "unknown");
        assert_eq!(state.activity.len(), 1);
        assert!(
            state.activity.entries()[0]
                .message
                .contains("runtime lock unavailable")
        );
    }
}
