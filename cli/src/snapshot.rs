//! `canvas snapshot <card_id|artifact_id> <out.png>`: asks the open viewer to
//! capture the card as it renders it, in the active theme, or for an `art-`
//! id the artifact's pane as it shows it, and writes that PNG to the path.
//! Prints `{"path": "...", "width": N, "height": N, "clipped": bool}`, the
//! size in pixels; `clipped` (with a stderr note) when the card is taller than
//! the window and the PNG stops at its bottom edge. No viewer open, or a card
//! or artifact the viewer isn't showing (filtered out, archived, pinned, the
//! other page, another artifact open), is an error naming why.

use std::path::Path;

use crate::client;

pub fn run(id: &str, out: &str) -> Result<(), String> {
    let client::Snapshot { png, clipped } = client::snapshot(id)?;
    let (width, height) =
        png_size(&png).ok_or("the viewer sent back something that is not a PNG")?;
    let path = std::path::absolute(Path::new(out)).map_err(|e| format!("{out}: {e}"))?;
    std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
    let shown = path.display().to_string();
    canvas_core::log::info(
        "snapshot written",
        &[
            ("id", &id),
            ("path", &shown),
            ("bytes", &png.len()),
            ("width", &width),
            ("height", &height),
            ("clipped", &clipped),
        ],
    );
    if clipped {
        eprintln!("the card is taller than the window; the PNG stops at the window's bottom edge");
    }
    println!(
        "{}",
        serde_json::json!({ "path": shown, "width": width, "height": height, "clipped": clipped })
    );
    Ok(())
}

/// Width and height from a PNG's IHDR chunk, which always comes first.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.len() < 24 || &png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::png_size;

    #[test]
    fn reads_the_size_from_the_ihdr_chunk() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&846u32.to_be_bytes());
        png.extend_from_slice(&312u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((846, 312)));
        assert_eq!(png_size(b"not a png at all, just text"), None);
    }
}
