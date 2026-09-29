//! Which GitHub repo a session's working directory belongs to.

use std::process::Stdio;
use std::time::Duration;

/// `owner/repo` of `cwd`'s `origin` remote, or `None` when `cwd` isn't in a
/// git checkout, has no `origin`, or `origin` isn't on github.com. A linked
/// worktree reports its main checkout's `origin`, so it resolves to the same
/// repo. Gives up after 1s rather than hold up the request.
pub async fn github_repo(cwd: &str) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::process::Command::new("git")
            .args(origin_url_args(cwd))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_remote(String::from_utf8_lossy(&output.stdout).trim())
}

/// The git arguments that print `cwd`'s `origin` URL. Shared by the async and
/// blocking resolvers so a repo key never depends on which one produced it.
fn origin_url_args(cwd: &str) -> [&str; 5] {
    ["-C", cwd, "remote", "get-url", "origin"]
}

/// `github_repo` for callers with no async runtime (the CLI never starts
/// one outside `canvas daemon`). Same git call, same parsing, so the key
/// `canvas profile --here` writes is the key the daemon reads for `?cwd=`.
pub fn github_repo_blocking(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(origin_url_args(cwd))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_github_remote(String::from_utf8_lossy(&output.stdout).trim())
}

/// `owner/repo` from a github.com remote URL in any of the forms git accepts:
/// `git@github.com:o/r.git`, `ssh://git@github.com/o/r.git`,
/// `https://github.com/o/r(.git)`.
pub fn parse_github_remote(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::parse_github_remote;

    #[test]
    fn parses_every_github_url_form() {
        for url in [
            "git@github.com:octo/hello.git",
            "ssh://git@github.com/octo/hello.git",
            "https://github.com/octo/hello",
            "https://github.com/octo/hello.git",
            "https://github.com/octo/hello/",
        ] {
            assert_eq!(
                parse_github_remote(url).as_deref(),
                Some("octo/hello"),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_other_hosts_and_malformed_paths() {
        for url in [
            "git@gitlab.com:octo/hello.git",
            "https://example.com/octo/hello",
            "https://github.com/octo",
            "https://github.com/octo/hello/tree/main",
            "/Users/me/repos/hello.git",
        ] {
            assert_eq!(parse_github_remote(url), None, "{url}");
        }
    }
}
