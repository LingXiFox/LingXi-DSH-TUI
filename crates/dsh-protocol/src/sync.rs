//! Pure helpers that move the runtime pin and check the wire contract.
//!
//! Everything here is text-in/text-out so the version-bump pipeline can be
//! tested without git, network, or a submodule checkout. The `dsh-runtime-lock`
//! binary supplies the I/O around these functions.

use crate::lock::RuntimeLock;
use crate::version::ReleaseVersion;

/// Result of moving the pin from one upstream release to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinMove {
    /// Tag the lock file pointed at before the move.
    pub from_tag: String,
    /// Tag the lock file points at after the move.
    pub to_tag: String,
    /// Commit before the move.
    pub from_commit: String,
    /// Commit after the move.
    pub to_commit: String,
}

/// Failures while rewriting the repository's version facts.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The manifest had no `[workspace.package]` table.
    #[error("Cargo.toml has no [workspace.package] section")]
    MissingWorkspacePackage,
    /// The table existed but carried no `version` key.
    #[error("[workspace.package] has no version key")]
    MissingVersionKey,
    /// The candidate tag did not parse as an upstream release.
    #[error(transparent)]
    Version(#[from] crate::version::VersionError),
    /// A commit string was not a full 40-character lowercase hex SHA.
    #[error("commit {0:?} is not a full 40-character hex SHA")]
    BadCommit(String),
    /// The lock file rejected the new pin.
    #[error(transparent)]
    Lock(#[from] crate::lock::LockError),
}

/// Read the workspace product version out of a `Cargo.toml`.
pub fn workspace_version(cargo_toml: &str) -> Result<String, SyncError> {
    let (start, end) = workspace_package_span(cargo_toml)?;
    for line in cargo_toml.lines().take(end).skip(start) {
        let Some(value) = line
            .trim_start()
            .strip_prefix("version")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
        else {
            continue;
        };
        return Ok(value.trim().trim_matches('"').to_owned());
    }
    Err(SyncError::MissingVersionKey)
}

/// Rewrite the workspace product version in a `Cargo.toml`, preserving everything else.
///
/// Only the `[workspace.package]` table is touched: per-crate manifests inherit the
/// version with `version.workspace = true`, so one edit keeps a single source of truth.
pub fn rewrite_workspace_version(cargo_toml: &str, version: &str) -> Result<String, SyncError> {
    ReleaseVersion::from_tag(&format!("dsh-v{version}"))?;
    let (start, end) = workspace_package_span(cargo_toml)?;
    let mut lines: Vec<String> = cargo_toml.lines().map(str::to_owned).collect();
    let mut found = false;
    for index in start..end.min(lines.len()) {
        let trimmed = lines[index].trim_start();
        let Some(rest) = trimmed.strip_prefix("version") else {
            continue;
        };
        if !rest.trim_start().starts_with('=') {
            continue;
        }
        let indent = trimmed.len() < lines[index].len();
        let rewritten = format!("version = \"{version}\" ");
        lines[index] = if indent {
            format!("{}{}", "    ", rewritten.trim_end())
        } else {
            rewritten.trim_end().to_owned()
        };
        found = true;
        break;
    }
    if !found {
        return Err(SyncError::MissingVersionKey);
    }
    let mut text = lines.join("\n");
    if cargo_toml.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Line range (start inclusive, end exclusive) of the `[workspace.package]` body.
fn workspace_package_span(cargo_toml: &str) -> Result<(usize, usize), SyncError> {
    let total = cargo_toml.lines().count();
    let mut start = None;
    for (index, line) in cargo_toml.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "[workspace.package]" {
            start = Some(index + 1);
            continue;
        }
        if let Some(begin) = start {
            if trimmed.starts_with('[') {
                return Ok((begin, index));
            }
        }
    }
    match start {
        Some(begin) => Ok((begin, total)),
        None => Err(SyncError::MissingWorkspacePackage),
    }
}

/// Move the pin in memory, validating the new values before anything is written.
pub fn apply_pin(lock: &mut RuntimeLock, tag: &str, commit: &str) -> Result<PinMove, SyncError> {
    let release = ReleaseVersion::from_tag(tag)?;
    if !crate::lock::is_full_sha(commit) {
        return Err(SyncError::BadCommit(commit.to_owned()));
    }
    let moved = PinMove {
        from_tag: lock.runtime.tag.clone(),
        to_tag: release.tag.clone(),
        from_commit: lock.runtime.commit.clone(),
        to_commit: commit.to_owned(),
    };
    let previous = lock.clone();
    lock.runtime.tag = release.tag.clone();
    lock.runtime.version = release.product_version();
    lock.runtime.commit = commit.to_owned();
    if let Err(error) = lock.verify() {
        *lock = previous;
        return Err(SyncError::Lock(error));
    }
    Ok(moved)
}

/// Wire names the upstream protocol source must still declare.
///
/// Kept as data so the contract check cannot silently drift from the enums it
/// mirrors; `expected_wire_names_matches_the_enums` holds the two in lockstep.
pub fn expected_wire_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for method in [
        crate::protocol::ClientMethod::Initialize,
        crate::protocol::ClientMethod::SessionPrompt,
        crate::protocol::ClientMethod::SessionQuery,
        crate::protocol::ClientMethod::SessionResume,
        crate::protocol::ClientMethod::SessionWait,
        crate::protocol::ClientMethod::SessionWorkingDirectoryGet,
        crate::protocol::ClientMethod::SessionWorkingDirectorySet,
        crate::protocol::ClientMethod::Shutdown,
    ] {
        names.push(method.as_str());
    }
    for method in [
        crate::protocol::ServerNotification::SessionEvent,
        crate::protocol::ServerNotification::SessionStatus,
        crate::protocol::ServerNotification::SessionQueryCompleted,
        crate::protocol::ServerNotification::SubagentStarted,
        crate::protocol::ServerNotification::SubagentFinished,
        crate::protocol::ServerNotification::UserQuestionCancelled,
    ] {
        names.push(method.as_str());
    }
    names.push(crate::protocol::METHOD_APPROVAL_REQUEST);
    names
}

