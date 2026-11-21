//! Choosing the upstream release to pin next.
//!
//! GitHub retags its "latest release" pointer without regard to prereleases, so
//! the sync workflow cannot trust `gh release view` alone. This module turns the
//! raw release list into a decision: highest semver tag, never a draft, never a
//! tag we cannot parse, and never something that is not strictly newer than the
//! current pin.

use serde::{Deserialize, Serialize};

use crate::version::{Compatibility, ReleaseVersion};

/// One entry of `GET /repos/{owner}/{repo}/releases`, trimmed to what matters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    /// Release tag, e.g. `dsh-v0.2.2-rc.5`.
    #[serde(rename = "tag_name")]
    pub tag_name: String,
    /// GitHub flags prereleases here.
    #[serde(default)]
    pub prerelease: bool,
    /// Drafts are invisible to users and must never be pinned.
    #[serde(default)]
    pub draft: bool,
    /// Publication timestamp, informational only.
    #[serde(default, rename = "published_at")]
    pub published_at: Option<String>,
    /// Release page, used in the pull request body.
    #[serde(default, rename = "html_url")]
    pub html_url: Option<String>,
}

/// Outcome of scanning the release list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Nothing newer was published; the pin stays put.
    UpToDate {
        /// Highest release seen, for logging.
        highest: String,
    },
    /// A strictly newer release exists.
    Newer {
        /// Parsed release to pin.
        release: ReleaseVersion,
        /// Whether upstream marked it a prerelease.
        prerelease: bool,
        /// Release page.
        html_url: Option<String>,
    },
}

/// Highest release in `candidates`, ignoring drafts, odd tags and downgrades.
///
/// Returns [`Selection::UpToDate`] together with the highest tag seen so the
/// workflow can log why it did nothing. An empty or fully unparsable list also
/// reports `UpToDate`, because refusing to move is always the safe answer.
pub fn select_newer(candidates: &[ReleaseCandidate], current: &ReleaseVersion) -> Selection {
    let parsed: Vec<(&ReleaseCandidate, ReleaseVersion)> = candidates
        .iter()
        .filter(|candidate| !candidate.draft)
        .filter_map(|candidate| {
            ReleaseVersion::from_tag(&candidate.tag_name)
                .ok()
                .map(|release| (candidate, release))
        })
        .collect();

    let highest = parsed
        .iter()
        .max_by(|left, right| left.1.cmp(&right.1))
        .map(|(_, release)| release.clone());

    let Some(highest) = highest else {
        return Selection::UpToDate {
            highest: String::new(),
        };
    };

    match Compatibility::classify(current, &highest) {
        Compatibility::Newer => {
            let winner = parsed
                .iter()
                .find(|(_, release)| *release == highest)
                .map(|(candidate, _)| *candidate);
            let (prerelease, html_url) = match winner {
                Some(candidate) => (candidate.prerelease, candidate.html_url.clone()),
                None => (false, None),
            };
            Selection::Newer {
                release: highest,
                prerelease,
                html_url,
            }
        }
        Compatibility::Same | Compatibility::Older => Selection::UpToDate {
            highest: highest.tag,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(tag: &str) -> ReleaseCandidate {
        ReleaseCandidate {
            tag_name: tag.to_owned(),
            prerelease: tag.contains("rc") || tag.contains("alpha"),
            draft: false,
            published_at: None,
            html_url: Some(format!("https://example.invalid/{tag}")),
        }
    }

    fn current(tag: &str) -> ReleaseVersion {
        ReleaseVersion::from_tag(tag).unwrap()
    }

    #[test]
    fn picks_the_newest_release() {
        let list = vec![
            candidate("dsh-v0.2.1-rc.5"),
            candidate("dsh-v0.2.1"),
            candidate("dsh-v0.2.2-rc.5"),
            candidate("dsh-v0.2.2-rc.4"),
        ];
        match select_newer(&list, &current("dsh-v0.2.2-rc.5")) {
            Selection::UpToDate { highest } => assert_eq!(highest, "dsh-v0.2.2-rc.5"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn offers_a_stable_after_its_prereleases() {
        let list = vec![candidate("dsh-v0.2.2-rc.5"), candidate("dsh-v0.2.2")];
        match select_newer(&list, &current("dsh-v0.2.2-rc.5")) {
            Selection::Newer {
                release,
                prerelease,
                ..
            } => {
                assert_eq!(release.tag, "dsh-v0.2.2");
                assert!(!prerelease);
            }
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn refuses_to_downgrade() {
        let list = vec![candidate("dsh-v0.2.1-rc.5"), candidate("dsh-v0.2.1")];
        match select_newer(&list, &current("dsh-v0.2.2-rc.5")) {
            Selection::UpToDate { highest } => assert_eq!(highest, "dsh-v0.2.1"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn drafts_and_unparsable_tags_are_ignored() {
        let mut draft = candidate("dsh-v9.9.9");
        draft.draft = true;
        let list = vec![draft, candidate("nightly"), candidate("dsh-v0.2.2-rc.6")];
        match select_newer(&list, &current("dsh-v0.2.2-rc.5")) {
            Selection::Newer { release, .. } => assert_eq!(release.tag, "dsh-v0.2.2-rc.6"),
            other => panic!("unexpected selection: {other:?}"),
        }
    }

    #[test]
    fn empty_release_list_stays_put() {
        assert_eq!(
            select_newer(&[], &current("dsh-v0.2.2-rc.5")),
            Selection::UpToDate {
                highest: String::new()
            }
        );
    }

    #[test]
    fn release_list_json_matches_the_github_shape() {
        let json = r#"[{"tag_name":"dsh-v0.2.2-rc.5","prerelease":true,"draft":false,"published_at":"2026-11-18T10:00:00Z","html_url":"https://example.invalid/rel"}]"#;
        let list: Vec<ReleaseCandidate> = serde_json::from_str(json).expect("parse");
        assert_eq!(list[0].tag_name, "dsh-v0.2.2-rc.5");
        assert!(list[0].prerelease);
        assert_eq!(
            list[0].html_url.as_deref(),
            Some("https://example.invalid/rel")
        );
    }
}
