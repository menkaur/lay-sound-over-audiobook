//! Cover image extraction — extracts embedded art from audio files
//! or finds common cover image files in the source directory.
//! Places `cover.jpg` in the output directory for Android audiobook
//! players (Smart AudioBook Player, Listen, etc.).

use std::fs;
use std::path::Path;
use std::process::Command;

/// Extract or find a cover image and place it as `cover.jpg` in `output_dir`.
///
/// Strategy:
/// 1. Skip if `cover.jpg` already exists (resume-friendly).
/// 2. Try extracting embedded art from the source file via ffmpeg.
/// 3. Fall back to common cover filenames in the source directory.
///
/// Returns `true` if a cover image was placed (or already existed).
pub fn extract_cover_image(source: &Path, output_dir: &Path) -> bool {
    let cover_path = output_dir.join("cover.jpg");

    if cover_path.exists() {
        return true;
    }

    if let Some(p) = cover_path.parent() {
        let _ = fs::create_dir_all(p);
    }

    // Try extracting embedded art
    let st = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(source)
        .args(["-an", "-vcodec", "mjpeg", "-frames:v", "1"])
        .arg(&cover_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if matches!(st, Ok(s) if s.success()) && cover_path.exists() {
        if fs::metadata(&cover_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            return true;
        }
        let _ = fs::remove_file(&cover_path);
    }

    // Fallback: common cover filenames
    let source_dir = source.parent().unwrap_or(Path::new("."));
    let cover_names = [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "folder.jpg",
        "folder.jpeg",
        "folder.png",
        "front.jpg",
        "front.jpeg",
        "front.png",
        "album.jpg",
        "album.jpeg",
        "album.png",
        "Cover.jpg",
        "Cover.jpeg",
        "Cover.png",
        "Folder.jpg",
        "Folder.jpeg",
        "Folder.png",
        "EmbeddedCover.jpg",
    ];

    for name in &cover_names {
        let candidate = source_dir.join(name);
        if candidate.exists() {
            if name.ends_with(".png") {
                let st = Command::new("ffmpeg")
                    .args(["-y", "-i"])
                    .arg(&candidate)
                    .arg(&cover_path)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                if matches!(st, Ok(s) if s.success()) {
                    return true;
                }
            } else if fs::copy(&candidate, &cover_path).is_ok() {
                return true;
            }
        }
    }

    false
}
