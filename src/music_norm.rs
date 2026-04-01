//! Music normalization — two-pass EBU R128 loudnorm for background
//! music tracks, with caching to avoid re-normalizing across runs.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::cache::{self, DurationCache};
use crate::cli::Args;
use crate::ffmpeg::{self, LoudnessTarget, NormConfig};
use crate::progress;

use crate::MIN_DURATION;

/// Absolute tolerance (seconds) when comparing a cached normalized file's
/// duration to the original source duration.
const NORM_DURATION_TOLERANCE_S: f64 = 2.0;

/// Relative tolerance (fraction) used as an alternative to the absolute check.
const NORM_DURATION_TOLERANCE_PCT: f64 = 0.05;

/// Derive the default normalized-music output directory from the music path.
///
/// `./music` → `./music_normalized`
/// `./path/to/bgm` → `./path/to/bgm_normalized`
pub fn default_normalize_music_dir(music: &Path) -> PathBuf {
    let mut name = music
        .file_name()
        .unwrap_or(music.as_os_str())
        .to_os_string();
    name.push("_normalized");
    match music.parent() {
        Some(parent) if parent != Path::new("") => parent.join(name),
        _ => PathBuf::from(name),
    }
}

/// Deterministic path for the normalized version of a music source file.
///
/// Preserves the relative directory structure from `music_dir` inside
/// `norm_dir`, replacing the extension with `.wav`.
///
/// ```text
/// music_dir  = ./music
/// source     = ./music/ambient/track1.mp3
/// norm_dir   = ./music_normalized
/// result     = ./music_normalized/ambient/track1.wav
/// ```
pub fn normalized_music_path(norm_dir: &Path, music_dir: &Path, source: &Path) -> PathBuf {
    let rel = source
        .strip_prefix(music_dir)
        .unwrap_or(source);
    norm_dir.join(rel).with_extension("wav")
}

/// Result of the music normalization phase.
pub struct MusicNormResult {
    /// Normalized (or original) music file paths.
    pub files: Vec<PathBuf>,
    /// Corresponding durations in seconds.
    pub durations: Vec<f64>,
    /// Hold the temp dir alive until we're done (if fallback was used).
    pub _tmp_dir: Option<tempfile::TempDir>,
}

