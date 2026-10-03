//! `canvas focus <card_id>`: every open viewer clears the search and filters
//! hiding the card, scrolls to it and rings it, the way a `canvas-post://`
//! link does. The window is never raised. Prints `{"viewers": N}`; a focus
//! no viewer received is an error, since nobody saw it.

use crate::client;

/// The stderr line when no viewer was connected to receive the focus.
pub const NO_VIEWER: &str = "no Canvas viewer is open to show the card";

pub fn run(card_id: &str) -> Result<(), String> {
    let viewers = client::focus_card(card_id)?;
    if viewers == 0 {
        return Err(NO_VIEWER.to_string());
    }
    println!("{}", serde_json::json!({ "viewers": viewers }));
    Ok(())
}
