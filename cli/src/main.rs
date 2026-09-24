fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        Some("hook") => {
            let event = match args.get(2) {
                Some(e) => e,
                None => {
                    eprintln!("usage: canvas hook <session-start|session-end|stop>");
                    std::process::exit(2);
                }
            };
            // A hook must never slow or break a Claude session: every
            // failure — bad stdin, canvasd unreachable, a malformed
            // transcript — is swallowed and the process exits 0 silently.
            let _ = canvas::hook::run(event);
            std::process::exit(0);
        }
        _ => {
            eprintln!("usage: canvas hook <session-start|session-end|stop>");
            std::process::exit(2);
        }
    }
}
