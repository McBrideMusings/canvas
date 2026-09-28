const USAGE: &str =
    "usage: canvas hook <session-start|session-end> | canvas post [file|-] [--format md|text|html] [--update <card_id>] | canvas guidance | canvas install [repo] | canvas daemon";

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
                .and_then(|cwd| canvas::client::fetch_guidance(&cwd))
                .and_then(|g| g.effective().map(str::to_string))
                .unwrap_or_else(|| canvas::guidance::TEXT.to_string());
            print!("{text}");
        }
        Some("install") => {
            if let Err(e) = canvas::install::run(args.get(2).map(String::as_str)) {
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
            if let Ok(Some(guidance)) = canvas::hook::run(event) {
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
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
