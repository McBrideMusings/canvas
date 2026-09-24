use canvas::extract::{extract, Extracted, TranscriptEntry};
use std::io::Write;

fn assistant_text(texts: &[&str]) -> TranscriptEntry {
    TranscriptEntry {
        assistant_text: texts.iter().map(|s| s.to_string()).collect(),
        tool_inputs: Vec::new(),
        tool_outputs: Vec::new(),
    }
}

fn temp_file(name: &str, ext: &str) -> String {
    let dir = std::env::temp_dir().join(format!("canvas-extract-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.{ext}"));
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "x").unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn url_in_text_is_extracted_as_a_link() {
    let entries = vec![assistant_text(&[
        "See https://example.com/docs for details.",
    ])];
    let result = extract(&entries);
    assert_eq!(result.links, vec!["https://example.com/docs"]);
    assert!(result.paths.is_empty());
    assert!(result.images.is_empty());
}

#[test]
fn existing_absolute_path_in_text_is_extracted() {
    let path = temp_file("notes", "txt");
    let entries = vec![assistant_text(&[&format!("Wrote it to {path} just now.")])];
    let result = extract(&entries);
    assert_eq!(result.paths, vec![path]);
    assert!(result.links.is_empty());
    assert!(result.images.is_empty());
}

#[test]
fn screenshot_saved_by_a_tool_and_never_mentioned_is_still_an_image() {
    let path = temp_file("shot", "png");
    let entry = TranscriptEntry {
        assistant_text: vec!["Done.".to_string()],
        tool_inputs: vec![serde_json::json!({"file_path": path, "content": "binary"})],
        tool_outputs: Vec::new(),
    };
    let result = extract(&[entry]);
    assert_eq!(result.images, vec![path]);
    assert!(result.links.is_empty());
    assert!(result.paths.is_empty());
}

#[test]
fn empty_turn_extracts_nothing() {
    let result = extract(&[]);
    assert_eq!(result, Extracted::default());

    let result = extract(&[assistant_text(&[""])]);
    assert_eq!(result, Extracted::default());
}

#[test]
fn relative_or_nonexistent_paths_are_excluded() {
    let entries = vec![assistant_text(&[
        "relative/path/to/file.txt is not absolute",
        "/definitely/does/not/exist/on/this/machine.txt is absolute but missing",
    ])];
    let result = extract(&entries);
    assert!(result.paths.is_empty());
    assert!(result.images.is_empty());
}

#[test]
fn results_are_deduped() {
    let path = temp_file("dup", "txt");
    let entries = vec![
        assistant_text(&[&format!("first mention {path}"), "https://example.com"]),
        assistant_text(&[&format!("second mention {path}"), "https://example.com"]),
    ];
    let result = extract(&entries);
    assert_eq!(result.paths, vec![path]);
    assert_eq!(result.links, vec!["https://example.com"]);
}
