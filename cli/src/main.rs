const USAGE: &str =
    "usage: canvas hook <session-start|session-end|prompt> [--agent name] | canvas post [file|-] [--format md|text|html] [--update <card_id>] [--focus] | canvas artifact <new [--title t] [--link <dir|file.html>] [--widget <file>] [--refresh <cmd> [--every <secs>] [--always]] [--focus] | put <id> <file|dir> [--widget <file>] [--refresh <cmd> [--every <secs>] [--always] | --refresh off] [--focus] | put <id> --refresh off | relink <id> <dir|file.html> | list | show <id> | delete <id> | log <id> | reset <id> | state <id> [<key>|--clear] | pane [<id> --size WxH|--reset|--full|--exit]> | canvas card <card_id> | canvas export <card_id> [-o file] | canvas export --all [--session <id>] [-o file.zip] | canvas focus <card_id|artifact_id> | canvas snapshot <card_id|artifact_id> <out.png> | canvas record --window <app> <out.mp4|out.webp> [--seconds N] | canvas theme [light|dark|system] | canvas data <card_id|artifact_id> [file|-] | canvas instructions [--reminders] [--cwd <dir>] [--json] | canvas instructions include on|off [--reminders] | canvas instructions projects | canvas integrations <list [--json] | install <agent> [repo]> | canvas logs --path | canvas daemon [install]";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let serving = args.get(1).map(String::as_str) == Some("daemon") && args.len() == 2;
    if !serving {
        canvas_core::log::init(canvas_core::log::Process::Cli);
    }

    match args.get(1).map(String::as_str) {
        Some("daemon") => match args.get(2).map(String::as_str) {
            None => canvas::daemon::run(),
            Some("install") if args.len() == 3 => match canvas::daemon::install() {
                Ok(report) => println!("{report}"),
                Err(e) => {
                    eprintln!("canvas daemon install: {e}");
                    canvas_core::log::error("daemon install failed", &[("error", &e)]);
                    std::process::exit(1);
                }
            },
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        },
        Some("instructions") => {
            let Ok(command) = canvas::instructions::parse_args(&args[2..]) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if let Err(e) = canvas::instructions::run(command) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("logs") => {
            if args.get(2).map(String::as_str) != Some("--path") {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
            let Some(dir) = canvas_core::log::logs_dir() else {
                eprintln!("no HOME: cannot place the log folder");
                std::process::exit(1);
            };
            // Best effort: an unwritable data dir still gets the path printed.
            let _ = std::fs::create_dir_all(&dir);
            println!("{}", dir.display());
        }
        Some("integrations") => {
            if let Err(e) = canvas::integrations::run(&args[2..]) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("hook") => {
            let event = match args.get(2) {
                Some(e) => e,
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            // A hook must never slow or break a Claude session: every
            // failure — bad stdin, canvasd unreachable, an unrecognised
            // event — is swallowed and the process exits 0 silently.
            let adapter = match args.get(3).map(String::as_str) {
                Some("--agent") => args.get(4).and_then(|n| canvas::agent::by_name(n)),
                None => Some(canvas::agent::default_adapter()),
                Some(_) => None,
            };
            let agent = match args.get(3).map(String::as_str) {
                Some("--agent") => args.get(4).map_or("", String::as_str),
                None => "claude-code",
                Some(other) => other,
            };
            match adapter.map(|a| canvas::hook::run(event, a)) {
                Some(run) => {
                    let mut fields: Vec<(&str, &dyn std::fmt::Display)> =
                        vec![("event", &event), ("agent", &agent)];
                    let extra = run
                        .fields
                        .iter()
                        .map(|(k, v)| (*k, v as &dyn std::fmt::Display));
                    match run.output {
                        Ok(output) => {
                            let outcome = if output.is_some() {
                                "printed"
                            } else {
                                "silent"
                            };
                            fields.push(("outcome", &outcome));
                            fields.extend(extra);
                            canvas_core::log::info("hook", &fields);
                            if let Some(text) = output {
                                print!("{text}");
                            }
                        }
                        Err(e) => {
                            fields.push(("error", &e));
                            fields.extend(extra);
                            canvas_core::log::warn("hook failed", &fields);
                        }
                    }
                }
                None => canvas_core::log::warn(
                    "hook skipped: unknown agent",
                    &[("event", event), ("agent", &agent)],
                ),
            }
            std::process::exit(0);
        }
        Some("post") => {
            // Unlike a hook, `canvas post` is run on purpose by an agent
            // that needs to see whether it worked, so failures are loud:
            // one line on stderr, non-zero exit.
            let rest = &args[2..];
            let parsed = match canvas::post::parse_args(rest) {
                Ok(parsed) => parsed,
                Err(canvas::post::UsageError(flag)) => {
                    if let Some(flag) = flag {
                        eprintln!("{flag}");
                    }
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = canvas::post::run(parsed) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("artifact") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let Ok(command) = canvas::artifact::parse_args(&args[2..]) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if let Err(e) = canvas::artifact::run(command) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("card") => {
            // Prints the card the way canvasd holds it: one JSON object
            // with its id, session, time, html, images and targets.
            let card_id = match args.get(2) {
                Some(id) => id,
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            match canvas::client::get_card(card_id) {
                Ok(card) => match serde_json::to_string(&card) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                },
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("export") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let parsed = match canvas::export::parse_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("{e}");
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = canvas::export::run(parsed) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("focus") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let (Some(card_id), None) = (args.get(2), args.get(3)) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if let Err(e) = canvas::focus::run(card_id) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("theme") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let theme = match (args.get(2).map(String::as_str), args.get(3)) {
                (None, _) => None,
                (Some(t @ ("light" | "dark" | "system")), None) => Some(t),
                _ => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = canvas::theme::run(theme) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("snapshot") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let (Some(card_id), Some(out), None) = (args.get(2), args.get(3), args.get(4)) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if let Err(e) = canvas::snapshot::run(card_id, out) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("record") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let parsed = match canvas::record::parse_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(canvas::record::ArgError::Usage) => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
                Err(canvas::record::ArgError::Bad(e)) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
            if let Err(e) = canvas::record::run(parsed) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Some("data") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let (Some(id), file, None) = (args.get(2), args.get(3), args.get(4)) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if id.starts_with("--") {
                eprintln!("unknown flag {id}");
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
            if let Err(e) = canvas::data::run(id, file.map(String::as_str)) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
