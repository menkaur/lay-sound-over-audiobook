//! CLI argument parsing, input item expansion, and validation.

use crate::chapters;
use crate::discovery;
use crate::ffmpeg::OutputFormat;
use crate::log::JsonLog;
use clap::Parser;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

// ─── CLI Arguments ─────────────────────────────────────────────
#[derive(Parser, Debug)]
#[command(
    name = "overlay-music",
    version,
    about = "🎧 Overlay background music onto audiobook/podcast files with loudness normalization",
    long_about = "\
🎧 overlay-music — Overlay background music onto audiobook/podcast files

Processes a directory of audio files by:
  1. Normalizing voice audio (EBU R128 two-pass loudnorm)
  2. Shuffling and seamlessly overlaying background music
  3. Encoding to the chosen output format

Music plays continuously across files, picking up exactly where it
left off. Voice and music are mixed so the voice stays prominent
while music provides ambiance.",
    after_long_help = "\
EXAMPLES:
  Basic usage (defaults: ogg format, quality 6, 44100 Hz):
    overlay-music --input ./audiobook --music ./ambient

  MP3 output at high quality with 1.5x voice speed:
    overlay-music -i ./audiobook -m ./music -f mp3 -q 2 --speed 1.5

  FLAC lossless with music normalization and crossfades:
    overlay-music -i ./audiobook -m ./music -f flac -q 8 \\
      --normalize-music --crossfade 3.0

  Split m4b audiobook into chapters with music fades:
    overlay-music -i ./books -m ./ambient --split-chapters \\
      --music-fade-in 2.0 --music-fade-out 2.0

  Custom loudness targets with quiet music:
    overlay-music -i ./podcast -m ./bgm -l 5.0 \\
      --loudness-i -14.0 --loudness-tp -1.0 --loudness-lra 7.0

  Resume interrupted processing with JSON log:
    overlay-music -i ./audiobook -m ./music --resume --log run.json

  Preview what would happen without processing:
    overlay-music -i ./audiobook -m ./music --dry-run

  Opus output at 160kbps with 8 threads and 2s pause between tracks:
    overlay-music -i ./episodes -m ./music -f opus -q 6 -t 8 --pause 2.0

  Speed up voice 2x, lower music more, custom sample rate:
    overlay-music -i ./lectures -m ./ambient --speed 2.0 -l 4.0 \\
      --sample-rate 48000

  Skip input normalization (overlay on original files):
    overlay-music -i ./audiobook -m ./music --normalize-input=false

  Normalize music to a custom directory:
    overlay-music -i ./audiobook -m ./music --normalize-music \\
      --normalize-music-output ./normalized_bgm

  Re-normalize music even if cached versions exist:
    overlay-music -i ./audiobook -m ./music --normalize-music \\
      --force-normalize-music

NOTES:
  • Voice speed (--speed) only affects voice; music plays at normal tempo
  • --crossfade supersedes --pause when both are specified
  • Cover images are extracted from source files and placed as cover.jpg
  • All image files (jpg/png/etc) from the input tree are copied to output
  • The duration cache (.audio_duration_cache.json) speeds up re-runs
  • Chapter splitting works best with m4b/m4a files containing chapter metadata
  • Normalized music files are cached by default; use --force-normalize-music
    to re-normalize (e.g. after changing loudness targets)"
)]
pub struct Args {
    /// Input directory containing audio files (searched recursively, natural sort)
    #[arg(short, long)]
    pub input: PathBuf,

    /// Music directory containing background music files
    #[arg(short, long)]
    pub music: PathBuf,

    /// Output directory [default: <input>_processed/]
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Loudness drop for music (music volume = 1 / this_value) [higher = quieter music]
    #[arg(short = 'l', long = "loudness-drop", default_value_t = 3.0)]
    pub loudness_drop: f64,

