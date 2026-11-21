//! Bounded activity log with adjacent-duplicate folding.
//!
//! The log is the one place that grows without user input, so it is capped and
//! folds repeats: the same notice arriving thirty times becomes one row with a
//! counter instead of thirty rows that push the interesting lines off screen.

use std::collections::VecDeque;

/// Categories rendered with distinct badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityKind {
    /// Startup/shutdown lifecycle.
    Lifecycle,
    /// Runtime handshake and version information.
    Runtime,
    /// Transport/protocol chatter.
    Transport,
    /// Problems the user should read.
    Warning,
}

impl ActivityKind {
    /// Short badge shown in the gutter.
    pub fn badge(self) -> &'static str {
        match self {
            Self::Lifecycle => "sys",
            Self::Runtime => "rt ",
            Self::Transport => "net",
            Self::Warning => "wrn",
        }
    }
}

/// One folded row of the activity log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    /// Category of the message.
    pub kind: ActivityKind,
    /// Text shown to the user.
    pub message: String,
    /// How many identical consecutive messages this row absorbed.
    pub repeats: u32,
}

impl ActivityEntry {
    /// Build a fresh entry with one occurrence.
    pub fn new(kind: ActivityKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            repeats: 1,
        }
    }

    /// Render the row, appending the repeat counter when folded.
    pub fn render(&self) -> String {
        if self.repeats > 1 {
            format!("{} {}  x{}", self.kind.badge(), self.message, self.repeats)
        } else {
            format!("{} {}", self.kind.badge(), self.message)
        }
    }
}

/// Fixed-capacity log that folds adjacent duplicates.
#[derive(Debug, Clone)]
pub struct ActivityLog {
    entries: VecDeque<ActivityEntry>,
    capacity: usize,
    folded: u64,
}

impl Default for ActivityLog {
    fn default() -> Self {
        Self::new(256)
    }
}

impl ActivityLog {
    /// Create a log that keeps at most `capacity` rows.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
            folded: 0,
        }
    }

    /// Push a message, folding it into the previous row when identical.
    pub fn push(&mut self, kind: ActivityKind, message: impl Into<String>) {
        let message = message.into();
        if let Some(last) = self.entries.back_mut()
            && last.kind == kind
            && last.message == message
        {
            last.repeats += 1;
            self.folded += 1;
            return;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(ActivityEntry::new(kind, message));
    }

    /// Rows in insertion order.
    pub fn entries(&self) -> &VecDeque<ActivityEntry> {
        &self.entries
    }

    /// Rows that were folded into a previous entry.
    pub fn folded_count(&self) -> u64 {
        self.folded
    }

    /// Number of retained rows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The last `limit` rows, oldest first.
    pub fn tail(&self, limit: usize) -> impl Iterator<Item = &ActivityEntry> {
        let skip = self.entries.len().saturating_sub(limit);
        self.entries.iter().skip(skip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_adjacent_duplicates() {
        let mut log = ActivityLog::new(8);
        log.push(ActivityKind::Transport, "waiting for runtime");
        log.push(ActivityKind::Transport, "waiting for runtime");
        log.push(ActivityKind::Transport, "waiting for runtime");
        assert_eq!(log.len(), 1);
        assert_eq!(log.entries()[0].repeats, 3);
        assert_eq!(log.folded_count(), 2);
        assert_eq!(log.entries()[0].render(), "net waiting for runtime  x3");
    }

    #[test]
    fn kind_change_breaks_the_fold() {
        let mut log = ActivityLog::new(8);
        log.push(ActivityKind::Transport, "same text");
        log.push(ActivityKind::Warning, "same text");
        assert_eq!(log.len(), 2);
        assert_eq!(log.entries()[1].repeats, 1);
    }

    #[test]
    fn capacity_drops_oldest_rows() {
        let mut log = ActivityLog::new(3);
        for i in 0..10 {
            log.push(ActivityKind::Lifecycle, format!("line {i}"));
        }
        assert_eq!(log.len(), 3);
        let tail: Vec<_> = log.tail(3).map(|e| e.message.clone()).collect();
        assert_eq!(tail, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn tail_saturates_when_shorter_than_limit() {
        let mut log = ActivityLog::new(8);
        log.push(ActivityKind::Runtime, "one");
        assert_eq!(log.tail(5).count(), 1);
        assert!(ActivityLog::new(8).is_empty());
    }

    #[test]
    fn badges_are_stable() {
        assert_eq!(ActivityKind::Lifecycle.badge(), "sys");
        assert_eq!(ActivityKind::Runtime.badge(), "rt ");
        assert_eq!(ActivityKind::Transport.badge(), "net");
        assert_eq!(ActivityKind::Warning.badge(), "wrn");
    }
}
