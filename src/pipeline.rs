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
use crate::plan::{self, MusicPiece};
use crate::progress;

use crate::MIN_DURATION;

/// Pre-computed plan for one output file.
///
/// Created after scanning and probing so that each file knows its
/// music overlay, expected duration, and output path before the
/// streaming pipeline begins.
pub struct FilePlan {
    pub item: InputItem,
    /// Raw source duration (or chapter segment duration) in seconds.
    pub source_duration: f64,
    pub output: PathBuf,
    /// Index into the music plan for this file.
    pub plan_index: usize,
}

/// Music-related data needed for the concurrent pipeline.
pub struct MusicInput {
    pub files: Vec<PathBuf>,
    pub durations: Vec<f64>,
    /// Pre-computed adjusted durations for input files (after speed).
    pub adjusted_input_durations: Vec<f64>,
    pub pause: f64,
    pub crossfade: f64,
    pub volume: f64,
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
    // Music plan synchronization
    music_plan: &'a Mutex<Option<Vec<Vec<MusicPiece>>>>,
    music_ready: &'a std::sync::atomic::AtomicBool,
    music_failed: &'a std::sync::atomic::AtomicBool,
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
        if ch.start >= ch.end {
            failed_count.fetch_add(1, Ordering::Relaxed);
            workers.complete_file();
            let mut l = json_log.lock().unwrap();
            l.failed.push(LogEntry {
                file: fp.item.relative.to_string_lossy().into(),
                error: Some(format!(
                    "invalid chapter range: start={:.3} >= end={:.3}",
                    ch.start, ch.end
                )),
                command: None,
            });
            return;
        }
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

    // ── Step 4: Wait for music plan to be ready ──────────
    while !ctx.music_ready.load(Ordering::Acquire) {
        std::thread::yield_now();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    // Check if music normalization failed
    if ctx.music_failed.load(Ordering::Acquire) {
        failed_count.fetch_add(1, Ordering::Relaxed);
        workers.complete_file();
        return;
    }

    // Get this file's music pieces
    let pieces = {
        let plans = ctx.music_plan.lock().unwrap();
        let all_plans = plans.as_ref().unwrap();
        match all_plans.get(fp.plan_index) {
            Some(p) => p.clone(),
            None => {
                failed_count.fetch_add(1, Ordering::Relaxed);
                workers.complete_file();
                let mut l = json_log.lock().unwrap();
                l.failed.push(LogEntry {
                    file: fp.item.relative.to_string_lossy().into(),
                    error: Some(format!(
                        "music plan index {} out of range ({})",
                        fp.plan_index,
                        all_plans.len()
                    )),
                    command: None,
                });
                return;
            }
        }
    };

    let task = FileTask {
        normalized,
        output: fp.output.clone(),
        duration: actual_adjusted,
        pieces,
    };

    // ── Step 5: Overlay + encode ──────────────────────
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

/// Shared state references passed through the pipeline.
pub struct PipelineShared<'a> {
    pub dur_cache: &'a Arc<Mutex<DurationCache>>,
    pub cancelled: &'a Arc<AtomicBool>,
    pub json_log: &'a Arc<Mutex<JsonLog>>,
    pub mp: &'a indicatif::MultiProgress,
    pub cache_path: &'a Path,
}

/// Run the streaming pipeline over all file plans.
///
/// Music normalization and planning happens concurrently in a dedicated thread.
/// Per-file pipeline (extract chapter → normalize → overlay) runs in parallel
/// via rayon. Each file's overlay starts as soon as that file's book norm is done
/// AND all music norm is done.
pub fn run_pipeline(
    cli: &Args,
    file_plans: &[FilePlan],
    music: MusicInput,
    shared: &PipelineShared<'_>,
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
        progress::WorkerBars::new(shared.mp, total_tasks as u64, cli.threads, phase_label);
    let failed_count = AtomicUsize::new(0);

    // Music plan synchronization
    let music_plan: Mutex<Option<Vec<Vec<MusicPiece>>>> = Mutex::new(None);
    let music_ready = AtomicBool::new(false);
    let music_failed = AtomicBool::new(false);
    // Keep temp dir alive until scope exits
    let music_tmp_holder: Mutex<Option<tempfile::TempDir>> = Mutex::new(None);

    std::thread::scope(|scope| {
        // ── Music normalization + plan building (dedicated OS thread) ──
        scope.spawn(|| {
            use crate::music_norm;

            let (m_files, m_durs, tmp_dir) = match music_norm::normalize_music(
                cli,
                &music.files,
                &music.durations,
                shared.dur_cache,
                shared.cancelled,
                shared.mp,
            ) {
                Ok(Some(result)) => (result.files, result.durations, result._tmp_dir),
                Ok(None) => (music.files.clone(), music.durations.clone(), None),
                Err(msg) => {
                    eprintln!("❌ {msg}");
                    music_failed.store(true, Ordering::Release);
                    music_ready.store(true, Ordering::Release);
                    return;
                }
            };

            cache::save(&shared.dur_cache.lock().unwrap(), shared.cache_path);

            println!("🎲  Building seamless music overlay plan...");
            let plans = plan::build_music_plan(
                &music.adjusted_input_durations,
                &m_files,
                &m_durs,
                music.pause,
                music.crossfade,
            );
            println!(
                "    Music volume = {:.4}  (1 / {:.1})\n",
                music.volume, cli.loudness_drop
            );

            *music_tmp_holder.lock().unwrap() = tmp_dir;
            *music_plan.lock().unwrap() = Some(plans);
            music_ready.store(true, Ordering::Release);
        });

        // ── Per-file pipeline (rayon thread pool) ─────────────────
        let ctx = PipelineCtx {
            cli,
            tmp: tmp.path(),
            dur_cache: shared.dur_cache,
            cancelled: shared.cancelled,
            workers: &workers,
            json_log: shared.json_log,
            failed_count: &failed_count,
            norm_config: audio.norm,
            overlay_config: audio.overlay,
            music_plan: &music_plan,
            music_ready: &music_ready,
            music_failed: &music_failed,
        };

        file_plans.par_iter().for_each(|fp| {
            process_one_file(fp, &ctx);
        });
    });

    workers.finish_all("Done");

    let failed = failed_count.load(Ordering::Relaxed);
    if failed > 0 {
        eprintln!("\n    ❌ {} files failed", failed);
    }
}
