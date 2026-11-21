//! The upstream runtime pin: `runtime/upstream.lock.toml`.
//!
//! One file is the single source of truth for the bundled Agent version. The
//! TUI reads it at startup, CI re-derives it from the submodule and compares,
//! and the sync pipeline rewrites it in the same commit that moves the pin.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::version::{Compatibility, ReleaseVersion, VersionError};

/// Default location of the lock file, relative to the workspace root.
pub const DEFAULT_LOCK_PATH: &str = "runtime/upstream.lock.toml";

/// Parsed contents of `runtime/upstream.lock.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeLock {
    /// Upstream repository metadata and the exact commit the TUI ships against.
    pub runtime: RuntimeSection,
    /// Schema revision of the lock file itself.
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// Product/packaging notes that are not derived from the pin.
    #[serde(default)]
    pub packaging: PackagingSection,
}

fn default_schema() -> u32 {
    1
}

/// The pinned upstream checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSection {
    /// Upstream repository URL.
    pub repository: String,
    /// Upstream release tag, e.g. `dsh-v0.2.2-rc.5`.
    pub tag: String,
    /// Version string that the product mirrors exactly.
    pub version: String,
    /// Full 40-character commit SHA the submodule points at.
    pub commit: String,
    /// Path of the submodule inside this repository.
    #[serde(default = "default_submodule_path")]
    pub submodule: String,
    /// Official release/build instructions the packaging scripts follow.
    #[serde(default)]
    pub build: Option<RuntimeBuild>,
}

fn default_submodule_path() -> String {
    "runtime/upstream".to_owned()
}

/// How the upstream runtime is built when packaging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBuild {
    /// Runtime version upstream reports for itself (package.json).
    #[serde(default)]
    pub package_manager: Option<String>,
    /// Minimum Node.js version upstream declares.
    #[serde(default)]
    pub node: Option<String>,
    /// Commands that produce a runnable runtime from the submodule.
    #[serde(default)]
    pub commands: Vec<String>,
}

/// Non-pin packaging metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackagingSection {
    /// Relative path of the built runtime entry once `commands` have run.
    #[serde(default)]
    pub runtime_entry: Option<String>,
    /// Free-form notes rendered in reports.
    #[serde(default)]
    pub notes: Vec<String>,
}

impl RuntimeLock {
    /// Read and parse a lock file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LockError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| LockError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text).map_err(|source| LockError::AtPath {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
    }

    /// Parse lock-file text.
    pub fn parse(text: &str) -> Result<Self, LockError> {
        toml::from_str(text).map_err(LockError::Parse)
    }

    /// Serialize back to TOML (used by the sync pipeline).
    pub fn to_toml(&self) -> Result<String, LockError> {
        toml::to_string_pretty(self).map_err(LockError::Serialize)
    }

    /// The pinned tag as a comparable release.
    pub fn release(&self) -> Result<ReleaseVersion, VersionError> {
        ReleaseVersion::from_tag(&self.runtime.tag)
    }

    /// Product version, which must equal the upstream version it bundles.
    pub fn product_version(&self) -> &str {
        &self.runtime.version
    }

    /// Verify every internal consistency rule the lock file must satisfy.
    ///
    /// Checks (all must hold):
    /// 1. `tag` is `dsh-v` + valid semver and equals `version`.
    /// 2. `commit` is a full 40-character lowercase hex SHA.
    /// 3. `repository` looks like a GitHub URL for the expected upstream.
    /// 4. `submodule` equals the constant path this repository uses.
    pub fn verify(&self) -> Result<(), LockError> {
        let release = self.release().map_err(LockError::Version)?;
        if release.product_version() != self.runtime.version {
            return Err(LockError::VersionMismatch {
                tag: self.runtime.tag.clone(),
                version: self.runtime.version.clone(),
            });
        }
        if !is_full_sha(&self.runtime.commit) {
            return Err(LockError::BadCommit(self.runtime.commit.clone()));
        }
        if !self
            .runtime
            .repository
            .contains("deepseek-ai/deepseek-harness")
        {
            return Err(LockError::UnexpectedRepository(
                self.runtime.repository.clone(),
            ));
        }
        if self.runtime.submodule != default_submodule_path() {
            return Err(LockError::UnexpectedSubmodule(
                self.runtime.submodule.clone(),
            ));
        }
        Ok(())
    }

    /// Classify `candidate` against this pin.
    pub fn compare(&self, candidate: &ReleaseVersion) -> Result<Compatibility, LockError> {
        let current = self.release().map_err(LockError::Version)?;
        Ok(Compatibility::classify(&current, candidate))
    }
}

