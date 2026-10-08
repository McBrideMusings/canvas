//! `canvas instructions`: prints the instructions (or, with `--reminders`, the
//! post reminders) a session in a directory reads, composed from the same
//! file layers the hooks read (`canvas_core::instructions`); sets the Include
//! flag for Canvas's built-in layer; lists the projects canvasd has recorded.
//! Run on purpose, so failures are loud: one stderr line, non-zero exit.

use std::path::PathBuf;

use canvas_core::instructions::{self, Kind, Overrides};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Print {
        kind: Kind,
        cwd: Option<PathBuf>,
        json: bool,
    },
    Include {
        kind: Kind,
        on: bool,
    },
    Projects,
}

pub struct UsageError;

pub fn parse_args(args: &[String]) -> Result<Command, UsageError> {
    let mut kind = Kind::Instructions;
    let mut cwd = None;
    let mut json = false;
    let mut include = None;
    let mut projects = false;
    let mut iter = args.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        match arg {
            "--reminders" => kind = Kind::Reminders,
            "--json" => json = true,
            "--cwd" => cwd = Some(PathBuf::from(iter.next().ok_or(UsageError)?)),
            "include" if include.is_none() && !projects => {
                include = match iter.next() {
                    Some("on") => Some(true),
                    Some("off") => Some(false),
                    _ => return Err(UsageError),
                }
            }
            "projects" if include.is_none() && !projects => projects = true,
            _ => return Err(UsageError),
        }
    }
    match (include, projects) {
        (Some(on), false) if cwd.is_none() && !json => Ok(Command::Include { kind, on }),
        (None, true) if cwd.is_none() && !json && kind == Kind::Instructions => {
            Ok(Command::Projects)
        }
        (None, false) => Ok(Command::Print { kind, cwd, json }),
        _ => Err(UsageError),
    }
}

pub fn run(command: Command) -> Result<(), String> {
    let data_dir = canvas_core::paths::data_dir();
    match command {
        Command::Print { kind, cwd, json } => {
            let cwd = match cwd {
                Some(dir) => Some(dir),
                None => std::env::current_dir().ok(),
            };
            let composed = instructions::compose(
                kind,
                data_dir.as_deref(),
                cwd.as_deref(),
                &Overrides::default(),
            );
            if kind == Kind::Reminders {
                if let Err(why) = instructions::check_reminders(&composed) {
                    canvas_core::log::warn(
                        "reminders fell back to the built-in",
                        &[("error", &why)],
                    );
                    eprintln!("{why}; hooks use the built-in reminders instead");
                }
            }
            if json {
                let out =
                    serde_json::to_string_pretty(&composed.layers).map_err(|e| e.to_string())?;
                println!("{out}");
            } else {
                print!("{}", composed.text);
            }
            Ok(())
        }
        Command::Include { kind, on } => {
            let dir = data_dir.ok_or("no HOME: cannot place the data folder")?;
            let flags = instructions::set_include(&dir, kind, on).map_err(|e| {
                canvas_core::log::warn(
                    "instructions include refused",
                    &[("kind", &kind.name()), ("error", &e)],
                );
                format!("cannot set the Include flag: {e}")
            })?;
            canvas_core::log::info(
                "instructions include set",
                &[("kind", &kind.name()), ("include", &on)],
            );
            println!(
                "{}",
                serde_json::to_string(&flags).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Command::Projects => {
            let projects = data_dir
                .as_deref()
                .map(instructions::seen_projects)
                .unwrap_or_default();
            println!(
                "{}",
                serde_json::to_string_pretty(&projects).map_err(|e| e.to_string())?
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Command> {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse_args(&args).ok()
    }

    #[test]
    fn parses_each_form() {
        assert_eq!(
            parse(&["--reminders", "--cwd", "/r", "--json"]),
            Some(Command::Print {
                kind: Kind::Reminders,
                cwd: Some("/r".into()),
                json: true
            })
        );
        assert_eq!(
            parse(&["include", "off", "--reminders"]),
            Some(Command::Include {
                kind: Kind::Reminders,
                on: false
            })
        );
        assert_eq!(parse(&["projects"]), Some(Command::Projects));
        assert_eq!(parse(&["include"]), None);
        assert_eq!(parse(&["include", "on", "--json"]), None);
        assert_eq!(parse(&["projects", "include", "on"]), None);
        assert_eq!(parse(&["--cwd"]), None);
        assert_eq!(parse(&["show"]), None);
    }
}
