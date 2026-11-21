//! Version ordering for upstream DSH releases.
//!
//! Upstream tags look like `dsh-v0.2.2-rc.5`. Ordering matters because the
//! sync pipeline must never "upgrade" to an older release: `rc.5` is newer than
//! `rc.4`, but `0.2.1` (stable) is *older* than `0.2.2-rc.1` (prerelease), and
//! semver already encodes exactly that relation.

use std::cmp::Ordering;
use std::fmt;

use semver::Version;

/// Tag prefix used by every upstream DSH release.
pub const TAG_PREFIX: &str = "dsh-v";

/// A released upstream DSH version parsed from a git tag or a bare version string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion {
    /// Semantic version without the `dsh-v` prefix.
    pub version: Version,
    /// Original upstream tag, e.g. `dsh-v0.2.2-rc.5`.
    pub tag: String,
}

impl ReleaseVersion {
    /// Parse an upstream tag such as `dsh-v0.2.2-rc.5`.
    pub fn from_tag(tag: &str) -> Result<Self, VersionError> {
        let raw = tag
            .strip_prefix(TAG_PREFIX)
            .ok_or_else(|| VersionError::BadTag(tag.to_owned()))?;
        let version = Version::parse(raw).map_err(|source| VersionError::BadSemver {
            input: raw.to_owned(),
            source,
        })?;
        Ok(Self {
            version,
            tag: tag.to_owned(),
        })
    }

    /// Build the canonical upstream tag for a version.
    pub fn tag_for(version: &Version) -> String {
        format!("{TAG_PREFIX}{version}")
    }

    /// The version string the product uses (identical to the upstream version).
    pub fn product_version(&self) -> String {
        self.version.to_string()
    }

    /// Whether this release is a stable release (no prerelease identifiers).
    pub fn is_stable(&self) -> bool {
        self.version.pre.is_empty()
    }

    /// Compare two releases using semver precedence.
    ///
    /// Prerelease identifiers sort below the matching release, so
    /// `0.2.2-rc.5 < 0.2.2`, while `0.2.1 < 0.2.2-rc.1`.
    pub fn is_newer_than(&self, other: &Self) -> bool {
        self.version > other.version
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.version.cmp(&other.version)
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.tag)
    }
}

/// Result of comparing the pinned runtime version with a candidate release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    /// Candidate is the version already pinned.
    Same,
    /// Candidate is newer and may be proposed by the sync pipeline.
    Newer,
    /// Candidate is older; the pipeline must refuse to downgrade.
    Older,
}

impl Compatibility {
    /// Classify `candidate` against the `current` pin.
    pub fn classify(current: &ReleaseVersion, candidate: &ReleaseVersion) -> Self {
        match candidate.version.cmp(&current.version) {
            Ordering::Equal => Self::Same,
            Ordering::Greater => Self::Newer,
            Ordering::Less => Self::Older,
        }
    }
}

/// Errors produced while parsing or comparing versions.
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    /// The tag did not start with `dsh-v`.
    #[error("upstream tag {0:?} does not start with {TAG_PREFIX:?}")]
    BadTag(String),
    /// The remaining string was not valid semver.
    #[error("{input:?} is not a semantic version: {source}")]
    BadSemver {
        /// Offending input text.
        input: String,
        /// Underlying parser error.
        source: semver::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tag: &str) -> ReleaseVersion {
        ReleaseVersion::from_tag(tag).expect("valid tag")
    }

    #[test]
    fn parses_prefixed_tag() {
        let parsed = v("dsh-v0.2.2-rc.5");
        assert_eq!(parsed.product_version(), "0.2.2-rc.5");
        assert!(!parsed.is_stable());
        assert_eq!(ReleaseVersion::tag_for(&parsed.version), "dsh-v0.2.2-rc.5");
    }

    #[test]
    fn rejects_missing_prefix() {
        let err = ReleaseVersion::from_tag("v0.2.2-rc.5").unwrap_err();
        assert!(matches!(err, VersionError::BadTag(_)));
        assert!(err.to_string().contains("does not start with"));
    }

    #[test]
    fn rejects_garbage_semver() {
        let err = ReleaseVersion::from_tag("dsh-vnot-a-version").unwrap_err();
        assert!(matches!(err, VersionError::BadSemver { .. }));
    }

    #[test]
    fn rc_numbers_are_ordered_numerically() {
        assert!(v("dsh-v0.2.2-rc.10") > v("dsh-v0.2.2-rc.9"));
        assert!(v("dsh-v0.2.2-rc.5") > v("dsh-v0.2.2-rc.4"));
    }

    #[test]
    fn stable_release_beats_its_own_prereleases() {
        assert!(v("dsh-v0.2.2") > v("dsh-v0.2.2-rc.5"));
        assert!(v("dsh-v0.2.2-alpha.3") < v("dsh-v0.2.2-rc.1"));
    }

    #[test]
    fn next_minor_prerelease_beats_previous_stable() {
        assert!(v("dsh-v0.2.2-rc.1") > v("dsh-v0.2.1"));
    }

    #[test]
    fn classify_detects_downgrades() {
        let current = v("dsh-v0.2.2-rc.5");
        assert_eq!(
            Compatibility::classify(&current, &v("dsh-v0.2.2-rc.5")),
            Compatibility::Same
        );
        assert_eq!(
            Compatibility::classify(&current, &v("dsh-v0.2.2-rc.4")),
            Compatibility::Older
        );
        assert_eq!(
            Compatibility::classify(&current, &v("dsh-v0.2.2")),
            Compatibility::Newer
        );
        assert_eq!(
            Compatibility::classify(&current, &v("dsh-v0.2.1")),
            Compatibility::Older
        );
    }
}