pub(crate) fn is_full_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Lock-file failures.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The file could not be read.
    #[error("cannot read runtime lock {path}: {source}")]
    Read {
        /// File that failed to load.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// TOML decoding/encoding failed.
    #[error("invalid runtime lock TOML: {0}")]
    Parse(toml::de::Error),
    /// TOML encoding failed.
    #[error("cannot encode runtime lock TOML: {0}")]
    Serialize(toml::ser::Error),
    /// Failure at a known path, with context.
    #[error("invalid runtime lock {path}: {source}")]
    AtPath {
        /// File that failed to load.
        path: std::path::PathBuf,
        /// Inner failure.
        source: Box<LockError>,
    },
    /// Tag or version strings were not valid releases.
    #[error(transparent)]
    Version(#[from] VersionError),
    /// `tag` and `version` disagreed.
    #[error("lock tag {tag} contradicts version {version}")]
    VersionMismatch {
        /// Tag from the file.
        tag: String,
        /// Version from the file.
        version: String,
    },
    /// Commit was not a full lowercase SHA.
    #[error("commit {0:?} is not a full 40-character hex SHA")]
    BadCommit(String),
    /// The repository was not the expected upstream.
    #[error("unexpected upstream repository {0:?}")]
    UnexpectedRepository(String),
    /// The submodule path was not the one this repo uses.
    #[error("unexpected submodule path {0:?}")]
    UnexpectedSubmodule(String),
}

/// The lock file shipped with this checkout, for tests and startup banners.
pub const SAMPLE_LOCK: &str = r#"
schema = 1

[runtime]
repository = "https://github.com/deepseek-ai/deepseek-harness"
tag = "dsh-v0.2.2-rc.5"
version = "0.2.2-rc.5"
commit = "9c2670d3bce5275e0d1610272958121af87787bb"
submodule = "runtime/upstream"

[runtime.build]
package_manager = "pnpm@11.28.5"
node = ">=24.9.0"
commands = ["pnpm install --frozen-lockfile", "pnpm run build"]

[packaging]
runtime_entry = "apps/cli/lib/bin.js"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_lock_is_valid() {
        let lock = RuntimeLock::parse(SAMPLE_LOCK).expect("parse");
        lock.verify().expect("verify");
        assert_eq!(lock.product_version(), "0.2.2-rc.5");
        assert_eq!(lock.release().unwrap().tag, "dsh-v0.2.2-rc.5");
    }

    #[test]
    fn round_trips_through_toml() {
        let lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        let text = lock.to_toml().unwrap();
        let again = RuntimeLock::parse(&text).unwrap();
        assert_eq!(lock, again);
    }

    #[test]
    fn missing_sections_are_reported() {
        let err = RuntimeLock::parse("[runtime]\ntag = \"dsh-v0.2.2-rc.5\"\n").unwrap_err();
        assert!(matches!(err, LockError::Parse(_)));
    }

    #[test]
    fn rejects_short_commit() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        lock.runtime.commit = "9c2670d".into();
        let err = lock.verify().unwrap_err();
        assert!(matches!(err, LockError::BadCommit(_)));
    }

    #[test]
    fn rejects_uppercase_commit() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        lock.runtime.commit = lock.runtime.commit.to_uppercase();
        assert!(matches!(
            lock.verify().unwrap_err(),
            LockError::BadCommit(_)
        ));
    }

    #[test]
    fn rejects_tag_version_drift() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        lock.runtime.version = "0.2.2-rc.4".into();
        let err = lock.verify().unwrap_err();
        assert!(matches!(err, LockError::VersionMismatch { .. }));
        assert!(err.to_string().contains("contradicts"));
    }

    #[test]
    fn rejects_foreign_repository_and_submodule() {
        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        lock.runtime.repository = "https://github.com/someone/else".into();
        assert!(matches!(
            lock.verify().unwrap_err(),
            LockError::UnexpectedRepository(_)
        ));

        let mut lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        lock.runtime.submodule = "vendor/dsh".into();
        assert!(matches!(
            lock.verify().unwrap_err(),
            LockError::UnexpectedSubmodule(_)
        ));
    }

    #[test]
    fn compare_uses_semver_precedence() {
        let lock = RuntimeLock::parse(SAMPLE_LOCK).unwrap();
        let older = ReleaseVersion::from_tag("dsh-v0.2.1").unwrap();
        let same = ReleaseVersion::from_tag("dsh-v0.2.2-rc.5").unwrap();
        let stable = ReleaseVersion::from_tag("dsh-v0.2.2").unwrap();
        assert_eq!(lock.compare(&older).unwrap(), Compatibility::Older);
        assert_eq!(lock.compare(&same).unwrap(), Compatibility::Same);
        assert_eq!(lock.compare(&stable).unwrap(), Compatibility::Newer);
    }
}
