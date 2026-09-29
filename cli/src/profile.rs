//! `canvas profile` — read and change named profiles (today only
//! `posting-guidance`) from the shell, globally or for one repo.
//!
//! Like `canvas post`, these verbs are run on purpose by a person or agent
//! who needs to know whether they worked: every failure is one line on
//! stderr and a non-zero exit. Everything here uses the blocking socket
//! client; no tokio runtime is started.

use std::io::Read;

use crate::client;

const DEFAULT_KIND: &str = canvasd::profiles::KIND_POSTING_GUIDANCE;

/// A malformed `canvas profile` argument list; the caller prints the usage
/// line and exits 2.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError;

/// Which assignment a verb acts on.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Global,
    Repo(String),
    /// The GitHub repo of the current working directory.
    Here,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    List,
    Show { name: String },
    ShowEffective { repo: Option<String> },
    Set { name: String, source: String },
    Delete { name: String },
    Assign { name: String, target: Target },
    Unassign { target: Target },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub kind: String,
    pub command: Command,
}

/// Parses everything after `canvas profile`. Flags may appear anywhere.
pub fn parse_args(rest: &[String]) -> Result<Parsed, UsageError> {
    let mut kind: Option<&str> = None;
    let mut repo: Option<&str> = None;
    let (mut here, mut effective) = (false, false);
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--kind" | "--repo" => {
                let slot = if rest[i] == "--kind" {
                    &mut kind
                } else {
                    &mut repo
                };
                if slot.is_some() {
                    return Err(UsageError);
                }
                *slot = Some(rest.get(i + 1).ok_or(UsageError)?.as_str());
                i += 2;
            }
            "--here" => {
                here = true;
                i += 1;
            }
            "--effective" => {
                effective = true;
                i += 1;
            }
            // A lone "-" is stdin for `set`, not a flag.
            other if other.starts_with("--") => return Err(UsageError),
            other => {
                positional.push(other);
                i += 1;
            }
        }
    }
    if here && repo.is_some() {
        return Err(UsageError);
    }
    let target = match (repo, here) {
        (Some(repo), _) => Target::Repo(repo.to_string()),
        (None, true) => Target::Here,
        (None, false) => Target::Global,
    };
    let scoped = repo.is_some() || here;
    let verb = positional.first().copied().ok_or(UsageError)?;
    let args = &positional[1..];
    let owned = |s: &str| s.to_string();
    // Flags a verb doesn't take are a usage error, not silently ignored.
    let command = match (verb, args) {
        ("list", []) if !scoped && !effective => Command::List,
        ("show", [name]) if !scoped && !effective => Command::Show { name: owned(name) },
        ("show", []) if effective && !here => Command::ShowEffective {
            repo: repo.map(owned),
        },
        ("set", [name, source]) if !scoped && !effective => Command::Set {
            name: owned(name),
            source: owned(source),
        },
        ("delete", [name]) if !scoped && !effective => Command::Delete { name: owned(name) },
        ("assign", [name]) if !effective => Command::Assign {
            name: owned(name),
            target,
        },
        ("unassign", []) if !effective => Command::Unassign { target },
        _ => return Err(UsageError),
    };
    Ok(Parsed {
        kind: kind.unwrap_or(DEFAULT_KIND).to_string(),
        command,
    })
}

/// `owner/repo` of the current directory, resolved by the daemon's own
/// resolver so the key written here is the key the hook reads with `?cwd=`.
fn here() -> Result<String, String> {
    let cwd = current_dir()?;
    canvasd::repo::github_repo_blocking(&cwd)
        .ok_or_else(|| format!("{cwd} is not in a git checkout with a github.com origin"))
}

fn current_dir() -> Result<String, String> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("could not read the current directory: {e}"))
}

/// The repo an assignment applies to, or `None` for the global one.
fn resolve_target(target: Target) -> Result<Option<String>, String> {
    match target {
        Target::Global => Ok(None),
        Target::Repo(repo) => Ok(Some(repo)),
        Target::Here => here().map(Some),
    }
}

fn read_text(source: &str, mut stdin: impl Read) -> Result<String, String> {
    let mut text = String::new();
    if source == "-" {
        stdin
            .read_to_string(&mut text)
            .map_err(|e| format!("could not read stdin: {e}"))?;
    } else {
        text =
            std::fs::read_to_string(source).map_err(|e| format!("could not read {source}: {e}"))?;
    }
    if text.trim().is_empty() {
        return Err("profile text is empty".to_string());
    }
    Ok(text)
}

fn unknown(name: &str) -> String {
    format!("no profile named {name:?}")
}

pub fn run(parsed: Parsed) -> Result<(), String> {
    let kind = parsed.kind.as_str();
    match parsed.command {
        Command::List => {
            let state = client::get_profiles(kind)?;
            let mut names: Vec<&String> = state.profiles.keys().collect();
            names.sort();
            for name in names {
                let mut marks = String::new();
                if state.global.as_deref() == Some(name.as_str()) {
                    marks.push_str("  (global)");
                }
                println!("{name}{marks}");
            }
            let mut repos: Vec<(&String, &String)> = state.repos.iter().collect();
            repos.sort();
            for (repo, name) in repos {
                println!("repo {repo} -> {name}");
            }
            if state.builtin.is_some() {
                println!("built-in default: present");
            }
        }
        Command::Show { name } => {
            let state = client::get_profiles(kind)?;
            let text = state.profiles.get(&name).ok_or_else(|| unknown(&name))?;
            print!("{text}");
        }
        Command::ShowEffective { repo } => {
            let effective = client::get_effective_profile(kind, repo.as_deref(), &current_dir()?)?;
            let text = match effective.text {
                Some(text) => text,
                None => canvasd::profiles::builtin_default(kind)
                    .ok_or_else(|| format!("no {kind} profile is assigned"))?
                    .to_string(),
            };
            print!("{text}");
        }
        Command::Set { name, source } => {
            let text = read_text(&source, std::io::stdin())?;
            client::set_profile_text(kind, &name, Some(text))?;
            println!("set {kind} profile {name}");
        }
        Command::Delete { name } => {
            let state = client::get_profiles(kind)?;
            if !state.profiles.contains_key(&name) {
                return Err(unknown(&name));
            }
            client::set_profile_text(kind, &name, None)?;
            println!("deleted {kind} profile {name}");
        }
        Command::Assign { name, target } => {
            let repo = resolve_target(target)?;
            client::assign_profile(kind, repo.as_deref(), Some(&name))?;
            match repo {
                Some(repo) => println!("assigned {name} to {repo}"),
                None => println!("assigned {name} globally"),
            }
        }
        Command::Unassign { target } => {
            let repo = resolve_target(target)?;
            client::assign_profile(kind, repo.as_deref(), None)?;
            match repo {
                Some(repo) => println!("cleared the assignment for {repo}"),
                None => println!("cleared the global assignment"),
            }
        }
    }
    Ok(())
}
