//! Which GitHub repo a session's working directory belongs to.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// `owner/repo` of `cwd`'s `origin` remote, or `None` when `cwd` isn't in a
/// git checkout, has no `origin`, or `origin` isn't on github.com. A linked
/// worktree reports its main checkout's `origin`, so it resolves to the same
/// repo. Gives up after 1s rather than hold up the post.
pub fn github_repo(cwd: &str) -> Option<String> {
    let mut child = Command::new("git")
        .args(origin_url_args(cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_github_remote(out.trim())
}

/// The git arguments that print `cwd`'s `origin` URL.
fn origin_url_args(cwd: &str) -> [&str; 5] {
    ["-C", cwd, "remote", "get-url", "origin"]
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
    use super::{github_repo, parse_github_remote};

    #[test]
    fn reads_origin_from_a_checkout_and_its_linked_worktree() {
        let root = std::env::temp_dir().join(format!("canvas-repo-{}", std::process::id()));
        let main = root.join("main");
        let linked = root.join("linked");
        std::fs::create_dir_all(&main).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&main)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:octo/hello.git"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);
        git(&["worktree", "add", "-q", linked.to_str().unwrap()]);

        assert_eq!(
            github_repo(main.to_str().unwrap()).as_deref(),
            Some("octo/hello")
        );
        assert_eq!(
            github_repo(linked.to_str().unwrap()).as_deref(),
            Some("octo/hello")
        );
        assert_eq!(github_repo("/"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

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
