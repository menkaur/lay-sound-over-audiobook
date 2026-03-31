//! Chapter detection — extracts chapter metadata from audio files
//! (especially m4b audiobooks) using ffprobe.

use std::path::Path;
use std::process::Command;

/// A single chapter with title and time range.
#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    pub start: f64,
    pub end: f64,
}

/// Extract chapters from an audio file via `ffprobe -show_chapters`.
/// Returns an empty vec if the file has no chapters or on any error.
pub fn get_chapters(path: &Path) -> Vec<Chapter> {
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_chapters"])
        .arg(path)
        .output();

    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };

    let json: serde_json::Value = match serde_json::from_slice(&out.stdout) {
        Ok(j) => j,
        Err(_) => return vec![],
    };

    let chapters = match json["chapters"].as_array() {
        Some(c) => c,
        None => return vec![],
    };

    chapters
        .iter()
        .enumerate()
        .filter_map(|(i, ch)| {
            let start: f64 = ch["start_time"].as_str()?.parse().ok()?;
            let end: f64 = ch["end_time"].as_str()?.parse().ok()?;
            let title = ch["tags"]["title"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("Chapter {:02}", i + 1));
            Some(Chapter { title, start, end })
        })
        .collect()
}