/// Normalize music tracks (Phase 2).
///
/// Returns `Ok(Some(result))` when normalization was performed or cached
/// files were found, `Ok(None)` when normalization is disabled, and
/// `Err(msg)` on fatal errors.
pub fn normalize_music(
    cli: &Args,
    music_files: &[PathBuf],
    music_durs: &[f64],
    dur_cache: &Arc<Mutex<DurationCache>>,
    cancelled: &Arc<AtomicBool>,
    mp: &indicatif::MultiProgress,
) -> Result<Option<MusicNormResult>, String> {
    if !cli.normalize_music {
        return Ok(None);
    }

    if cli.dry_run {
        println!(
            "🔊  [DRY RUN] Would normalize {} music tracks\n",
            music_files.len()
        );
        return Ok(None);
    }

    println!("🔊  Normalizing music tracks (two-pass loudnorm)...");

    let target = LoudnessTarget {
        i: cli.loudness_i,
        tp: cli.loudness_tp,
        lra: cli.loudness_lra,
    };

    // ── Resolve output directory ───────────────────────
    let norm_music_dir: PathBuf = cli
        .normalize_music_output
        .clone()
        .unwrap_or_else(|| default_normalize_music_dir(&cli.music));

    let (actual_norm_dir, temp_holder): (PathBuf, Option<tempfile::TempDir>) =
        match fs::create_dir_all(&norm_music_dir) {
            Ok(_) => {
                println!("    Output directory: {:?}", norm_music_dir);
                (norm_music_dir, None)
            }
            Err(e) => {
                eprintln!(
                    "⚠️  Cannot create normalized music directory {:?}: {}",
                    norm_music_dir, e
                );
                let tmp = tempfile::tempdir().expect("failed to create temp dir for music");
                let tmp_path = tmp.path().to_path_buf();
                eprintln!("    Using temporary directory instead: {:?}", tmp_path);
                eprintln!(
                    "    ⚠️  Normalized music files will be deleted when processing completes."
                );
                eprint!("    Continue? [Y/n] ");
                let _ = io::stdout().flush();
                let mut answer = String::new();
                let stdin_ok = io::stdin().read_line(&mut answer).is_ok();
                let answer = answer.trim().to_lowercase();
                if !stdin_ok || answer == "n" || answer == "no" {
                    return Err("Aborted by user.".into());
                }
                (tmp_path, Some(tmp))
            }
        };

    let is_temp = temp_holder.is_some();

    // ── Check for already-normalized files ─────────────
    let mut already_done: Vec<(PathBuf, f64)> = Vec::new();
    let mut to_normalize: Vec<(PathBuf, PathBuf)> = Vec::new(); // (source, dest)

    let can_use_cache = !cli.force_normalize_music && !is_temp;

    if can_use_cache {
        for (i, src) in music_files.iter().enumerate() {
            let norm_path = normalized_music_path(&actual_norm_dir, &cli.music, src);
            let src_dur = music_durs[i];

            let cached_dur = if norm_path.exists() {
                cache::get_duration(&norm_path, dur_cache)
            } else {
                None
            };

            let is_valid = cached_dur.is_some_and(|d| {
                if d <= MIN_DURATION {
                    return false;
                }
                let diff = (d - src_dur).abs();
                diff <= NORM_DURATION_TOLERANCE_S
                    || diff <= src_dur * NORM_DURATION_TOLERANCE_PCT
            });

            if is_valid {
                already_done.push((norm_path, cached_dur.unwrap()));
            } else {
                to_normalize.push((src.clone(), norm_path));
            }
        }

        if !already_done.is_empty() {
            println!(
                "    ⏭️  {} music track(s) already normalized \
                 (use --force-normalize-music to redo)",
                already_done.len()
            );
        }
    } else {
        if cli.force_normalize_music {
            println!("    🔄 Forcing re-normalization of all music tracks");
        }
        for src in music_files.iter() {
            let norm_path = normalized_music_path(&actual_norm_dir, &cli.music, src);
            to_normalize.push((src.clone(), norm_path));
        }
    }

    // ── Normalize the remaining files ──────────────────
    if !to_normalize.is_empty() {
        let music_workers = progress::WorkerBars::new(
            mp,
            to_normalize.len() as u64,
            cli.threads,
            "Normalizing music",
        );

        // When fewer files than threads, give each ffmpeg more cores
        let ffmpeg_threads =
            (cli.threads / cli.threads.min(to_normalize.len())).max(1) as u32;
        let norm_config = NormConfig {
            target,
            sample_rate: cli.sample_rate,
            ffmpeg_threads,
        };

        let results: Vec<Option<(PathBuf, f64)>> = to_normalize
            .par_iter()
            .map(|(src, dst)| {
                if cancelled.load(Ordering::Relaxed) {
                    return None;
                }
                let src_dur = cache::get_duration(src, dur_cache).unwrap_or(0.0);
                let dur_ms = (src_dur * 1000.0) as u64;
                let name = src
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                music_workers.begin_phase(&name, dur_ms, "norm₁");
                let m = ffmpeg::measure_loudness(
                    src,
                    &target,
                    src_dur,
                    music_workers.current_pb(),
                    ffmpeg_threads,
                )?;

                if let Some(p) = dst.parent() {
                    let _ = fs::create_dir_all(p);
                }

                music_workers.begin_phase(&name, dur_ms, "norm₂");
                let (ok, _) = ffmpeg::normalize_two_pass(
                    src,
                    dst,
                    &norm_config,
                    &m,
                    src_dur,
                    music_workers.current_pb(),
                );
                music_workers.complete_file();
                if !ok {
                    return None;
                }
                let dur = cache::get_duration(dst, dur_cache)?;
                if dur > MIN_DURATION {
                    Some((dst.clone(), dur))
                } else {
                    None
                }
            })
            .collect();

        music_workers.finish_all("Done");

        let newly_normalized: Vec<(PathBuf, f64)> = results.into_iter().flatten().collect();
        println!(
            "    {} music track(s) newly normalized",
            newly_normalized.len()
        );
        already_done.extend(newly_normalized);
    }

    // ── Build result ──────────────────────────────────
    let total_norm = already_done.len();
    let (nf, nd): (Vec<_>, Vec<_>) = already_done.into_iter().unzip();

    if !is_temp {
        println!(
            "    {} total normalized music track(s) in: {:?}\n",
            total_norm, actual_norm_dir
        );
    } else {
        println!("    {} total normalized music track(s)\n", total_norm);
    }

    if nf.is_empty() {
        return Err("All music normalization failed.".into());
    }

    Ok(Some(MusicNormResult {
        files: nf,
        durations: nd,
        _tmp_dir: temp_holder,
    }))
}
