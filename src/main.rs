mod cache;
mod chapters;
mod cover;
mod discovery;
mod ffmpeg;
mod log;
mod plan;
mod progress;
mod time;

use crate::ffmpeg::{FileTask, OutputFormat};
use crate::log::{JsonLog, LogEntry, LogSettings};
use clap::Parser;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const CACHE_FILE: &str = ".audio_duration_cache.json";
const MIN_DURATION: f64 = 0.01;

/// Image extensions to copy from source directories.
const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "bmp", "gif", "webp", "tiff", "tif", "svg",
];

// ─── CLI Arguments ─────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "drop-ins", version, about)]
struct Args {
    /// Input directory containing audio files (searched recursively, natural sort)
    #[arg(short, long)]
    input: PathBuf,

    /// Music directory containing background music files
    #[arg(short, long)]
    music: PathBuf,

    /// Output directory. Defaults to <input>_processed/
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Loudness drop for music (music volume = 1 / this_value)
    #[arg(short = 'l', long = "loudness-drop", default_value_t = 4.0)]
    loudness_drop: f64,

    /// Number of processing threads
    #[arg(short = 't', long, default_value_t = 48)]
    threads: usize,

    /// Pause between music tracks (seconds). Ignored if --crossfade > 0.
    #[arg(short, long, default_value_t = 0.0)]
    pause: f64,

    /// Crossfade duration between music tracks (seconds). Supersedes --pause.
    #[arg(long, default_value_t = 0.0)]
    crossfade: f64,

    /// Output format
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Ogg)]
    format: OutputFormat,

    /// Output quality (0–10 for ogg/mp3/opus, 0–12 for flac)
    #[arg(short, long, default_value_t = 6)]
    quality: u8,

    /// Sample rate in Hz
    #[arg(long, default_value_t = 44100)]
    sample_rate: u32,

    /// Target integrated loudness (LUFS)
    #[arg(long, default_value_t = -16.0)]
    loudness_i: f64,

    /// True peak limit (dBTP)
    #[arg(long, default_value_t = -1.5)]
    loudness_tp: f64,

    /// Loudness range (LU)
    #[arg(long, default_value_t = 11.0)]
    loudness_lra: f64,

    /// Music fade-in at start of each input file (seconds)
    #[arg(long, default_value_t = 0.0)]
    music_fade_in: f64,

    /// Music fade-out at end of each input file (seconds)
    #[arg(long, default_value_t = 0.0)]
    music_fade_out: f64,

    /// Also normalize music tracks
    #[arg(long, default_value_t = false)]
    normalize_music: bool,

    /// Skip files that already exist in output directory
    #[arg(long, default_value_t = false)]
    resume: bool,

    /// Show what would be done without doing it
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Split m4b/m4a audiobooks by chapters
    #[arg(long, default_value_t = false)]
    split_chapters: bool,

    /// Write JSON log to file
    #[arg(long)]
    log: Option<PathBuf>,

    /// Speed up voice audio (1.0 = normal, 1.5 = 50% faster). Music is unaffected.
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
}

// ─── Input Item ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct InputItem {
    source: PathBuf,
    relative: PathBuf,
    chapter: Option<chapters::Chapter>,
}

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

