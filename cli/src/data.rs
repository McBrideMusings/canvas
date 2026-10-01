//! `canvas data <card_id> [file|-]` and `canvas data --slot <slot> [file|-]`:
//! pushes one JSON value into a card's running script. The viewer delivers it
//! as a `canvas-data` message without rebuilding the card, so a dashboard keeps
//! its scroll, inputs and chart state between pushes. A slot names the card
//! pinned there in the caller's repo.

use crate::{client, pin, post};

/// Which card receives the value.
pub enum Target<'a> {
    Card(&'a str),
    Slot(&'a str),
}

pub fn run(target: Target, arg: Option<&str>) -> Result<(), String> {
    let text = post::read_html(arg, std::io::stdin())?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("canvas data needs one JSON value: {e}"))?;
    match target {
        Target::Card(id) => client::push_data(id, &value),
        Target::Slot(slot) => client::push_data(&pin::card_in_slot(slot)?.id, &value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_an_error() {
        let err = run(Target::Card("c1"), Some("/nonexistent/data.json")).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
    }
}
