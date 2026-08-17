//! "There is a newer hideGit."
//!
//! One unauthenticated `GET` against the releases of one fixed repository, at
//! most once a day, and it never installs anything. Nothing about the machine
//! or what is done with it is sent — this is a read, not telemetry, which is
//! the line [ROADMAP](../../../docs/ROADMAP.md) draws and this stays on the
//! right side of.
//!
//! **It cannot use `/releases/latest`.** That route ignores pre-releases, and
//! every hideGit release is one — the binaries are unsigned, which is said
//! plainly rather than hidden behind a stable-looking tag. With nothing but
//! pre-releases the route returns 404, checked against the live repository
//! rather than assumed. So the full list is read and the newest usable entry
//! taken from it.
//!
//! On by default, and switchable. That is a deliberate call rather than a
//! drift: hideGit ships unsigned archives from GitHub Releases and no operating
//! system will ever update them, so a build with a bug in it has no other way
//! of learning there is a fix.

use serde::Deserialize;

/// Where releases are read from. `owner/name`.
pub const REPOSITORY: &str = "youhide/hideGit";

/// A release worth telling someone about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Update {
    /// The tag, as written — `v0.2.0`.
    pub tag: String,
    /// Where to read about it. Never fetched automatically, never installed.
    pub url: String,
}

/// The check did not produce an answer.
///
/// One variant rather than "unreachable" and "refused": the caller logs it and
/// carries on either way, and the two cannot be told apart reliably anyway — a
/// 403 with an empty body arrives as a deserialisation failure, not as a status.
/// Two variants nothing distinguishes are a distinction the code cannot keep.
#[derive(Debug, thiserror::Error)]
#[error("the update check did not get an answer: {0}")]
pub struct UpdateError(String);

/// One release as this needs it. Everything else in the payload is ignored.
#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
}

/// A version as three numbers, which is all hideGit's tags ever are.
///
/// Anything after them — `-rc1`, `+build` — makes the version *older* than the
/// same three numbers without it, which is what semver says and what keeps a
/// release candidate from being offered over the release it precedes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// `true` for a plain `1.2.3`. Ordered after a suffixed one because `false`
    /// sorts first, which is the whole reason it is stored this way round.
    released: bool,
}

impl Version {
    /// Parses `v1.2.3`, `1.2.3` or `1.2.3-rc1`. `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('v');
        let (numbers, suffix) = match text.find(['-', '+']) {
            Some(at) => (&text[..at], &text[at..]),
            None => (text, ""),
        };

        let mut parts = numbers.split('.');
        let mut number = || parts.next()?.parse::<u64>().ok();
        let version = Self {
            major: number()?,
            minor: number()?,
            patch: number()?,
            released: suffix.is_empty(),
        };

        // `1.2.3.4` is not a version this understands, and guessing which of
        // the four numbers to drop is worse than saying so.
        parts.next().is_none().then_some(version)
    }
}

/// How long between checks.
///
/// A day. Often enough that a fix is not missed for a week, rare enough that it
/// is not a request every time somebody opens a repository — and this happens
/// whether or not anybody is signed in, so it has to be cheap to be polite.
pub const INTERVAL: i64 = 60 * 60 * 24;

/// Whether to ask at all right now.
///
/// The rule, kept away from the request so it can be tested without one: off
/// means never, and a check that ran less than [`INTERVAL`] ago does not run
/// again. A `last` in the future — a clock that was wrong, or moved — is
/// treated as due rather than as a reason never to check again.
pub fn due(enabled: bool, last: Option<i64>, now: i64) -> bool {
    if !enabled {
        return false;
    }

    match last {
        None => true,
        Some(last) => !(last..last + INTERVAL).contains(&now),
    }
}

/// The newest release, if it is newer than `current`.
///
/// `None` is the ordinary answer: nothing is newer. Errors are for the caller
/// to log and forget — a version check that could not reach the network is not
/// worth telling anyone about, because there is nothing they can do and nothing
/// is wrong.
pub async fn check(
    api: &str,
    repository: &str,
    current: &str,
) -> Result<Option<Update>, UpdateError> {
    let releases = releases(api, repository).await?;
    Ok(newer_than(current, &releases))
}

/// Picks the newest release ahead of `current`, given the payload.
///
/// Separated from the request so the decision — which is the part with rules in
/// it — is testable without a server.
fn newer_than(current: &str, releases: &[Release]) -> Option<Update> {
    let current = Version::parse(current)?;

    releases
        .iter()
        // A draft is not published; offering one would send somebody to a page
        // they cannot see. Pre-releases are *not* filtered: every hideGit
        // release is one.
        .filter(|release| !release.draft)
        .filter_map(|release| Some((Version::parse(&release.tag_name)?, release)))
        // The list arrives newest-first, but that is GitHub's ordering by
        // creation, not by version — a patch to an older line published after a
        // newer one would come first.
        .max_by_key(|(version, _)| *version)
        .filter(|(version, _)| *version > current)
        .map(|(_, release)| Update {
            tag: release.tag_name.clone(),
            url: release.html_url.clone(),
        })
}