/// Validate all CLI arguments, exiting on error.
fn validate_args(cli: &Args) {
    if cli.loudness_drop <= 0.0 {
        eprintln!("❌ --loudness-drop must be > 0");
        std::process::exit(1);
    }
    if cli.threads == 0 {
        eprintln!("❌ --threads must be ≥ 1");
        std::process::exit(1);
    }
    if cli.pause < 0.0 {
        eprintln!("❌ --pause must be ≥ 0");
        std::process::exit(1);
    }
    if cli.crossfade < 0.0 {
        eprintln!("❌ --crossfade must be ≥ 0");
        std::process::exit(1);
    }
    if cli.quality > cli.format.max_quality() {
        eprintln!(
            "❌ --quality must be 0–{} for {:?}",
            cli.format.max_quality(),
            cli.format
        );
        std::process::exit(1);
    }
    if cli.music_fade_in < 0.0 || cli.music_fade_out < 0.0 {
        eprintln!("❌ --music-fade-in and --music-fade-out must be ≥ 0");
        std::process::exit(1);
    }
    if cli.speed < 0.5 || cli.speed > 100.0 {
        eprintln!("❌ --speed must be between 0.5 and 100.0");
        std::process::exit(1);
    }
    if cli.crossfade > 0.001 && cli.pause > 0.001 {
        eprintln!("ℹ️  --crossfade supersedes --pause; pause will be ignored");
    }
    if !cli.input.is_dir() {
        eprintln!("❌ Input directory does not exist: {:?}", cli.input);
        std::process::exit(1);
    }
    if !cli.music.is_dir() {
        eprintln!("❌ Music directory does not exist: {:?}", cli.music);
        std::process::exit(1);
    }
    for tool in ["ffmpeg", "ffprobe"] {
        if Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_err()
        {
            eprintln!("❌ `{tool}` not found in PATH. Please install FFmpeg.");
            std::process::exit(1);
        }
    }
}

/// Expand input files into InputItems, optionally splitting by chapters.
fn expand_chapters(cli: &Args, input_files: &[PathBuf]) -> Vec<InputItem> {
    if cli.split_chapters {
        println!("    Detecting chapters...");
        let items: Vec<InputItem> = input_files
            .iter()
            .flat_map(|src| {
                let rel = src.strip_prefix(&cli.input).unwrap_or(src).to_path_buf();
                let chaps = chapters::get_chapters(src);
                if chaps.is_empty() {
                    vec![InputItem {
                        source: src.clone(),
                        relative: rel,
                        chapter: None,
                    }]
                } else {
                    chaps
                        .into_iter()
                        .enumerate()
                        .map(|(i, ch)| {
                            let parent = rel.parent().unwrap_or(Path::new(""));
                            let stem = rel.file_stem().unwrap_or_default().to_string_lossy();
                            let new_name = format!(
                                "{} - {:03} - {}",
                                stem,
                                i + 1,
                                discovery::sanitize_filename(&ch.title)
                            );
                            InputItem {
                                source: src.clone(),
                                relative: parent.join(new_name),
                                chapter: Some(ch),
                            }
                        })
                        .collect()
                }
            })
            .collect();
        println!("    {} items after chapter expansion\n", items.len());
        items
    } else {
        input_files
            .iter()
            .map(|src| InputItem {
                source: src.clone(),
                relative: src.strip_prefix(&cli.input).unwrap_or(src).to_path_buf(),
                chapter: None,
            })
            .collect()
    }
}

/// Filter out already-processed files when --resume is set.
fn filter_resume(
    cli: &Args,
    output_dir: &Path,
    items: Vec<InputItem>,
    json_log: &Arc<Mutex<JsonLog>>,
) -> Vec<InputItem> {
    if !cli.resume {
        return items;
    }
    let (to_process, to_skip): (Vec<_>, Vec<_>) = items.into_iter().partition(|item| {
        let out = output_dir
            .join(&item.relative)
            .with_extension(cli.format.extension());
        !out.exists()
    });
    if !to_skip.is_empty() {
        println!("    ⏭️  Skipping {} already-processed files", to_skip.len());
        let mut l = json_log.lock().unwrap();
        for item in &to_skip {
            l.skipped.push(item.relative.to_string_lossy().into());
        }
    }
    to_process
}

