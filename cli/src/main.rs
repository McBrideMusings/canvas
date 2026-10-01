const USAGE: &str =
    "usage: canvas hook <session-start|session-end|prompt> [--agent name] | canvas post [file|-] [--format md|text|html] [--update <card_id>] | canvas card <card_id> | canvas data <card_id> [file|-] | canvas wait <card_id> [--timeout secs] | canvas replies <card_id> | canvas profile <list|show|set|delete|assign|unassign> [--kind k] [--repo owner/name | --here] | canvas guidance | canvas integrations <list [--json] | install <agent> [repo]> | canvas daemon";

fn main() {
    let args: Vec<String> = std::env::args().collect();

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
            if let Some(Ok(Some(guidance))) = adapter.map(|a| canvas::hook::run(event, a)) {
                print!("{guidance}");
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
                Err(canvas::post::UsageError) => {
                    eprintln!("{USAGE}");
                    std::process::exit(2);
                }
            };
            if let Err(e) = canvas::post::run(parsed.arg, parsed.format_flag, parsed.update_id) {
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
        Some("data") => {
            // Run on purpose, so failures are loud like `canvas post`.
            let Some(card_id) = args.get(2) else {
                eprintln!("{USAGE}");
                std::process::exit(2);
            };
            if let Err(e) = canvas::data::run(card_id, args.get(3).map(String::as_str)) {
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
