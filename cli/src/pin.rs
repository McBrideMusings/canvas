//! `canvas unpin <slot|card_id>` and the slot lookup `canvas data --slot`
//! shares. A slot is unique within the caller's repo, so both resolve it from
//! the current directory.

use canvas_core::Card;

use crate::client;

fn no_such_slot(slot: &str) -> String {
    format!("no pinned post in slot {slot:?} in this repo")
}

/// The card pinned in `slot` in the repo this shell runs in.
pub fn card_in_slot(slot: &str) -> Result<Card, String> {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| format!("could not resolve the current directory: {e}"))?;
    client::find_pinned(&cwd, slot)?.ok_or_else(|| no_such_slot(slot))
}

/// Releases a slot named by `target`, or a pinned card named by its id. The
/// card stays in the feed.
pub fn unpin(target: &str) -> Result<(), String> {
    let id = match card_in_slot(target) {
        Ok(card) => card.id,
        Err(_) => target.to_string(),
    };
    if client::unpin_card(&id)? {
        Ok(())
    } else {
        Err(no_such_slot(target))
    }
}
