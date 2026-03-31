//! Duration cache — persists audio file durations to JSON so that
//! re-runs skip expensive ffprobe calls for unchanged files.
//!
//! Cache entries are keyed by absolute path and validated against
//! (mtime, file_size) to detect changes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// On-disk cache format.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct DurationCache {
    pub entries: HashMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub duration: f64,
    pub modified: u64,
    pub size: u64,
}

/// Load cache from a JSON file, returning an empty cache on any error.
pub fn load(path: &Path) -> DurationCache {
    path.exists()
        .then(|| {
            fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .flatten()
        .unwrap_or_default()
}

/// Save cache to a JSON file. Errors are silently ignored.
pub fn save(cache: &DurationCache, path: &Path) {
    if let Ok(data) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(path, data);
    }
}

/// Return `(mtime_secs, file_size)` for cache validation.
fn file_metadata_key(path: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(path).ok()?;
    let mt = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mt, m.len()))
}

/// Get duration via ffprobe, using the shared cache.
///
/// - Fast path: returns cached value if `(mtime, size)` match.
/// - Slow path: runs `ffprobe`, parses JSON output, updates cache.
/// - The cache lock is **never** held during the ffprobe call.
pub fn get_duration(path: &Path, cache: &Arc<Mutex<DurationCache>>) -> Option<f64> {
    let key = path.to_string_lossy().to_string();
    let (modified, size) = file_metadata_key(path)?;

    // Fast path
    {
        let c = cache.lock().unwrap();
        if let Some(e) = c.entries.get(&key) {
            if e.modified == modified && e.size == size {
                return Some(e.duration);
            }
        }
    }

    // Slow path — lock is NOT held here
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_format"])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let duration: f64 = json["format"]["duration"].as_str()?.parse().ok()?;

    // Update cache
    {
        let mut c = cache.lock().unwrap();
        c.entries.insert(
            key,
            CacheEntry {
                duration,
                modified,
                size,
            },
        );
    }
    Some(duration)
}
