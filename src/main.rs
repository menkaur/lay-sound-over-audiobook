use clap::Parser;
use rand::seq::SliceRandom;
use rand::thread_rng;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use walkdir::WalkDir;

// ─── Constants ─────────────────────────────────────────────────

const CACHE_FILE: &str = ".audio_duration_cache.json";
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "flac", "aac", "m4a", "m4b", "wma", "opus", "webm",
];
/// Minimum duration (seconds) for a music file to be considered usable.
const MIN_DURATION: f64 = 0.01;
/// Canonical format applied to every stream entering concat / amix
/// so that sample rate, sample format, and channel layout all match.
const AFMT: &str = "aresample=44100,aformat=sample_fmts=fltp:channel_layouts=stereo";

// ─── CLI Arguments ─────────────────────────────────────────────

/// Drop-ins: overlay background music onto input audio files.
#[derive(Parser, Debug)]
#[command(name = "drop-ins", version, about)]
struct Args {
    /// Input directory containing audio files (searched recursively, natural sort)
    #[arg(short, long)]
    input: PathBuf,

    /// Music directory containing background music files
    #[arg(short, long)]
    music: PathBuf,

    /// Output directory (preserves input directory structure).
    /// Defaults to <input>_processed/
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Loudness drop for music (music volume = 1 / this_value)
    #[arg(short = 'l', long = "loudness-drop", default_value_t = 3.0)]
    loudness_drop: f64,

    /// Number of processing threads
    #[arg(short = 't', long, default_value_t = 48)]
    threads: usize,

    /// Pause between consecutive music tracks (seconds)
    #[arg(short, long, default_value_t = 0.0)]
    pause: f64,
}

// ─── Duration Cache ────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default, Clone)]
struct DurationCache {
    entries: HashMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    duration: f64,
    modified: u64,
    size: u64,
}

fn load_cache(path: &Path) -> DurationCache {
    path.exists()
        .then(|| {
            fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        })
        .flatten()
        .unwrap_or_default()
}

fn save_cache(cache: &DurationCache, path: &Path) {
    if let Ok(data) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(path, data);
    }
}

/// Return (mtime_secs, file_size) for cache validation.
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
fn get_duration_cached(path: &Path, cache: &Arc<Mutex<DurationCache>>) -> Option<f64> {
    let key = path.to_string_lossy().to_string();
    let (modified, size) = file_metadata_key(path)?;

    // Fast path: cache hit
    {
        let c = cache.lock().unwrap();
        if let Some(e) = c.entries.get(&key) {
            if e.modified == modified && e.size == size {
                return Some(e.duration);
            }
        }
    }

    // Slow path: probe with ffprobe
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

// ─── File Discovery ────────────────────────────────────────────

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Recursively collect audio files with **natural** sort order.
fn collect_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .map(|e| e.into_path())
        .collect();
    v.sort_by(|a, b| natord::compare(&a.to_string_lossy(), &b.to_string_lossy()));
    v
}

/// Collect audio files (order doesn't matter for music).
fn collect_unsorted(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_audio(e.path()))
        .map(|e| e.into_path())
        .collect()
}

// ─── Path Helpers ──────────────────────────────────────────────

/// Build a path that appends `ext` *after* the full original filename,
/// preventing collisions when two source files share a stem but have
/// different extensions (e.g. track.mp3 → track.mp3.wav,
/// track.flac → track.flac.wav).
fn append_extension(base_dir: &Path, rel: &Path, ext: &str) -> PathBuf {
    let parent = rel.parent().unwrap_or(Path::new(""));
    let mut fname: OsString = rel.file_name().unwrap_or_default().to_os_string();
    fname.push(".");
    fname.push(ext);
    base_dir.join(parent).join(fname)
}

