//! `canvas integrations list|install`: detect each coding agent, check that
//! its Canvas hooks match this `canvas` binary, and install or repair them.
//! The per-agent work sits behind `AgentAdapter::{detect, status, install}`;
//! like `post`, a failure prints one line on stderr and exits non-zero.

pub mod claude;
pub mod codex;

use serde_json::json;

use crate::agent::{self, AgentAdapter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Current,
    OutOfDate,
    NotInstalled,
    /// Installed and current, but the agent has not yet been told to trust
    /// the hooks; the user approves them in the agent.
    NeedsReview,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::OutOfDate => "out of date",
            Status::NotInstalled => "not installed",
            Status::NeedsReview => "needs review",
        }
    }
}

/// Whether `program` is an executable file in a `PATH` directory.
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// `args` follows `canvas integrations`.
pub fn run(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let rows: Vec<_> = agent::adapters().iter().map(|a| row(*a)).collect();
            if args.iter().any(|a| a == "--json") {
                println!("{}", serde_json::Value::Array(rows));
            } else {
                for r in &rows {
                    let status = r["status"].as_str().unwrap_or("-");
                    let error = r["error"].as_str().unwrap_or("");
                    println!(
                        "{:<12} {:<10} {status} {error}",
                        r["agent"].as_str().unwrap_or(""),
                        if r["found"] == true {
                            "found"
                        } else {
                            "not found"
                        },
                    );
                }
            }
            Ok(())
        }
        Some("install") => {
            let name = args
                .get(1)
                .ok_or("usage: canvas integrations install <agent> [repo]")?;
            let adapter = agent::by_name(name)
                .ok_or_else(|| format!("unknown agent {name}; expected claude-code or codex"))?;
            if !adapter.detect() {
                return Err(format!("{name} is not on PATH"));
            }
            adapter.install(args.get(2).map(String::as_str))
        }
        _ => Err("usage: canvas integrations <list [--json] | install <agent> [repo]>".into()),
    }
}

fn row(adapter: &dyn AgentAdapter) -> serde_json::Value {
    if !adapter.detect() {
        return json!({ "agent": adapter.name(), "found": false, "status": null, "error": null });
    }
    match adapter.status() {
        Ok(s) => {
            json!({ "agent": adapter.name(), "found": true, "status": s.label(), "error": null })
        }
        Err(e) => json!({ "agent": adapter.name(), "found": true, "status": null, "error": e }),
    }
}
