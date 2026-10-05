const USAGE: &str =
    "usage: canvas hook <session-start|session-end|prompt> [--agent name] | canvas post [file|-] [--format md|text|html] [--update <card_id>] [--focus] | canvas artifact <new [--title t] [--link <dir|file.html>] [--widget <file>] [--refresh <cmd> [--every <secs>]] [--focus] | put <id> <file|dir> [--widget <file>] [--refresh <cmd> [--every <secs>]] [--focus] | relink <id> <dir|file.html> | list | show <id> | delete <id> | log <id> | pane [<id> --size WxH|--reset|--full|--exit]> | canvas card <card_id> | canvas export <card_id> [-o file] | canvas focus <card_id|artifact_id> | canvas snapshot <card_id|artifact_id> <out.png> | canvas theme [light|dark] | canvas data <card_id|artifact_id> [file|-] | canvas wait <card_id> [--timeout secs] | canvas replies <card_id> | canvas profile <list|show|set|delete|assign|unassign> [--kind k] [--repo owner/name | --here] | canvas guidance | canvas integrations <list [--json] | install <agent> [repo]> | canvas logs --path | canvas daemon";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("daemon") {
        canvas_core::log::init(canvas_core::log::Process::Cli);
    }

    match args.get(1).map(String::as_str) {
        Some("daemon") => {
            canvas::daemon::run();
        }
        Some("guidance") => {
            let text = std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
                .and_then(|cwd| {
                    canvas::client::fetch_profile_text(
                        canvasd::profiles::KIND_POSTING_GUIDANCE,
                        &cwd,
                    )
                })
                .unwrap_or_else(|| canvas::guidance::TEXT.to_string());
            print!("{text}");
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
                Some(Ok(output)) => {
                    let outcome = if output.is_some() {
                        "printed"
                    } else {
                        "silent"
                    };
                    canvas_core::log::info(
                        "hook",
                        &[("event", event), ("agent", &agent), ("outcome", &outcome)],
                    );
                    if let Some(text) = output {
                        print!("{text}");
                    }
                }
                Some(Err(e)) => canvas_core::log::warn(
                    "hook failed",
                    &[("event", event), ("agent", &agent), ("error", &e)],
                ),
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
        Some("profile") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let parsed = match canvas::profile::parse_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(canvas::profile::UsageError) => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = canvas::profile::run(parsed) {
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
                (Some(t @ ("light" | "dark")), None) => Some(t),
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
        Some("wait") => {
            // canvas-17z: blocks until the named card's iframe has posted a
            // reply, or the timeout elapses. See client::wait_for_reply.
            let card_id = match args.get(2) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            let timeout_secs = args
                .iter()
                .position(|a| a == "--timeout")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);
            match canvas::client::wait_for_reply(
                &card_id,
                std::time::Duration::from_secs(timeout_secs),
            ) {
                Ok(value) => {
                    println!("{value}");
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("replies") => {
            // canvas-17z: one non-blocking check, for an agent that's doing
            // other work and checking back rather than sitting in `wait`.
            let card_id = match args.get(2) {
                Some(id) => id.clone(),
                None => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            // Exit 1 means "not yet" — keep polling. Exit 3 means canvasd
            // itself failed, which a caller retrying on exit code alone
            // needs to tell apart from "no reply yet" or it retries forever
            // against a daemon that isn't there.
            match canvas::client::get_reply(&card_id) {
                Ok(Some(value)) => {
                    println!("{value}");
                }
                Ok(None) => {
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(3);
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