/// Derive the default output directory from the input path:
///   recordings/       → recordings_processed/
///   /data/episodes    → /data/episodes_processed/
///   ./my.files/       → ./my.files_processed/
fn default_output_dir(input: &Path) -> PathBuf {
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

// ─── Music Overlay Plan ────────────────────────────────────────

#[derive(Debug, Clone)]
enum MusicPiece {
    Segment {
        file: PathBuf,
        start: f64,
        duration: f64,
    },
    Silence {
        duration: f64,
    },
}

/// For every input file (given as its duration), compute which music
/// segments to overlay.  A global cursor across the shuffled playlist
/// ensures seamless playback: when one input file ends, the next one
/// picks up the music exactly where the previous left off.
fn build_music_plan(
    input_durations: &[f64],
    music_files: &[PathBuf],
    music_durations: &[f64],
    pause: f64,
) -> Vec<Vec<MusicPiece>> {
    assert!(
        !music_files.is_empty(),
        "build_music_plan called with no music"
    );

    let mut rng = thread_rng();
    let mut playlist: Vec<usize> = (0..music_files.len()).collect();
    playlist.shuffle(&mut rng);

    let mut tidx: usize = 0; // index into playlist
    let mut tpos: f64 = 0.0; // position inside current track
    let mut plans = Vec::with_capacity(input_durations.len());

    for &dur in input_durations {
        let mut remaining = dur;
        let mut pieces: Vec<MusicPiece> = Vec::new();

        // Safety cap: every productive iteration consumes ≥ 0.001s,
        // non-consuming iterations bounded by playlist len per cycle.
        let max_iters = (music_files.len() + 2).saturating_mul((dur / 0.001) as usize + 2);
        let mut iters = 0usize;

        while remaining > 0.001 {
            iters += 1;
            if iters > max_iters {
                eprintln!(
                    "⚠️  Music plan safety limit hit for a {dur:.2}s input — \
                     truncating music overlay"
                );
                break;
            }

            // If we exhausted the playlist, reshuffle and restart
            if tidx >= playlist.len() {
                playlist.shuffle(&mut rng);
                tidx = 0;
                tpos = 0.0;
            }

            let mi = playlist[tidx];
            let avail = music_durations[mi] - tpos;

            if avail > 0.001 {
                let take = remaining.min(avail);
                pieces.push(MusicPiece::Segment {
                    file: music_files[mi].clone(),
                    start: tpos,
                    duration: take,
                });
                tpos += take;
                remaining -= take;
            }

            // Advance to next track when the current one is used up
            if music_durations[playlist[tidx]] - tpos < 0.001 {
                tidx += 1;
                tpos = 0.0;

                // Optional silence between music tracks
                if remaining > 0.001 && pause > 0.001 {
                    let p = remaining.min(pause);
                    pieces.push(MusicPiece::Silence { duration: p });
                    remaining -= p;
                }
            }
        }
        plans.push(pieces);
    }
    plans
}

// ─── FFmpeg Workers ────────────────────────────────────────────

/// Apply EBU R128 loudnorm to an input file → WAV temp file.
fn normalize_file(input: &Path, output: &Path) -> bool {
    if let Some(p) = output.parent() {
        let _ = fs::create_dir_all(p);
    }
    let st = Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(input)
        .args([
            "-vn",
            "-af",
            "loudnorm=I=-16:TP=-1.5:LRA=11",
            "-ar",
            "44100",
            "-ac",
            "2",
        ])
        .arg(output)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(st, Ok(s) if s.success())
}

struct FileTask {
    normalized: PathBuf,
    relative: PathBuf,
    output: PathBuf,
    pieces: Vec<MusicPiece>,
}

/// Build and run a single ffmpeg command that mixes the normalised
/// voice file with the required music segments and writes OGG Vorbis.
fn overlay_music(task: &FileTask, volume: f64) -> bool {
    if let Some(p) = task.output.parent() {
        let _ = fs::create_dir_all(p);
    }

    // Trivial case: no music pieces at all
    if task.pieces.is_empty() {
        let st = Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(&task.normalized)
            .args(["-codec:a", "libvorbis", "-q:a", "6"])
            .arg(&task.output)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        return matches!(st, Ok(s) if s.success());
    }

    // ── Build the complex-filter ffmpeg invocation ──────────────
    let mut args: Vec<String> = vec!["-y".into()];

    // Input 0 – normalised voice
    args.push("-i".into());
    args.push(task.normalized.to_string_lossy().into());

    let mut inp: usize = 1; // next input index
    let mut parts: Vec<String> = vec![];
    let mut labels: Vec<String> = vec![];
    let mut sil_n: usize = 0;

    for piece in &task.pieces {
        match piece {
            MusicPiece::Segment {
                file,
                start,
                duration,
            } => {
                args.extend([
                    "-ss".into(),
                    format!("{start:.6}"),
                    "-t".into(),
                    format!("{duration:.6}"),
                    "-i".into(),
                    file.to_string_lossy().into(),
                ]);
                let l = format!("m{inp}");
                // Resample + reformat + reset PTS for clean concat
                parts.push(format!("[{inp}:a]{AFMT},asetpts=PTS-STARTPTS[{l}]"));
                labels.push(format!("[{l}]"));
                inp += 1;
            }
            MusicPiece::Silence { duration } => {
                let l = format!("z{sil_n}");
                // Generate silence with SAME format as segments so
                // concat sees uniform streams.  anullsrc defaults to
                // sample_fmt=dbl, so explicit aformat is required.
                parts.push(format!(
                    "anullsrc=r=44100:cl=stereo[{l}r];\
                     [{l}r]atrim=0:{duration:.6},\
                     asetpts=PTS-STARTPTS,\
                     aformat=sample_fmts=fltp:channel_layouts=stereo[{l}]"
                ));
                labels.push(format!("[{l}]"));
                sil_n += 1;
            }
        }
    }

    let n = labels.len();
    let filter = if n == 1 {
        // Single piece – no concat needed
        format!(
            "{p};\
             {seg}volume={volume:.6}[mv];\
             [0:a]{AFMT}[v];\
             [v][mv]amix=inputs=2:duration=first:normalize=0,\
             alimiter=limit=1[out]",
            p = parts.join(";"),
            seg = labels[0],
        )
    } else {
        // Multiple pieces – concat then mix
        format!(
            "{p};\
             {segs}concat=n={n}:v=0:a=1[mc];\
             [mc]volume={volume:.6}[mv];\
             [0:a]{AFMT}[v];\
             [v][mv]amix=inputs=2:duration=first:normalize=0,\
             alimiter=limit=1[out]",
            p = parts.join(";"),
            segs = labels.join(""),
        )
    };

    args.extend([
        "-filter_complex".into(),
        filter,
        "-map".into(),
        "[out]".into(),
        "-codec:a".into(),
        "libvorbis".into(),
        "-q:a".into(),
        "6".into(),
        task.output.to_string_lossy().into(),
    ]);

    let st = Command::new("ffmpeg")
        .args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    matches!(st, Ok(s) if s.success())
}

// ─── Main ──────────────────────────────────────────────────────

fn main() {
    let cli = Args::parse();

    // ── Resolve output directory ───────────────────────────────
    // If not provided, default to <input>_processed/
    let output_dir: PathBuf = cli.output.unwrap_or_else(|| default_output_dir(&cli.input));

    // ── Validate arguments ─────────────────────────────────────
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
    if !cli.input.is_dir() {
        eprintln!("❌ Input directory does not exist: {:?}", cli.input);
        std::process::exit(1);
    }
    if !cli.music.is_dir() {
        eprintln!("❌ Music directory does not exist: {:?}", cli.music);
        std::process::exit(1);
    }

    // Validate external dependencies
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

    println!("📁  Output directory: {:?}", output_dir);

    // Global rayon thread pool
    rayon::ThreadPoolBuilder::new()
        .num_threads(cli.threads)
        .build_global()
        .expect("failed to build thread pool");

    let cache_path = PathBuf::from(CACHE_FILE);
    let cache = Arc::new(Mutex::new(load_cache(&cache_path)));

    // ── 1. Music: collect & probe durations (parallel) ─────────
    println!("🎵  Scanning music dir: {:?}", cli.music);
    let music_all = collect_unsorted(&cli.music);
    println!(
        "    Found {} music files – probing durations…",
        music_all.len()
    );

    let music_ok: Vec<(PathBuf, f64)> = music_all
        .par_iter()
        .filter_map(|f| Some((f.clone(), get_duration_cached(f, &cache)?)))
        .collect();
    save_cache(&cache.lock().unwrap(), &cache_path);

    // Filter out degenerate zero/near-zero duration tracks to prevent
    // infinite loops in the music planner.
    let (music_files, music_durs): (Vec<_>, Vec<_>) = music_ok
        .into_iter()
        .filter(|(_, d)| *d > MIN_DURATION)
        .unzip();

    let total_music_s: f64 = music_durs.iter().sum();
    println!(
        "    {} usable tracks  ({:.1} s total)\n",
        music_files.len(),
        total_music_s
    );

    if music_files.is_empty() {
        eprintln!("❌ No usable music found.");
        std::process::exit(1);
    }

    // ── 2. Input: collect files in natural order ───────────────
    println!("📂  Scanning input dir: {:?}", cli.input);
    let inputs = collect_sorted(&cli.input);
    println!("    {} input files (natural-sorted)\n", inputs.len());

    if inputs.is_empty() {
        eprintln!("❌ No input files found.");
        std::process::exit(1);
    }

    // ── 3. Normalise (loudnorm) to temp dir (parallel) ────────
    let tmp = tempfile::tempdir().expect("failed to create temp dir");
    println!(
        "🔊  Normalising {} files (loudnorm → {:?})…",
        inputs.len(),
        tmp.path()
    );

    let norm_counter = AtomicUsize::new(0);
    let total = inputs.len();

    // par_iter().map().collect() preserves element order.
    let normed: Vec<Option<(PathBuf, PathBuf)>> = inputs
        .par_iter()
        .map(|src| {
            let rel = src.strip_prefix(&cli.input).unwrap_or(src).to_path_buf();
            // Append ".wav" after the full filename to avoid collisions
            // when two files share a stem (e.g. track.mp3 → track.mp3.wav,
            // track.flac → track.flac.wav).
            let dst = append_extension(tmp.path(), &rel, "wav");
            let ok = normalize_file(src, &dst);

            let n = norm_counter.fetch_add(1, Ordering::Relaxed) + 1;
            if ok {
                println!("    ✅ [{n}/{total}] {}", rel.display());
                Some((rel, dst))
            } else {
                eprintln!("    ❌ [{n}/{total}] FAILED {}", rel.display());
                None
            }
        })
        .collect();

    let normed: Vec<(PathBuf, PathBuf)> = normed.into_iter().flatten().collect();

    if normed.is_empty() {
        eprintln!("❌ All normalisation jobs failed.");
        std::process::exit(1);
    }

    // ── 4. Get durations of normalised files (parallel) ────────
    println!("\n⏱️   Probing normalised durations…");

    let norm_durs: Vec<Option<f64>> = normed
        .par_iter()
        .map(|(_, n)| get_duration_cached(n, &cache))
        .collect();
    save_cache(&cache.lock().unwrap(), &cache_path);

    let ready: Vec<(PathBuf, PathBuf, f64)> = normed
        .into_iter()
        .zip(norm_durs)
        .filter_map(|((rel, norm), d)| d.map(|d| (rel, norm, d)))
        .collect();

    let total_s: f64 = ready.iter().map(|r| r.2).sum();
    println!("    {} files ready ({:.1} s)\n", ready.len(), total_s);

    if ready.is_empty() {
        eprintln!("❌ No normalised files could be probed.");
        std::process::exit(1);
    }

    // ── 5. Build seamless music plan (sequential) ──────────────
    println!("🎲  Building seamless music overlay plan…");
    let dur_list: Vec<f64> = ready.iter().map(|r| r.2).collect();
    let plans = build_music_plan(&dur_list, &music_files, &music_durs, cli.pause);

    let volume = 1.0 / cli.loudness_drop;
    println!(
        "    Music volume = {volume:.4}  (1 / {:.1})\n",
        cli.loudness_drop
    );

    // ── 6. Assemble tasks ──────────────────────────────────────
    let tasks: Vec<FileTask> = ready
        .into_iter()
        .zip(plans)
        .map(|((rel, norm, _dur), pieces)| {
            let out = output_dir.join(&rel).with_extension("ogg");
            FileTask {
                normalized: norm,
                relative: rel,
                output: out,
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
                "⚠️  Warning: {} output path(s) have collisions \
                 (input files with same stem but different extension):",
                dupes.len()
            );
            for (out, srcs) in &dupes {
                let names: Vec<_> = srcs.iter().map(|s| s.display().to_string()).collect();
                eprintln!("      {} ← {}", out.display(), names.join(", "));
            }
            eprintln!(
                "    Later files will overwrite earlier ones. \
                 Consider renaming conflicting input files.\n"
            );
        }
    }

    // ── 7. Overlay + encode to OGG (parallel) ──────────────────
    let total_tasks = tasks.len();
    let done_counter = AtomicUsize::new(0);

    println!(
        "🎶  Overlaying & encoding {total_tasks} files → {:?}",
        output_dir
    );

    tasks.par_iter().for_each(|t| {
        let ok = overlay_music(t, volume);
        let n = done_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if ok {
            println!("    ✅ [{n}/{total_tasks}] {}", t.relative.display());
        } else {
            eprintln!("    ❌ [{n}/{total_tasks}] FAILED {}", t.relative.display());
        }
    });

    // tempdir is cleaned up on drop
    println!("\n✨  All done!  Output in {:?}", output_dir);
}
