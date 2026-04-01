//! Per-file streaming pipeline — normalize → overlay for each file
//! independently, with real-time per-worker progress bars.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::cache::{self, DurationCache};
use crate::cli::{Args, InputItem};
use crate::discovery;
use crate::ffmpeg::{self, FileTask, NormConfig, OverlayConfig};
use crate::log::{JsonLog, LogEntry};
use crate::plan::MusicPiece;
use crate::progress;

use crate::MIN_DURATION;

/// Pre-computed plan for one output file.
///
/// Created after scanning and probing so that each file knows its
/// music overlay, expected duration, and output path before the
/// streaming pipeline begins.
pub struct FilePlan {
    pub item: InputItem,
    pub pieces: Vec<MusicPiece>,
    /// Raw source duration (or chapter segment duration) in seconds.
    pub source_duration: f64,
    pub output: PathBuf,
}

/// Shared context for the streaming pipeline — avoids passing many
/// arguments through every `process_one_file` call.
struct PipelineCtx<'a> {
    cli: &'a Args,
    tmp: &'a Path,
    dur_cache: &'a Arc<Mutex<DurationCache>>,
    cancelled: &'a Arc<AtomicBool>,
    workers: &'a progress::WorkerBars,
    json_log: &'a Arc<Mutex<JsonLog>>,
    failed_count: &'a AtomicUsize,
    norm_config: NormConfig,
    overlay_config: OverlayConfig,
}

/// Process a single file through the full pipeline:
/// extract chapter → normalize → probe → overlay.
fn process_one_file(fp: &FilePlan, ctx: &PipelineCtx<'_>) {
    let cli = ctx.cli;
    let tmp = ctx.tmp;
    let dur_cache = ctx.dur_cache;
    let cancelled = ctx.cancelled;
    let workers = ctx.workers;
    let json_log = ctx.json_log;
    let failed_count = ctx.failed_count;
    if cancelled.load(Ordering::Relaxed) {
        return;
    }

    let name = fp
        .item
        .relative
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // ── Step 1: Extract chapter segment if needed ──────
    let source_for_norm = if let Some(ref ch) = fp.item.chapter {
        let dur = ch.end - ch.start;
        if dur <= MIN_DURATION {
            failed_count.fetch_add(1, Ordering::Relaxed);
            workers.complete_file();
            let mut l = json_log.lock().unwrap();
            l.failed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: Some(format!(
                    "invalid chapter duration: {dur:.3}s (start={:.3}, end={:.3})",
                    ch.start, ch.end
                )),
                command: None,
            });
            return;
        }

        let chapter_tmp =
            discovery::append_extension(tmp, &fp.item.relative, "chapter.wav");
        if let Some(p) = chapter_tmp.parent() {
            let _ = fs::create_dir_all(p);
        }
        let ft = ctx.overlay_config.ffmpeg_threads;
        let mut chapter_args = Command::new("ffmpeg");
        chapter_args.arg("-y");
        if ft > 1 {
            chapter_args.args(["-threads", &ft.to_string()]);
        }
        let st = chapter_args
            .arg("-i")
            .arg(&fp.item.source)
            .args([
                "-ss",
                &format!("{:.6}", ch.start),
                "-t",
                &format!("{dur:.6}"),
                "-vn",
                "-ar",
                &cli.sample_rate.to_string(),
                "-ac",
                "2",
            ])
            .arg(&chapter_tmp)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if !matches!(st, Ok(s) if s.success()) {
            failed_count.fetch_add(1, Ordering::Relaxed);
            workers.complete_file();
            let mut l = json_log.lock().unwrap();
            l.failed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: Some("chapter extraction failed".into()),
                command: None,
            });
            return;
        }
        chapter_tmp
    } else {
        fp.item.source.clone()
    };

    // ── Step 2: Normalize input (if enabled) ──────────
    let normalized = if cli.normalize_input {
        let dst = discovery::append_extension(tmp, &fp.item.relative, "wav");
        if let Some(p) = dst.parent() {
            let _ = fs::create_dir_all(p);
        }

        let src_dur = cache::get_duration(&source_for_norm, dur_cache)
            .unwrap_or(fp.source_duration);
        let dur_ms = (src_dur * 1000.0) as u64;

        // Pass 1: measure loudness
        workers.begin_phase(&name, dur_ms, "norm₁");
        let measurement = match ffmpeg::measure_loudness(
            &source_for_norm,
            &ctx.norm_config.target,
            src_dur,
            workers.current_pb(),
            ctx.norm_config.ffmpeg_threads,
        ) {
            Some(m) => m,
            None => {
                if fp.item.chapter.is_some() {
                    let _ = fs::remove_file(&source_for_norm);
                }
                failed_count.fetch_add(1, Ordering::Relaxed);
                workers.complete_file();
                let mut l = json_log.lock().unwrap();
                l.failed.push(LogEntry {
                    file: fp.item.relative.to_string_lossy().into(),
                    error: Some("loudness measurement failed".into()),
                    command: None,
                });
                return;
            }
        };

        // Pass 2: normalize
        workers.begin_phase(&name, dur_ms, "norm₂");
        let (ok, cmd) = ffmpeg::normalize_two_pass(
            &source_for_norm,
            &dst,
            &ctx.norm_config,
            &measurement,
            src_dur,
            workers.current_pb(),
        );

        if fp.item.chapter.is_some() {
            let _ = fs::remove_file(&source_for_norm);
        }

        if !ok {
            failed_count.fetch_add(1, Ordering::Relaxed);
            workers.complete_file();
            let mut l = json_log.lock().unwrap();
            l.failed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: Some("normalization failed".into()),
                command: if cmd.is_empty() {
                    None
                } else {
                    Some(format!("ffmpeg {}", cmd.join(" ")))
                },
            });
            return;
        }

        dst
    } else {
        // No normalization — use source directly (or extracted chapter)
        if fp.item.chapter.is_some() {
            source_for_norm // keep the extracted chapter tmp file
        } else {
            fp.item.source.clone()
        }
    };

    // ── Step 3: Probe actual duration and build task ──
    let raw_dur =
        cache::get_duration(&normalized, dur_cache).unwrap_or(fp.source_duration);
    let actual_adjusted = raw_dur / cli.speed;

    if actual_adjusted <= MIN_DURATION {
        failed_count.fetch_add(1, Ordering::Relaxed);
        workers.complete_file();
        return;
    }

    let task = FileTask {
        normalized,
        output: fp.output.clone(),
        duration: actual_adjusted,
        pieces: fp.pieces.clone(),
    };

    // ── Step 4: Overlay + encode ──────────────────────
    workers.begin_phase(&name, (actual_adjusted * 1000.0) as u64, "mix");

    match ffmpeg::overlay_music(
        &task,
        &ctx.overlay_config,
        cancelled,
        workers.current_pb(),
    ) {
        Ok(()) => {
            let mut l = json_log.lock().unwrap();
            l.processed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: None,
                command: None,
            });
        }
        Err((err, cmd)) => {
            failed_count.fetch_add(1, Ordering::Relaxed);
            let mut l = json_log.lock().unwrap();
            l.failed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: Some(err),
                command: if cmd.is_empty() { None } else { Some(cmd) },
            });
        }
    }

    workers.complete_file();
}

