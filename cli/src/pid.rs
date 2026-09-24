//! Finds the Claude process a hook invocation belongs to.
//!
//! Claude Code runs a hook command through a shell (`sh -c "<command>"`), so
//! this process's immediate parent (`getppid`) is that shell, not `claude`
//! itself. Worse, an agent can wrap that command in another non-shell
//! process — `timeout 5 canvas post`, or a `python3 -c 'subprocess.run(...)'`
//! — which sits between the shell and `claude` in the ancestry. We walk up
//! the process tree from our own pid looking for an ancestor whose command
//! name looks like `claude`, and only fall back to the first non-shell
//! ancestor (the old best guess) if no such ancestor turns up within
//! `MAX_HOPS` — a broken or unusually deep ancestry still returns something
//! rather than nothing. Standard library Rust has no `getppid`/`ps`
//! wrapper, so each step shells out to `ps`, which is always present on
//! macOS.

const MAX_HOPS: usize = 8;
const SHELL_NAMES: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

pub fn claude_pid() -> Option<u32> {
    let mut pid = std::process::id();
    let mut fallback: Option<u32> = None;

    for _ in 0..MAX_HOPS {
        // A `ps` failure this far up the tree doesn't erase a fallback
        // already found on an earlier hop — stop the walk and return
        // whatever we have rather than discarding it.
        let Some(ppid) = parent_of(pid) else {
            break;
        };
        if ppid == 0 {
            break;
        }
        let comm = command_name(ppid).unwrap_or_default();
        let base = base_name(&comm);

        if base.contains("claude") {
            return Some(ppid);
        }
        if !SHELL_NAMES.contains(&base) && fallback.is_none() {
            // Not a shell and not obviously claude — remember it as a best
            // guess, but keep walking in case a real `claude` ancestor is
            // further up (e.g. this is `timeout`, `env`, or a `python3`
            // wrapper sitting between the shell and `claude`).
            fallback = Some(ppid);
        }
        pid = ppid;
    }

    fallback
}

/// `ps -o comm=` prints the path with a trailing newline, and a login shell
/// shows as `-zsh`; reduce both to the bare command name.
fn base_name(comm: &str) -> &str {
    let comm = comm.trim();
    let base = comm.rsplit('/').next().unwrap_or(comm);
    base.trim_start_matches('-')
}

fn parent_of(pid: u32) -> Option<u32> {
    run_ps(&["-o", "ppid=", "-p", &pid.to_string()])?
        .trim()
        .parse()
        .ok()
}

fn command_name(pid: u32) -> Option<String> {
    run_ps(&["-o", "comm=", "-p", &pid.to_string()])
}

fn run_ps(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("ps").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps_output_reduces_to_a_matchable_shell_name() {
        assert_eq!(base_name("/bin/zsh\n"), "zsh");
        assert_eq!(base_name("bash\n"), "bash");
        assert_eq!(base_name("-zsh\n"), "zsh");
        assert_eq!(base_name("claude\n"), "claude");
    }
}
