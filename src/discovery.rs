//! File discovery — recursively finds audio files and provides
//! path manipulation helpers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Recognised audio file extensions.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "flac", "aac", "m4a", "m4b", "wma", "opus", "webm",
];

/// Check if a path has a recognised audio extension.
pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Recursively collect audio files with **natural** sort order.
/// Used for input files where playback order matters.
pub fn collect_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .map(|e| e.into_path())
        .collect();
    v.sort_by(|a, b| natord::compare(&a.to_string_lossy(), &b.to_string_lossy()));
    v
}

/// Recursively collect audio files (unsorted).
/// Used for music files where order doesn't matter (they get shuffled).
pub fn collect_unsorted(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .map(|e| e.into_path())
        .collect()
}

/// Build a path that appends `ext` *after* the full original filename,
/// preventing collisions when two source files share a stem but have
/// different extensions.
///
/// Example: `track.mp3` → `track.mp3.wav`, `track.flac` → `track.flac.wav`
pub fn append_extension(base_dir: &Path, rel: &Path, ext: &str) -> PathBuf {
    let parent = rel.parent().unwrap_or(Path::new(""));
    let mut fname: OsString = rel.file_name().unwrap_or_default().to_os_string();
    fname.push(".");
    fname.push(ext);
    base_dir.join(parent).join(fname)
}

/// Derive the default output directory from the input path.
///
/// Examples:
/// - `recordings/` → `recordings_processed/`
/// - `/data/episodes` → `/data/episodes_processed/`
pub fn default_output_dir(input: &Path) -> PathBuf {
    let mut name = input
        .file_name()
        .unwrap_or(input.as_os_str())
        .to_os_string();
    name.push("_processed");
    match input.parent() {
        Some(parent) if parent != Path::new("") => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Sanitize a string for use in a filename by replacing unsafe characters.
pub fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}