    /// Number of parallel processing threads.
    /// With many small files, each file gets its own thread.
    /// With few large files, spare threads are given to each ffmpeg
    /// subprocess for faster decoding/encoding.
    #[arg(short = 't', long, default_value_t = 48)]
    pub threads: usize,

    /// Silence between music tracks in seconds (ignored if --crossfade > 0)
    #[arg(short, long, default_value_t = 0.0)]
    pub pause: f64,

    /// Crossfade between music tracks in seconds (supersedes --pause)
    #[arg(long, default_value_t = 0.0)]
    pub crossfade: f64,

    /// Output audio format
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Ogg)]
    pub format: OutputFormat,

    /// Output quality (0–10 for ogg/mp3/opus, 0–12 for flac) [lower = better for mp3]
    #[arg(short, long, default_value_t = 6)]
    pub quality: u8,

    /// Output sample rate in Hz
    #[arg(long, default_value_t = 44100)]
    pub sample_rate: u32,

    /// Target integrated loudness in LUFS (EBU R128)
    #[arg(long, default_value_t = -16.0)]
    pub loudness_i: f64,

    /// True peak limit in dBTP
    #[arg(long, default_value_t = -1.5)]
    pub loudness_tp: f64,

    /// Loudness range target in LU
    #[arg(long, default_value_t = 11.0)]
    pub loudness_lra: f64,

    /// Fade in music at the start of each output file (seconds)
    #[arg(long, default_value_t = 0.0)]
    pub music_fade_in: f64,

    /// Fade out music at the end of each output file (seconds)
    #[arg(long, default_value_t = 0.0)]
    pub music_fade_out: f64,

    /// Normalize input audio files (two-pass loudnorm).
    /// Set to false to overlay music on original (non-normalized) files.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub normalize_input: bool,

    /// Also normalize music tracks to the same loudness target
    #[arg(long, default_value_t = false)]
    pub normalize_music: bool,

    /// Output directory for normalized music files (only used with --normalize-music).
    /// [default: <music>_normalized/]
    #[arg(long)]
    pub normalize_music_output: Option<PathBuf>,

    /// Force re-normalization of all music files, even if normalized versions
    /// already exist in the output directory and pass duration checks.
    /// Useful after changing loudness targets or sample rate.
    #[arg(long, default_value_t = false)]
    pub force_normalize_music: bool,

    /// Skip files whose output already exists (resume interrupted runs)
    #[arg(long, default_value_t = false)]
    pub resume: bool,

    /// Show what would be done without writing any files
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,

    /// Split m4b/m4a audiobooks by embedded chapter metadata
    #[arg(long, default_value_t = true)]
    pub split_chapters: bool,

    /// Write a machine-readable JSON log to this file
    #[arg(long)]
    pub log: Option<PathBuf>,

    /// Speed up voice audio (0.5–100.0, default 1.0). Music tempo is unaffected.
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,
}

// ─── Input Item ────────────────────────────────────────────────

/// A single input file (or chapter segment) to process.
#[derive(Debug, Clone)]
pub struct InputItem {
    pub source: PathBuf,
    pub relative: PathBuf,
    pub chapter: Option<chapters::Chapter>,
}

// ─── Validation ────────────────────────────────────────────────

/// Validate all CLI arguments, exiting on error.
pub fn validate_args(cli: &Args) {
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
    if cli.normalize_music_output.is_some() && !cli.normalize_music {
        eprintln!("ℹ️  --normalize-music-output requires --normalize-music; ignoring");
    }
    if cli.force_normalize_music && !cli.normalize_music {
        eprintln!("ℹ️  --force-normalize-music requires --normalize-music; ignoring");
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

// ─── Chapter Expansion ─────────────────────────────────────────

/// Expand input files into InputItems, optionally splitting by chapters.
pub fn expand_chapters(cli: &Args, input_files: &[PathBuf]) -> Vec<InputItem> {
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

// ─── Resume Filter ─────────────────────────────────────────────

/// Filter out already-processed files when --resume is set.
pub fn filter_resume(
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