/// Audio processing settings for the pipeline.
pub struct PipelineAudioConfig {
    pub norm: NormConfig,
    pub overlay: OverlayConfig,
}

/// Run the Phase 5 streaming pipeline over all file plans.
///
/// Each file flows through (extract chapter →) normalize → overlay
/// immediately, without waiting for all files to finish a stage.
pub fn run_pipeline(
    cli: &Args,
    file_plans: &[FilePlan],
    dur_cache: &Arc<Mutex<DurationCache>>,
    cancelled: &Arc<AtomicBool>,
    json_log: &Arc<Mutex<JsonLog>>,
    mp: &indicatif::MultiProgress,
    audio: PipelineAudioConfig,
) {
    let total_tasks = file_plans.len();
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    if cli.dry_run {
        if cli.normalize_input {
            println!("🔊  [DRY RUN] Would normalize {} files", total_tasks);
        }
        println!("🎶  [DRY RUN] Would overlay & encode {total_tasks} files:");
        for fp in file_plans {
            println!(
                "      {} → {}",
                fp.item.relative.display(),
                fp.output.display()
            );
        }
        return;
    }

    let phase_label = if cli.normalize_input {
        "Normalizing & encoding"
    } else {
        "Encoding"
    };
    println!("🎶  {phase_label} {total_tasks} files (per-file streaming)");

    let workers =
        progress::WorkerBars::new(mp, total_tasks as u64, cli.threads, phase_label);
    let failed_count = AtomicUsize::new(0);

    let ctx = PipelineCtx {
        cli,
        tmp: tmp.path(),
        dur_cache,
        cancelled,
        workers: &workers,
        json_log,
        failed_count: &failed_count,
        norm_config: audio.norm,
        overlay_config: audio.overlay,
    };

    file_plans.par_iter().for_each(|fp| {
        process_one_file(fp, &ctx);
    });

    workers.finish_all("Done");

    let failed = failed_count.load(Ordering::Relaxed);
    if failed > 0 {
        eprintln!("\n    ❌ {} files failed", failed);
    }
}
