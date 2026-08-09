//! Recognising a forge repository in a remote URL.
//!
//! Hand rolled rather than reached for a URL crate, because the shape Git uses
//! most — `git@github.com:owner/repo.git` — is not a URL at all. It has no
//! scheme and its colon separates a path rather than a port, so a conforming
//! parser rejects it and every caller would need the hand-written branch
//! anyway.
//!
//! **Remote URLs come from repositories that may have been cloned from
//! anywhere.** An owner or repository name read from one ends up interpolated
//! into an API request, so anything that is not plainly a name is refused here
//! rather than sanitised further down. Nothing is guessed: an unrecognised
//! shape returns `None`.

use crate::model::RepoRef;

/// Characters a forge allows in an owner or repository name.
///
/// Deliberately narrower than any provider's real rule. Being strict costs at
/// worst an unrecognised remote, which degrades to "no pull requests for this
/// repository"; being loose puts untrusted text into a request.
fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// A name that is safe to put in a request, and is not a path traversal.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name != "."
        && name != ".."
        && name.chars().all(is_name_char)
}

/// A host that is plausibly a hostname.
///
/// Ports and user information have already been stripped by the time this runs.
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.chars().all(|c| is_name_char(c) && c != '.'))
}

/// Reads `host`, `owner` and `name` out of a remote URL.
///
/// Provider-agnostic: it says where a remote points, not who hosts it. Deciding
/// whether a host is GitHub is `GitHub::detect`'s job, because a self-hosted
/// instance lives on an arbitrary domain and only configuration knows which.
///
/// Recognised shapes:
///
/// - `https://github.com/owner/repo.git` — and `http`, `git`, `ssh`
/// - `https://user@github.com/owner/repo`
/// - `ssh://git@github.com:22/owner/repo.git`
/// - `git@github.com:owner/repo.git` — the scp-like form
///
/// A path that is not exactly two segments returns `None`. That is not a
/// limitation to work around later: a GitLab subgroup path is three or more
/// segments, and quietly reading its last two would name a repository that does
/// not exist.
pub fn parse_remote_url(url: &str) -> Option<RepoRef> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    let (host, path) = match split_scheme(url) {
        Some(rest) => split_authority(rest, '/')?,
        // No scheme, so this is either the scp-like form or a local path. A
        // local path has no `:` before its first `/`, which is what tells the
        // two apart — and `C:\src\repo` is a local path, not a host called `C`.
        None => split_authority(url, ':')?,
    };

    let host = strip_userinfo(host);
    let host = strip_port(host)?;
    if !valid_host(host) {
        return None;
    }

    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');

    let mut segments = path.split('/');
    let owner = segments.next()?;
    let name = segments.next()?;
    if segments.next().is_some() {
        return None;
    }

    if !valid_name(owner) || !valid_name(name) {
        return None;
    }

    Some(RepoRef {
        host: host.to_ascii_lowercase(),
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// Strips a `scheme://` prefix, returning what follows it.
fn split_scheme(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    matches!(scheme, "https" | "http" | "ssh" | "git").then_some(rest)
}

/// Splits an authority from a path at the first `separator`.
///
/// The separator differs by shape: a URL path starts at `/`, and the scp-like
/// form separates its path with `:`.
fn split_authority(rest: &str, separator: char) -> Option<(&str, &str)> {
    let at = rest.find(separator)?;
    let (host, path) = rest.split_at(at);
    Some((host, &path[separator.len_utf8()..]))
}

/// Drops `user@` or `user:password@`.
///
/// A password in a remote URL is somebody's credential; it is dropped here and
/// never reaches a `RepoRef`, which is logged and shown.
fn strip_userinfo(host: &str) -> &str {
    host.rsplit_once('@').map_or(host, |(_, after)| after)
}

/// Drops a `:port` suffix, refusing one that is not a number.
///
/// The refusal matters for the scp-like form: `git@host:owner/repo` has already
/// been split at its colon, so anything left here that looks like a port but is
/// not a number means the URL was not the shape it appeared to be.
fn strip_port(host: &str) -> Option<&str> {
    match host.rsplit_once(':') {
        Some((before, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            Some(before)
        }
        Some(_) => None,
        None => Some(host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(url: &str) -> (String, String, String) {
        let repo = parse_remote_url(url).unwrap_or_else(|| panic!("{url} should parse"));
        (repo.host, repo.owner, repo.name)
    }

    #[test]
    fn every_shape_git_writes_a_remote_in_reaches_the_same_repository() {
        let expected = (
            "github.com".to_owned(),
            "youhide".to_owned(),
            "hideGit".to_owned(),
        );

        for url in [
            "https://github.com/youhide/hideGit.git",
            "https://github.com/youhide/hideGit",
            "https://github.com/youhide/hideGit/",
            "http://github.com/youhide/hideGit.git",
            "git://github.com/youhide/hideGit.git",
            "ssh://git@github.com/youhide/hideGit.git",
            "ssh://git@github.com:22/youhide/hideGit.git",
            "git@github.com:youhide/hideGit.git",
            "git@github.com:youhide/hideGit",
            "  https://github.com/youhide/hideGit.git  ",
        ] {
            assert_eq!(parsed(url), expected, "{url}");
        }
    }

    #[test]
    fn a_password_in_the_url_is_dropped_rather_than_carried() {
        // A RepoRef is logged and displayed. A credential that reached one would
        // be a credential in the log.
        let repo = parse_remote_url("https://someone:hunter2@github.com/youhide/hideGit").unwrap();
        assert_eq!(repo.host, "github.com");
        assert_eq!(repo.owner, "youhide");
    }

    #[test]
    fn the_host_is_kept_because_enterprise_lives_anywhere() {
        let repo = parse_remote_url("https://github.example.com/team/thing.git").unwrap();
        assert_eq!(repo.host, "github.example.com");
        assert_eq!(repo.owner, "team");
        assert_eq!(repo.name, "thing");
    }

    #[test]
    fn a_host_is_compared_case_insensitively_but_a_name_is_not() {
        let repo = parse_remote_url("https://GitHub.COM/youhide/hideGit.git").unwrap();
        assert_eq!(repo.host, "github.com", "hosts are case-insensitive");
        assert_eq!(repo.name, "hideGit", "repository names are not");
    }

    #[test]
    fn a_path_that_is_not_exactly_owner_and_repository_is_refused() {
        // A GitLab subgroup is three segments. Reading the last two would name a
        // repository that does not exist, which is worse than declining.
        for url in [
            "https://gitlab.com/group/subgroup/thing.git",
            "https://github.com/youhide",
            "https://github.com/",
            "https://github.com",
        ] {
            assert!(parse_remote_url(url).is_none(), "{url} should not parse");
        }
    }

    #[test]
    fn a_local_path_is_not_a_remote_repository() {
        // Every remote in the test suite is a bare repository on a local path,
        // so this is the case the rest of hideGit hits most.
        for url in [
            "/srv/git/hideGit.git",
            "./relative/thing.git",
            "../up/thing.git",
            "file:///srv/git/hideGit.git",
            "C:\\src\\hideGit",
        ] {
            assert!(parse_remote_url(url).is_none(), "{url} should not parse");
        }
    }

    #[test]
    fn a_name_that_is_not_plainly_a_name_is_refused_rather_than_sanitised() {
        // These end up in an API request. Anything unrecognised declines; it is
        // never trimmed into something acceptable, because a URL crafted to
        // survive trimming is exactly the input that matters.
        for url in [
            "https://github.com/../etc/passwd",
            "https://github.com/owner/..",
            "https://github.com/own er/repo",
            "https://github.com/owner/re\npo",
            "https://github.com/owner/repo\") { x }",
            "https://github.com/owner/re%2Fpo",
            "https://git hub.com/owner/repo",
            "https://github.com:notaport/owner/repo",
            "",
            "   ",
        ] {
            assert!(parse_remote_url(url).is_none(), "{url:?} should not parse");
        }
    }

    #[test]
    fn a_name_ending_in_git_keeps_the_word_when_it_is_the_name() {
        // Only one `.git` suffix comes off. A repository genuinely called
        // `hideGit.git` is rare, but stripping twice would rename it.
        let repo = parse_remote_url("https://github.com/youhide/hideGit.git.git").unwrap();
        assert_eq!(repo.name, "hideGit.git");
    }

    #[test]
    fn a_repo_ref_reads_a_remotes_fetch_url() {
        let remote = hidegit_core::model::Remote {
            name: "origin".to_owned(),
            fetch_url: "git@github.com:youhide/hideGit.git".to_owned(),
            push_url: None,
        };

        let repo = RepoRef::from_remote(&remote).unwrap();
        assert_eq!(repo.to_string(), "youhide/hideGit");
    }
}
