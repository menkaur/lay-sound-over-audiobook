mod cache;
mod chapters;
mod cli;
mod cover;
mod discovery;
mod ffmpeg;
mod images;
mod log;
mod music_norm;
mod pipeline;
mod plan;
mod progress;
mod time;

use crate::cli::Args;
use crate::log::{JsonLog, LogSettings};
use crate::pipeline::FilePlan;
use clap::Parser;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const CACHE_FILE: &str = ".audio_duration_cache.json";

/// Minimum usable audio duration in seconds — files/chapters shorter
/// than this are skipped as unusable.  Shared across modules.
pub const MIN_DURATION: f64 = 0.01;

// ─── Helpers ───────────────────────────────────────────────────

/// Check for Ctrl+C and exit cleanly if cancelled.
fn check_cancel(cancelled: &AtomicBool, cli: &Args, json_log: &Arc<Mutex<JsonLog>>) {
    if cancelled.load(Ordering::Relaxed) {
        cleanup_and_exit(cli, json_log, 1);
    }
}

/// Write the JSON log (if configured) and exit.
fn cleanup_and_exit(cli: &Args, json_log: &Arc<Mutex<JsonLog>>, code: i32) -> ! {
    {
        let mut l = json_log.lock().unwrap();
        l.finished = Some(time::now_iso8601());
    }
    if let Some(ref log_path) = cli.log {
        let l = json_log.lock().unwrap();
        if let Ok(json) = serde_json::to_string_pretty(&*l) {
            let _ = fs::write(log_path, json);
        }
    }
    std::process::exit(code);
}

// ─── Main ──────────────────────────────────────────────────────

