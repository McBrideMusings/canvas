//! Finds the Claude process a hook invocation belongs to.
//!
//! Claude Code runs a hook command through a shell (`sh -c "<command>"`), so
//! this process's immediate parent (`getppid`) is that shell, not `claude`
//! itself. We walk up the process tree from our own pid, skipping any
//! ancestor whose command name looks like a shell, and return the first
//! ancestor that either looks like `claude` or isn't a shell (a best-effort
//! stop so a broken ancestry still returns something rather than looping).
//! Standard library Rust has no `getppid`/`ps` wrapper, so each step shells
//! out to `ps`, which is always present on macOS.

const MAX_HOPS: usize = 8;
const SHELL_NAMES: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

pub fn claude_pid() -> Option<u32> {
    let mut pid = std::process::id();

    for _ in 0..MAX_HOPS {
        let ppid = parent_of(pid)?;
        if ppid == 0 {
            return None;
        }
        let comm = command_name(ppid).unwrap_or_default();
        let base = comm.rsplit('/').next().unwrap_or(&comm);

        if base.contains("claude") {
            return Some(ppid);
        }
        if SHELL_NAMES.contains(&base) {
            pid = ppid;
            continue;
        }
        // Not a shell and not obviously claude — best guess, stop here.
        return Some(ppid);
    }

    None
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
