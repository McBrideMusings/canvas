//! `canvas focus <card_id|artifact_id>`: for a card, every open viewer clears
//! the search and filters hiding it, scrolls to it and rings it, the way a
//! `canvas-post://` link does; for an `art-` id, every viewer switches to the
//! Artifacts page and opens that artifact. The window is never raised. Prints
//! `{"viewers": N}`; a focus no viewer received is an error, since nobody saw it.

use crate::client;

/// The stderr line when no viewer was connected to receive the focus.
pub const NO_VIEWER: &str = "no Canvas viewer is open to show the card";

/// The same, for an artifact.
pub const NO_VIEWER_ARTIFACT: &str = "no Canvas viewer is open to show the artifact";

pub fn run(id: &str) -> Result<(), String> {
    let artifact = id.starts_with(canvasd::artifacts::ID_PREFIX);
    let viewers = if artifact {
        client::focus_artifact(id)?
    } else {
        client::focus_card(id)?
    };
    if viewers == 0 {
        return Err(if artifact {
            NO_VIEWER_ARTIFACT
        } else {
            NO_VIEWER
        }
        .to_string());
    }
    println!("{}", serde_json::json!({ "viewers": viewers }));
    Ok(())
}
