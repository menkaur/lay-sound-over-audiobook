//! Image copying and cover art extraction.
//!
//! Copies image files from the input directory tree to the output
//! directory tree, preserving relative paths.  Also extracts embedded
//! cover art from audio files and places `cover.jpg` in each output
//! subdirectory.

use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::cover;

/// Image extensions to copy from source directories.
const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "webp", "tiff", "tif", "svg",
];

/// Check if a path has an image extension.
fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Copy all image files from the input directory tree to the output
/// directory tree, preserving relative paths.  Also extracts embedded
/// cover art from audio files and places `cover.jpg` in each output
/// subdirectory.
///
/// `cover_dirs` maps output subdirectory → source audio file (for
/// cover extraction).
///
/// Skips images that already exist in the output (resume-friendly).
pub fn copy_images_and_extract_covers(
    input_dir: &Path,
    output_dir: &Path,
    cover_dirs: &HashMap<PathBuf, PathBuf>,
    dry_run: bool,
) {
    if dry_run {
        println!("🖼️   [DRY RUN] Would copy images and extract covers\n");
        return;
    }

    println!("🖼️   Copying images and extracting covers...");

    let mut copied: usize = 0;

    // 1. Copy all image files from input directory tree
    for entry in walkdir::WalkDir::new(input_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_image(e.path()))
    {
        let src = entry.path();
        let rel = src.strip_prefix(input_dir).unwrap_or(src);
        let dst = output_dir.join(rel);

        if dst.exists() {
            copied += 1;
            continue;
        }

        if let Some(p) = dst.parent() {
            let _ = fs::create_dir_all(p);
        }

        if fs::copy(src, &dst).is_ok() {
            copied += 1;
        }
    }

    // 2. Extract embedded cover art into each unique output subdirectory
    let ext_count = AtomicUsize::new(0);
    let pairs: Vec<(&PathBuf, &PathBuf)> = cover_dirs.iter().collect();
    pairs.par_iter().for_each(|(out_dir, source)| {
        if cover::extract_cover_image(source, out_dir) {
            ext_count.fetch_add(1, Ordering::Relaxed);
        }
    });
    let extracted = ext_count.load(Ordering::Relaxed);

    println!(
        "    {} image(s) copied, {} cover(s) extracted\n",
        copied, extracted
    );
}