/// Check if a path has an image extension.
fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Copy all image files from the input directory tree to the output
/// directory tree, preserving relative paths. Also extracts embedded
/// cover art from audio files and places `cover.jpg` in each output
/// subdirectory.
///
/// Skips images that already exist in the output (resume-friendly).
fn copy_images_and_extract_covers(
    input_dir: &Path,
    output_dir: &Path,
    tasks: &[FileTask],
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

        // Skip if already exists
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
    let mut cover_dirs: HashMap<PathBuf, PathBuf> = HashMap::new();
    for t in tasks {
        let out_dir = t.output.parent().unwrap_or(output_dir).to_path_buf();
        cover_dirs
            .entry(out_dir)
            .or_insert_with(|| t.source.clone());
    }

    let ext_count = AtomicUsize::new(0);
    cover_dirs.par_iter().for_each(|(out_dir, source)| {
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
            normalize_music: cli.normalize_music,
            split_chapters: cli.split_chapters,
            speed: cli.speed,
        },
        ..Default::default()
    }));

    validate_args(&cli);

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
    println!();

    rayon::ThreadPoolBuilder::new()
        .num_threads(cli.threads)
        .build_global()
        .expect("failed to build thread pool");

    let cache_path = PathBuf::from(CACHE_FILE);
    let dur_cache = Arc::new(Mutex::new(cache::load(&cache_path)));
    let mp = indicatif::MultiProgress::new();

    // ── 1. Music: collect & probe durations ────────────────────
    println!("🎵  Scanning music dir: {:?}", cli.music);
    let music_all = discovery::collect_unsorted(&cli.music);
    println!("    Found {} music files", music_all.len());
    check_cancel(&cancelled, &cli, &json_log);

    let pb = progress::create_bar(&mp, music_all.len() as u64, "Probing music");
    let music_ok: Vec<(PathBuf, f64)> = music_all
        .par_iter()
        .filter_map(|f| {
            if cancelled.load(Ordering::Relaxed) {
                return None;
            }
            let dur = cache::get_duration(f, &dur_cache)?;
            pb.inc(1);
            Some((f.clone(), dur))
        })
        .collect();
    pb.finish_with_message("Done");
    cache::save(&dur_cache.lock().unwrap(), &cache_path);

    let (mut music_files, mut music_durs): (Vec<_>, Vec<_>) = music_ok
        .into_iter()
        .filter(|(_, d)| *d > MIN_DURATION)
        .unzip();

    let total_music_s: f64 = music_durs.iter().sum();
    println!(
        "    {} usable tracks  ({:.1} s total)\n",
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

    // ── 1b. Optionally normalize music ─────────────────────────
    let _music_tmp_dir: Option<tempfile::TempDir>;

    if cli.normalize_music && !cli.dry_run {
        println!("🔊  Normalizing music tracks...");
        let tmp_music = tempfile::tempdir().expect("failed to create temp dir for music");
        let pb = progress::create_bar(&mp, music_files.len() as u64, "Normalizing music");

        let results: Vec<Option<(PathBuf, f64)>> = music_files
            .par_iter()
            .enumerate()
            .map(|(i, src)| {
                if cancelled.load(Ordering::Relaxed) {
                    return None;
                }
                let src_dur = cache::get_duration(src, &dur_cache).unwrap_or(0.0);
                let m = ffmpeg::measure_loudness(
                    src,
                    cli.loudness_i,
                    cli.loudness_tp,
                    cli.loudness_lra,
                    src_dur,
                    None,
                )?;
                let dst = discovery::append_extension(
                    tmp_music.path(),
                    &PathBuf::from(format!("music_{i}")),
                    "wav",
                );
                let (ok, _) = ffmpeg::normalize_two_pass(
                    src,
                    &dst,
                    cli.loudness_i,
                    cli.loudness_tp,
                    cli.loudness_lra,
                    cli.sample_rate,
                    &m,
                    src_dur,
                    None,
                );
                pb.inc(1);
                if !ok {
                    return None;
                }
                let dur = cache::get_duration(&dst, &dur_cache)?;
                if dur > MIN_DURATION {
                    Some((dst, dur))
                } else {
                    None
                }
            })
            .collect();

        pb.finish_with_message("Done");

        let (nf, nd): (Vec<_>, Vec<_>) = results.into_iter().flatten().unzip();
        println!("    {} music tracks normalized\n", nf.len());

        if nf.is_empty() {
            eprintln!("❌ All music normalization failed.");
            cleanup_and_exit(&cli, &json_log, 1);
        }

        music_files = nf;
        music_durs = nd;
        _music_tmp_dir = Some(tmp_music);
    } else {
        _music_tmp_dir = None;
    }

    cache::save(&dur_cache.lock().unwrap(), &cache_path);
    check_cancel(&cancelled, &cli, &json_log);

    // ── 2. Input: collect files in natural order ───────────────
    println!("📂  Scanning input dir: {:?}", cli.input);
    let input_files = discovery::collect_sorted(&cli.input);
    println!("    {} input files (natural-sorted)", input_files.len());

    if input_files.is_empty() {
        eprintln!("❌ No input files found.");
        cleanup_and_exit(&cli, &json_log, 1);
    }

    // ── 2b. Expand chapters ────────────────────────────────────
    let input_items = expand_chapters(&cli, &input_files);

    // ── 2c. Resume filter ──────────────────────────────────────
    let input_items = filter_resume(&cli, &output_dir, input_items, &json_log);

    if input_items.is_empty() {
        println!("✅ All files already processed!");
        cleanup_and_exit(&cli, &json_log, 0);
    }

    println!("    {} files to process\n", input_items.len());
    check_cancel(&cancelled, &cli, &json_log);

    // ── 3. Normalise (two-pass loudnorm) to temp dir ───────────
    let tmp = tempfile::tempdir().expect("failed to create temp dir");

    if cli.dry_run {
        println!("🔊  [DRY RUN] Would normalize {} files", input_items.len());
    } else {
        println!(
            "🔊  Normalizing {} files (two-pass loudnorm)",
            input_items.len()
        );
    }

    let pb_norm = progress::create_bar(&mp, input_items.len() as u64, "Normalizing");
    let failed_norm = AtomicUsize::new(0);

    let normed: Vec<Option<(InputItem, PathBuf, f64)>> = input_items
        .par_iter()
        .map(|item| {
            if cancelled.load(Ordering::Relaxed) {
                return None;
            }

            let dst = discovery::append_extension(tmp.path(), &item.relative, "wav");

            if cli.dry_run {
                pb_norm.inc(1);
                let raw_dur = item
                    .chapter
                    .as_ref()
                    .map(|c| c.end - c.start)
                    .or_else(|| cache::get_duration(&item.source, &dur_cache))
                    .unwrap_or(60.0);
                let adjusted = raw_dur / cli.speed;
                return Some((item.clone(), dst, adjusted));
            }

            // For chapters, extract the segment first
            let source_for_norm = if let Some(ref ch) = item.chapter {
                let chapter_tmp = dst.with_extension("chapter.wav");
                if let Some(p) = chapter_tmp.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let dur = ch.end - ch.start;
                let st = Command::new("ffmpeg")
                    .args(["-y", "-i"])
                    .arg(&item.source)
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
                    failed_norm.fetch_add(1, Ordering::Relaxed);
                    pb_norm.inc(1);
                    return None;
                }
                chapter_tmp
            } else {
                item.source.clone()
            };

            // Get source duration for progress tracking
            let src_dur = cache::get_duration(&source_for_norm, &dur_cache).unwrap_or(0.0);

            // Pass 1: measure
            let measurement = match ffmpeg::measure_loudness(
                &source_for_norm,
                cli.loudness_i,
                cli.loudness_tp,
                cli.loudness_lra,
                src_dur,
                None,
            ) {
                Some(m) => m,
                None => {
                    if item.chapter.is_some() {
                        let _ = fs::remove_file(&source_for_norm);
                    }
                    failed_norm.fetch_add(1, Ordering::Relaxed);
                    pb_norm.inc(1);
                    return None;
                }
            };

            // Pass 2: apply
            let (ok, _cmd) = ffmpeg::normalize_two_pass(
                &source_for_norm,
                &dst,
                cli.loudness_i,
                cli.loudness_tp,
                cli.loudness_lra,
                cli.sample_rate,
                &measurement,
                src_dur,
                None,
            );

            // Clean up chapter temp
            if item.chapter.is_some() {
                let _ = fs::remove_file(&source_for_norm);
            }

            pb_norm.inc(1);

            if ok {
                let raw_dur = cache::get_duration(&dst, &dur_cache).unwrap_or(0.0);
                let adjusted = raw_dur / cli.speed;
                if adjusted > MIN_DURATION {
                    Some((item.clone(), dst, adjusted))
                } else {
                    failed_norm.fetch_add(1, Ordering::Relaxed);
                    None
                }
            } else {
                failed_norm.fetch_add(1, Ordering::Relaxed);
                None
            }
        })
        .collect();

    pb_norm.finish_with_message("Done");

    let ready: Vec<(InputItem, PathBuf, f64)> = normed.into_iter().flatten().collect();

    let failed = failed_norm.load(Ordering::Relaxed);
    if failed > 0 {
        eprintln!("    ⚠️  {} files failed normalization", failed);
    }

    cache::save(&dur_cache.lock().unwrap(), &cache_path);

    let total_s: f64 = ready.iter().map(|r| r.2).sum();
    println!(
        "    {} files ready ({:.1} s after speed adjustment)\n",
        ready.len(),
        total_s
    );

    {
        let mut l = json_log.lock().unwrap();
        l.input_files = ready.len();
        l.input_duration_s = total_s;
    }

    if ready.is_empty() {
        eprintln!("❌ No files could be normalized.");
        cleanup_and_exit(&cli, &json_log, 1);
    }

    check_cancel(&cancelled, &cli, &json_log);

    // ── 5. Build seamless music plan ───────────────────────────
    println!("🎲  Building seamless music overlay plan...");
    let dur_list: Vec<f64> = ready.iter().map(|r| r.2).collect();
    let plans = plan::build_music_plan(
        &dur_list,
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

    // ── 6. Assemble tasks ──────────────────────────────────────
    let tasks: Vec<FileTask> = ready
        .into_iter()
        .zip(plans)
        .map(|((item, norm, dur), pieces)| {
            let out = output_dir
                .join(&item.relative)
                .with_extension(cli.format.extension());
            FileTask {
                source: item.source,
                normalized: norm,
                relative: item.relative,
                output: out,
                duration: dur,
                pieces,
            }
        })
        .collect();

    // ── 6b. Detect output path collisions ──────────────────────
    {
        let mut seen: HashMap<PathBuf, Vec<&PathBuf>> = HashMap::new();
        for t in &tasks {
            seen.entry(t.output.clone()).or_default().push(&t.relative);
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

    // ── 6c. Copy images and extract covers ─────────────────────
    copy_images_and_extract_covers(&cli.input, &output_dir, &tasks, cli.dry_run);

    // ── 7. Overlay + encode ────────────────────────────────────
    let total_tasks = tasks.len();

    if cli.dry_run {
        println!("🎶  [DRY RUN] Would overlay & encode {total_tasks} files:");
        for t in &tasks {
            println!("      {} → {}", t.relative.display(), t.output.display());
        }
    } else {
        println!("🎶  Overlaying & encoding {total_tasks} files");

        let pb_overlay = progress::create_bar(&mp, total_tasks as u64, "Encoding");
        let failed_overlay = AtomicUsize::new(0);

        tasks.par_iter().for_each(|t| {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }

            match ffmpeg::overlay_music(
                t,
                volume,
                cli.format,
                cli.quality,
                cli.sample_rate,
                cli.speed,
                cli.music_fade_in,
                cli.music_fade_out,
                &cancelled,
                None,
            ) {
                Ok(()) => {
                    let mut l = json_log.lock().unwrap();
                    l.processed.push(LogEntry {
                        file: t.relative.to_string_lossy().into(),
                        error: None,
                        command: None,
                    });
                }
                Err((err, cmd)) => {
                    failed_overlay.fetch_add(1, Ordering::Relaxed);
                    let mut l = json_log.lock().unwrap();
                    l.failed.push(LogEntry {
                        file: t.relative.to_string_lossy().into(),
                        error: Some(err),
                        command: if cmd.is_empty() { None } else { Some(cmd) },
                    });
                }
            }
            pb_overlay.inc(1);
        });

        pb_overlay.finish_with_message("Done");

        let failed = failed_overlay.load(Ordering::Relaxed);
        if failed > 0 {
            eprintln!("\n    ❌ {} files failed encoding", failed);
        }
    }

    // ── Finalize ───────────────────────────────────────────────
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