/// Names from `expected` that do not appear as a quoted literal in `protocol_source`.
pub fn missing_wire_names(protocol_source: &str, expected: &[&'static str]) -> Vec<String> {
    expected
        .iter()
        .filter(|name| {
            !protocol_source.contains(&format!("{name:?}"))
                && !protocol_source.contains(&format!("'{name}'"))
        })
        .map(|name| (*name).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::SAMPLE_LOCK;

    const MANIFEST: &str = "cargo\n\n[workspace.package]\nname = \"x\"\nversion = \"0.2.2-rc.5\"\nedition = \"2024\"\n\n[workspace.dependencies]\nserde = \"1\"\n";

    #[test]
    fn workspace_version_reads_the_table() {
        assert_eq!(workspace_version(MANIFEST).unwrap(), "0.2.2-rc.5");
    }

    #[test]
    fn rewrite_only_touches_the_workspace_version() {
        let rewritten = rewrite_workspace_version(MANIFEST, "0.2.3").unwrap();
        assert!(rewritten.contains("version = \"0.2.3\"\n"));
        assert_eq!(workspace_version(&rewritten).unwrap(), "0.2.3");
        assert!(rewritten.contains("[workspace.dependencies]\nserde = \"1\"\n"));
        assert!(rewritten.ends_with('\n'));
    }

    #[test]
    fn rewrite_rejects_a_version_that_is_not_a_release() {
        assert!(matches!(
            rewrite_workspace_version(MANIFEST, "not-a-version"),
            Err(SyncError::Version(_))
        ));
    }

    #[test]
    fn missing_workspace_package_is_reported() {
        assert!(matches!(
            workspace_version("[package]\nversion = \"1.0.0\"\n"),
            Err(SyncError::MissingWorkspacePackage)
        ));
    }

    #[test]
    fn missing_version_key_is_reported() {
        let text = "cargo\n\n[workspace.package]\nname = \"x\"\n";
        assert!(matches!(
            workspace_version(text),
            Err(SyncError::MissingVersionKey)
        ));
    }

    #[test]
    fn apply_pin_moves_tag_version_and_commit() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        let moved = apply_pin(
            &mut lock,
            "dsh-v0.2.3-rc.1",
            "0123456789abcdef0123456789abcdef01234567",
        )
        .unwrap();
        assert_eq!(moved.from_tag, "dsh-v0.2.2-rc.5");
        assert_eq!(moved.to_tag, "dsh-v0.2.3-rc.1");
        assert_eq!(lock.runtime.version, "0.2.3-rc.1");
        assert_eq!(lock.runtime.commit, moved.to_commit);
        lock.verify().unwrap();
    }

    #[test]
    fn apply_pin_rolls_back_on_bad_input() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        let before = lock.clone();
        assert!(matches!(
            apply_pin(&mut lock, "dsh-v0.2.3", "short"),
            Err(SyncError::BadCommit(_))
        ));
        assert!(matches!(
            apply_pin(
                &mut lock,
                "v0.2.3",
                "0123456789abcdef0123456789abcdef01234567"
            ),
            Err(SyncError::Version(_))
        ));
        assert_eq!(lock, before);
    }

    #[test]
    fn expected_wire_names_matches_the_enums() {
        let names = expected_wire_names();
        assert_eq!(names.len(), 15);
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "wire names must be unique");
        assert!(names.contains(&"approval/request"));
        assert!(names.contains(&"session/working-directory/get"));
    }

    #[test]
    fn missing_wire_names_finds_absent_literals() {
        let source = "type ClientMethod = 'initialize' | 'shutdown';";
        let missing = missing_wire_names(source, &["initialize", "shutdown", "session/prompt"]);
        assert_eq!(missing, vec!["session/prompt".to_owned()]);
    }

    #[test]
    fn missing_wire_names_accepts_both_quote_styles() {
        let source = "case 'initialize': case \"shutdown\":";
        assert!(missing_wire_names(source, &["initialize", "shutdown"]).is_empty());
    }
}