fn main() {
    let cli = Args::parse();

    // Cancellation handler
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let c = cancelled.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n⚠️  Ctrl+C received, finishing current tasks and cleaning up...");
            c.store(true, Ordering::SeqCst);
        })
        .expect("Error setting Ctrl+C handler");
    }

    let output_dir: PathBuf = cli
        .output
        .clone()
        .unwrap_or_else(|| discovery::default_output_dir(&cli.input));

    let json_log = Arc::new(Mutex::new(JsonLog {
        started: time::now_iso8601(),
        input_dir: cli.input.to_string_lossy().into(),
        music_dir: cli.music.to_string_lossy().into(),
        output_dir: output_dir.to_string_lossy().into(),
        settings: LogSettings {
            loudness_drop: cli.loudness_drop,
            threads: cli.threads,
            pause: cli.pause,
            crossfade: cli.crossfade,
            format: format!("{:?}", cli.format).to_lowercase(),
            quality: cli.quality,
            sample_rate: cli.sample_rate,
            loudness_i: cli.loudness_i,
            loudness_tp: cli.loudness_tp,
            loudness_lra: cli.loudness_lra,
            music_fade_in: cli.music_fade_in,
            music_fade_out: cli.music_fade_out,
            normalize_input: cli.normalize_input,
            normalize_music: cli.normalize_music,
            normalize_music_output: cli
                .normalize_music_output
                .as_ref()
                .map(|p| p.to_string_lossy().into()),
            split_chapters: cli.split_chapters,
            speed: cli.speed,
        },
        ..Default::default()
    }));

    cli::validate_args(&cli);

    if cli.dry_run {
        println!("🔍 DRY RUN MODE — no files will be modified\n");
    }

    println!("📁  Output directory: {:?}", output_dir);
    println!(
        "📊  Format: {:?}, Quality: {}, Sample rate: {} Hz",
        cli.format, cli.quality, cli.sample_rate
    );
    if (cli.speed - 1.0).abs() > 0.001 {
        println!("⏩  Voice speed: {:.2}x", cli.speed);
    }
    if !cli.normalize_input {
        println!("⏭️   Input normalization: disabled (overlaying on original files)");
    }
    println!();

    rayon::ThreadPoolBuilder::new()
        .num_threads(cli.threads)
        .build_global()
        .expect("failed to build thread pool");

    let cache_path = PathBuf::from(CACHE_FILE);
    let dur_cache = Arc::new(Mutex::new(cache::load(&cache_path)));
    let mp = indicatif::MultiProgress::new();

    // ══════════════════════════════════════════════════════════════
    // Phase 1: Scan directories and probe durations concurrently
    // ══════════════════════════════════════════════════════════════

    println!("🎵  Scanning music dir: {:?}", cli.music);
    let music_all = discovery::collect_unsorted(&cli.music);
    println!("    Found {} music files", music_all.len());
    check_cancel(&cancelled, &cli, &json_log);

    println!("📂  Scanning input dir: {:?}", cli.input);
    let input_files = discovery::collect_sorted(&cli.input);
    println!("    {} input files (natural-sorted)", input_files.len());

    if input_files.is_empty() {
        eprintln!("❌ No input files found.");
        cleanup_and_exit(&cli, &json_log, 1);
    }

    let input_items = cli::expand_chapters(&cli, &input_files);
    let input_items = cli::filter_resume(&cli, &output_dir, input_items, &json_log);

    if input_items.is_empty() {
        println!("✅ All files already processed!");
        cleanup_and_exit(&cli, &json_log, 0);
    }

    println!("    {} files to process\n", input_items.len());
    check_cancel(&cancelled, &cli, &json_log);

    // ── Probe durations concurrently ──────────────────────────────
    let pb_music = progress::create_bar(&mp, music_all.len() as u64, "Probing music");
    let pb_input = progress::create_bar(&mp, input_items.len() as u64, "Probing input");

    let (music_ok, input_with_durs) = rayon::join(
        || {
            let result: Vec<(PathBuf, f64)> = music_all
                .par_iter()
                .filter_map(|f| {
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    let dur = cache::get_duration(f, &dur_cache)?;
                    pb_music.inc(1);
                    Some((f.clone(), dur))
                })
                .collect();
            pb_music.finish_with_message("Done");
            result
        },
        || {
            let result: Vec<(cli::InputItem, f64)> = input_items
                .par_iter()
                .filter_map(|item| {
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    let raw_dur = if let Some(ref ch) = item.chapter {
                        ch.end - ch.start
                    } else {
                        cache::get_duration(&item.source, &dur_cache)?
                    };
                    pb_input.inc(1);
                    Some((item.clone(), raw_dur))
                })
                .collect();
            pb_input.finish_with_message("Done");
            result
        },
    );

    cache::save(&dur_cache.lock().unwrap(), &cache_path);

    // ── Process music results ─────────────────────────────────────
    let (mut music_files, mut music_durs): (Vec<_>, Vec<_>) = music_ok
        .into_iter()
        .filter(|(_, d)| *d > MIN_DURATION)
        .unzip();

    let total_music_s: f64 = music_durs.iter().sum();
    println!(
        "\n    {} usable music tracks  ({:.1} s total)",
        music_files.len(),
        total_music_s
    );

    {
        let mut l = json_log.lock().unwrap();
        l.music_files = music_files.len();
        l.music_duration_s = total_music_s;
    }

    if music_files.is_empty() {
        eprintln!("❌ No usable music found.");
        cleanup_and_exit(&cli, &json_log, 1);
    }

    // ── Check input probing results ───────────────────────────────
    if input_with_durs.is_empty() {
        eprintln!("❌ No input files could be probed.");
        cleanup_and_exit(&cli, &json_log, 1);
    }

    let total_input_s: f64 = input_with_durs.iter().map(|(_, d)| d / cli.speed).sum();
    println!(
        "    {} input files ready  ({:.1} s after speed adjustment)\n",
        input_with_durs.len(),
        total_input_s
    );

    {
        let mut l = json_log.lock().unwrap();
        l.input_files = input_with_durs.len();
        l.input_duration_s = total_input_s;
    }

    check_cancel(&cancelled, &cli, &json_log);

    // ══════════════════════════════════════════════════════════════
    // Phase 2: Normalize music (if requested)
    // ══════════════════════════════════════════════════════════════

    let _music_tmp_dir: Option<tempfile::TempDir>;

    match music_norm::normalize_music(
        &cli,
        &music_files,
        &music_durs,
        &dur_cache,
        &cancelled,
        &mp,
    ) {
        Ok(Some(result)) => {
            music_files = result.files;
            music_durs = result.durations;
            _music_tmp_dir = result._tmp_dir;
        }
        Ok(None) => {
            _music_tmp_dir = None;
        }
        Err(msg) => {
            eprintln!("❌ {msg}");
            cleanup_and_exit(&cli, &json_log, 1);
        }
    }

    cache::save(&dur_cache.lock().unwrap(), &cache_path);
    check_cancel(&cancelled, &cli, &json_log);

    // ══════════════════════════════════════════════════════════════
    // Phase 3: Build seamless music plan
    // ══════════════════════════════════════════════════════════════

    println!("🎲  Building seamless music overlay plan...");
    let adjusted_durations: Vec<f64> = input_with_durs.iter().map(|(_, d)| d / cli.speed).collect();
    let plans = plan::build_music_plan(
        &adjusted_durations,
        &music_files,
        &music_durs,
        cli.pause,
        cli.crossfade,
    );

    let volume = 1.0 / cli.loudness_drop;
    println!(
        "    Music volume = {volume:.4}  (1 / {:.1})\n",
        cli.loudness_drop
    );

    // ══════════════════════════════════════════════════════════════
    // Phase 4: Assemble file plans
    // ══════════════════════════════════════════════════════════════

    let file_plans: Vec<FilePlan> = input_with_durs
        .into_iter()
        .zip(plans)
        .map(|((item, raw_dur), pieces)| {
            let output = output_dir
                .join(&item.relative)
                .with_extension(cli.format.extension());
            FilePlan {
                item,
                pieces,
                source_duration: raw_dur,
                output,
            }
        })
        .collect();

    // ── Detect output path collisions ─────────────────────────────
    {
        let mut seen: HashMap<PathBuf, Vec<&PathBuf>> = HashMap::new();
        for fp in &file_plans {
            seen.entry(fp.output.clone())
                .or_default()
                .push(&fp.item.relative);
        }
        let dupes: Vec<_> = seen.iter().filter(|(_, v)| v.len() > 1).collect();
        if !dupes.is_empty() {
            eprintln!(
                "⚠️  Warning: {} output path(s) have collisions:",
                dupes.len()
            );
            for (out, srcs) in &dupes {
                let names: Vec<_> = srcs.iter().map(|s| s.display().to_string()).collect();
                eprintln!("      {} ← {}", out.display(), names.join(", "));
            }
            eprintln!();
        }
    }

    // ── Copy images and extract covers ────────────────────────────
    {
        let mut cover_dirs: HashMap<PathBuf, PathBuf> = HashMap::new();
        for fp in &file_plans {
            let out_dir = fp.output.parent().unwrap_or(&output_dir).to_path_buf();
            cover_dirs
                .entry(out_dir)
                .or_insert_with(|| fp.item.source.clone());
        }
        images::copy_images_and_extract_covers(&cli.input, &output_dir, &cover_dirs, cli.dry_run);
    }

    // ══════════════════════════════════════════════════════════════
    // Phase 5: Per-file streaming pipeline
    // ══════════════════════════════════════════════════════════════

    pipeline::run_pipeline(
        &cli,
        &file_plans,
        &dur_cache,
        &cancelled,
        &json_log,
        &mp,
        {
            // When fewer files than threads, give each ffmpeg more cores
            let ffmpeg_threads =
                (cli.threads / cli.threads.min(file_plans.len())).max(1) as u32;
            let target = ffmpeg::LoudnessTarget {
                i: cli.loudness_i,
                tp: cli.loudness_tp,
                lra: cli.loudness_lra,
            };
            pipeline::PipelineAudioConfig {
                norm: ffmpeg::NormConfig {
                    target,
                    sample_rate: cli.sample_rate,
                    ffmpeg_threads,
                },
                overlay: ffmpeg::OverlayConfig {
                    volume,
                    format: cli.format,
                    quality: cli.quality,
                    sample_rate: cli.sample_rate,
                    speed: cli.speed,
                    music_fade_in: cli.music_fade_in,
                    music_fade_out: cli.music_fade_out,
                    ffmpeg_threads,
                },
            }
        },
    );

    // ── Finalize ──────────────────────────────────────────────────
    {
        let mut l = json_log.lock().unwrap();
        l.finished = Some(time::now_iso8601());
    }

    if let Some(ref log_path) = cli.log {
        let l = json_log.lock().unwrap();
        if let Ok(json) = serde_json::to_string_pretty(&*l) {
            if let Err(e) = fs::write(log_path, json) {
                eprintln!("⚠️  Failed to write log file: {e}");
            } else {
                println!("\n📄  Log written to {:?}", log_path);
            }
        }
    }

    println!("\n✨  All done!  Output in {:?}", output_dir);
}
