const USAGE: &str =
    "usage: canvas hook <session-start|session-end> | canvas post [file|-] [--format md|text|html] | canvas daemon";

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("daemon") => {
            canvas::daemon::run();
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
            let _ = canvas::hook::run(event);
            std::process::exit(0);
        }
        Some("post") => {
            // Unlike a hook, `canvas post` is run on purpose by an agent
            // that needs to see whether it worked, so failures are loud:
            // one line on stderr, non-zero exit.
            let rest = &args[2..];
            let mut arg: Option<&str> = None;
            let mut format_flag: Option<&str> = None;
            let mut i = 0;
            let mut usage_error = false;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--format" => match rest.get(i + 1) {
                        Some(v) => {
                            format_flag = Some(v.as_str());
                            i += 2;
                        }
                        None => {
                            usage_error = true;
                            break;
                        }
                    },
                    other if arg.is_none() => {
                        arg = Some(other);
                        i += 1;
                    }
                    _ => {
                        usage_error = true;
                        break;
                    }
                }
            }
            if usage_error {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
            if let Err(e) = canvas::post::run(arg, format_flag) {
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