async fn releases(api: &str, repository: &str) -> Result<Vec<Release>, UpdateError> {
    // Unauthenticated, and built without a token store: this reads one public
    // repository and has nothing to do with whether anybody is signed in.
    // octocrab rather than a second HTTP client, and `get` with hideGit's own
    // type rather than octocrab's `Release`, which carries three dozen fields
    // this does not read and every one of them would have to be faked in a test.
    let client = octocrab::Octocrab::builder()
        .base_uri(api)
        .map_err(|error| UpdateError(error.to_string()))?
        .build()
        .map_err(|error| UpdateError(error.to_string()))?;

    // Built from constants, never from anything typed: `repository` is
    // `REPOSITORY` in every caller but the tests.
    let route = format!("/repos/{repository}/releases?per_page=20");

    client
        .get(&route, None::<&()>)
        .await
        .map_err(|error| UpdateError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool) -> Release {
        Release {
            tag_name: tag.to_owned(),
            html_url: format!("https://github.com/youhide/hideGit/releases/tag/{tag}"),
            draft,
        }
    }

    #[test]
    fn a_version_is_three_numbers_and_maybe_a_suffix() {
        assert_eq!(Version::parse("v1.2.3"), Version::parse("1.2.3"));
        assert!(Version::parse("1.2.4") > Version::parse("1.2.3"));
        assert!(Version::parse("1.3.0") > Version::parse("1.2.99"));
        assert!(Version::parse("2.0.0") > Version::parse("1.99.99"));

        // A release candidate precedes the release, so it must never be
        // offered over it.
        assert!(Version::parse("1.2.3") > Version::parse("1.2.3-rc1"));

        for nonsense in ["", "v", "1.2", "1.2.3.4", "next", "1.x.3"] {
            assert_eq!(Version::parse(nonsense), None, "“{nonsense}” parsed");
        }
    }

    #[test]
    fn a_newer_release_is_offered_and_the_current_one_is_not() {
        let releases = [release("v0.2.0", false), release("v0.1.0", false)];

        assert_eq!(
            newer_than("0.1.0", &releases).map(|update| update.tag),
            Some("v0.2.0".to_owned())
        );
        assert_eq!(newer_than("0.2.0", &releases), None, "already on it");
        assert_eq!(newer_than("0.3.0", &releases), None, "ahead of it");
    }

    #[test]
    fn a_pre_release_is_still_a_release() {
        // The reason `/releases/latest` cannot be used: it skips these, and
        // every hideGit release is one. Checked against the live repository —
        // that route answers 404 today.
        let releases = [release("v0.0.1", false)];

        assert_eq!(
            newer_than("0.0.0", &releases).map(|update| update.tag),
            Some("v0.0.1".to_owned()),
            "a pre-release was skipped, which would leave the check finding nothing at all"
        );
    }

    #[test]
    fn a_draft_is_not_offered() {
        // It is not published. Offering it sends somebody to a page that is
        // not there.
        let releases = [release("v0.9.0", true), release("v0.2.0", false)];

        assert_eq!(
            newer_than("0.1.0", &releases).map(|update| update.tag),
            Some("v0.2.0".to_owned())
        );
    }

    #[test]
    fn the_newest_version_wins_rather_than_the_most_recently_published() {
        // GitHub orders by creation. A patch to an older line published after a
        // newer release would come first in the list and must not be offered as
        // an upgrade from it.
        let releases = [release("v0.1.1", false), release("v0.2.0", false)];

        assert_eq!(
            newer_than("0.1.0", &releases).map(|update| update.tag),
            Some("v0.2.0".to_owned())
        );
    }

    #[test]
    fn a_tag_nothing_can_read_is_skipped_rather_than_taken() {
        let releases = [release("nightly", false), release("v0.2.0", false)];

        assert_eq!(
            newer_than("0.1.0", &releases).map(|update| update.tag),
            Some("v0.2.0".to_owned())
        );
    }

    #[test]
    fn a_check_runs_once_a_day_and_never_when_it_is_off() {
        assert!(due(true, None, 1_000), "never checked, so it is due");
        assert!(!due(false, None, 1_000), "off is off");

        assert!(!due(true, Some(1_000), 1_000 + INTERVAL - 1));
        assert!(due(true, Some(1_000), 1_000 + INTERVAL));

        // A clock that moved backwards would otherwise put the next check
        // years away, and the answer to "the clock is wrong" is not "stop
        // checking for updates".
        assert!(due(true, Some(9_999), 1_000), "a future timestamp is due");
    }

    #[tokio::test]
    async fn the_list_route_is_read_rather_than_the_latest_route() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/youhide/hideGit/releases"))
            .and(query_param("per_page", "20"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "tag_name": "v9.0.0",
                    "html_url": "https://example.invalid/9",
                    "draft": false,
                    "prerelease": true,
                }])),
            )
            .mount(&server)
            .await;

        let update = check(&server.uri(), "youhide/hideGit", "0.0.1")
            .await
            .expect("the server answered");

        assert_eq!(
            update,
            Some(Update {
                tag: "v9.0.0".to_owned(),
                url: "https://example.invalid/9".to_owned(),
            })
        );
    }

    #[tokio::test]
    async fn a_refusal_is_an_error_rather_than_no_update() {
        // "Nothing is newer" and "nobody answered" are different answers, and
        // reporting the second as the first hides a check that stopped working.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let outcome = check(&server.uri(), "youhide/hideGit", "0.0.1").await;

        assert!(outcome.is_err(), "got {outcome:?}");
    }
}
