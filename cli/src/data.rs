//! `canvas data <card_id> [file|-]`: pushes one JSON value into a card's
//! running script. The viewer delivers it as a `canvas-data` message without
//! rebuilding the card, so a dashboard keeps its scroll, inputs and chart
//! state between pushes.

use crate::{client, post};

pub fn run(card_id: &str, arg: Option<&str>) -> Result<(), String> {
    let text = post::read_html(arg, std::io::stdin())?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("canvas data needs one JSON value: {e}"))?;
    client::push_data(card_id, &value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_an_error() {
        let err = run("c1", Some("/nonexistent/data.json")).unwrap_err();
        assert!(err.contains("could not read"), "{err}");
    }
}
