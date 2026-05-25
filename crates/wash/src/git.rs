//! Shared git helpers.
//!
//! Currently exposes a parser for GitHub `owner/repo` slugs out of a `git remote` URL
//! and a convenience that combines it with shelling out to `git config`. Used by the
//! GhPR tool to recover the owner/repo segment when the agent omits the `repo` arg,
//! so `gh api repos/.../pulls/N/comments` works the same way `gh pr <op>` does today.

use std::process::Command;

/// Parse a git remote URL into a `owner/repo` slug, restricted to GitHub hosts.
///
/// Accepts the common forms emitted by `git config --get remote.origin.url`:
/// - `git@github.com:owner/repo.git`
/// - `git@github.com:owner/repo`
/// - `https://github.com/owner/repo.git`
/// - `https://github.com/owner/repo`
/// - `ssh://git@github.com/owner/repo.git`
///
/// Returns `None` for non-GitHub hosts, malformed strings, or anything we can't safely
/// canonicalize into two non-empty path segments. Conservative on purpose — the caller
/// only feeds the result back into a `gh api` URL, so a wrong guess is worse than a
/// `None` that surfaces a clear error.
pub fn parse_github_owner_repo(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // Strip protocol/host and reduce to the "owner/repo[.git]" tail.
    let tail = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = url.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("git://github.com/") {
        rest
    } else {
        return None;
    };

    let tail = tail.strip_suffix('/').unwrap_or(tail);
    let tail = tail.strip_suffix(".git").unwrap_or(tail);

    let mut parts = tail.splitn(2, '/');
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    // Reject anything with additional path segments — `owner/repo/extra` is not a slug.
    if repo.contains('/') {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// Best-effort `owner/repo` resolution from `cwd`'s `remote.origin.url`. Returns
/// `None` if `git` isn't on PATH, the command fails, or the URL doesn't parse as a
/// GitHub slug.
pub fn github_owner_repo_from_cwd(cwd: &std::path::Path) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_github_owner_repo(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssh_scp_with_dot_git() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:foo/bar.git").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn parses_ssh_scp_without_dot_git() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:foo/bar").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn parses_https_with_dot_git() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/foo/bar.git").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn parses_https_without_dot_git() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/foo/bar").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn parses_ssh_protocol() {
        assert_eq!(
            parse_github_owner_repo("ssh://git@github.com/foo/bar.git").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn parses_trailing_slash() {
        assert_eq!(
            parse_github_owner_repo("https://github.com/foo/bar/").as_deref(),
            Some("foo/bar")
        );
    }

    #[test]
    fn rejects_non_github_host() {
        assert!(parse_github_owner_repo("https://gitlab.com/foo/bar.git").is_none());
        assert!(parse_github_owner_repo("git@gitlab.com:foo/bar.git").is_none());
        assert!(parse_github_owner_repo("https://bitbucket.org/foo/bar").is_none());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_github_owner_repo("").is_none());
        assert!(parse_github_owner_repo("   ").is_none());
        assert!(parse_github_owner_repo("not a url").is_none());
        assert!(parse_github_owner_repo("https://github.com/").is_none());
        assert!(parse_github_owner_repo("https://github.com/foo").is_none());
        assert!(parse_github_owner_repo("git@github.com:foo").is_none());
        assert!(parse_github_owner_repo("git@github.com:/bar").is_none());
        assert!(parse_github_owner_repo("https://github.com/foo/").is_none());
    }

    #[test]
    fn rejects_extra_path_segments() {
        // `gh api repos/foo/bar/extra/pulls/...` would 404 — keep slugs strict.
        assert!(parse_github_owner_repo("https://github.com/foo/bar/baz").is_none());
        assert!(parse_github_owner_repo("https://github.com/foo/bar/baz.git").is_none());
    }
}
