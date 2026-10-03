//! `canvas theme [light|dark]`: with a theme, every open viewer switches to it
//! the way a click on its title-bar button does, so the choice persists in the
//! viewer's localStorage like a click's. Prints `{"viewers": N}`; a set no
//! viewer received is an error, since nothing changed. With no argument it
//! prints `{"theme": "light"|"dark"}`, the theme a viewer last reported
//! showing, and fails when no viewer is open or none has reported one.

use crate::client;

/// The stderr line when no viewer was connected to receive the theme.
pub const NO_VIEWER: &str = "no Canvas viewer is open to change theme";

/// The stderr line when no open viewer has reported a theme.
pub const NOT_REPORTED: &str = "no open Canvas viewer has reported its theme";

pub fn run(theme: Option<&str>) -> Result<(), String> {
    let Some(theme) = theme else {
        let current = client::viewer_theme()?.ok_or_else(|| NOT_REPORTED.to_string())?;
        println!("{}", serde_json::json!({ "theme": current }));
        return Ok(());
    };
    let viewers = client::set_theme(theme)?;
    if viewers == 0 {
        return Err(NO_VIEWER.to_string());
    }
    println!("{}", serde_json::json!({ "viewers": viewers }));
    Ok(())
}
