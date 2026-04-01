//! FFmpeg workers — two-pass loudnorm normalization, music overlay
//! encoding, speed control, and audio format helpers.
//!
//! All ffmpeg/ffprobe interactions are in this module. We shell out
//! to the `ffmpeg` CLI rather than using FFmpeg bindings, keeping
//! the dependency tree small and the build simple.
//!
//! Progress tracking: normalization (pass 1 + pass 2) and overlay
//! functions accept an optional `ProgressBar` and `expected_duration`.
//! FFmpeg's stderr is read line-by-line; the `time=` field is parsed
//! and used to update the bar in real-time.

use crate::plan::MusicPiece;
use clap::ValueEnum;
use indicatif::ProgressBar;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

// ─── Output Format ─────────────────────────────────────────────

/// Supported output audio formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Ogg,
    Mp3,
    Opus,
    Flac,
}

impl OutputFormat {
    /// File extension for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            OutputFormat::Ogg => "ogg",
            OutputFormat::Mp3 => "mp3",
            OutputFormat::Opus => "opus",
            OutputFormat::Flac => "flac",
        }
    }

    /// FFmpeg encoder arguments for this format at the given quality level.
    pub fn encoder_args(&self, quality: u8) -> Vec<String> {
        match self {
            OutputFormat::Ogg => vec![
                "-codec:a".into(),
                "libvorbis".into(),
                "-q:a".into(),
                quality.to_string(),
            ],
            OutputFormat::Mp3 => vec![
                "-codec:a".into(),
                "libmp3lame".into(),
                "-q:a".into(),
                quality.to_string(),
            ],
            OutputFormat::Opus => vec![
                "-codec:a".into(),
                "libopus".into(),
                "-b:a".into(),
                format!("{}k", 64 + (quality as u32) * 16),
            ],
            OutputFormat::Flac => vec![
                "-codec:a".into(),
                "flac".into(),
                "-compression_level".into(),
                quality.to_string(),
            ],
        }
    }

    /// Maximum valid quality value for this format.
    pub fn max_quality(&self) -> u8 {
        match self {
            OutputFormat::Flac => 12,
            _ => 10,
        }
    }
}

// ─── Audio Format String ───────────────────────────────────────

/// Build the canonical audio format filter string for the given sample rate.
///
/// This ensures every stream entering `concat` / `amix` has identical
/// sample rate, sample format (`fltp`), and channel layout (`stereo`).
fn afmt(sample_rate: u32) -> String {
    format!(
        "aresample={},aformat=sample_fmts=fltp:channel_layouts=stereo",
        sample_rate
    )
}

// ─── FFmpeg Progress Parsing ───────────────────────────────────

/// Parse a `HH:MM:SS.ms` time string into seconds.
fn parse_ffmpeg_time(s: &str) -> Option<f64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// Extract the `time=...` value from an ffmpeg stderr line.
///
/// Example line:
/// ```text
/// size=  1024kB time=00:01:30.00 bitrate=128.0kbits/s speed=2.5x
/// ```
fn extract_progress_time(line: &str) -> Option<f64> {
    let idx = line.find("time=")?;
    let rest = &line[idx + 5..];
    let end = rest
        .find([' ', '\r', '\n'])
        .unwrap_or(rest.len());
    let time_str = &rest[..end];
    // Skip negative times (startup) and N/A
    if time_str.starts_with('-') || time_str == "N/A" {
        return None;
    }
    parse_ffmpeg_time(time_str)
}

/// Update a progress bar based on ffmpeg time output.
/// Bar length is `expected_duration * 1000` (milliseconds).
fn update_progress(pb: &ProgressBar, time: f64, expected_duration: f64) {
    if expected_duration > 0.001 {
        let pos = (time * 1000.0).min(expected_duration * 1000.0) as u64;
        pb.set_position(pos);
    }
}

/// Read ffmpeg stderr, splitting on `\r` and `\n` (ffmpeg uses `\r` for
/// real-time progress updates). Updates progress bar and optionally
/// captures the full output text.
fn read_ffmpeg_stderr(
    stderr: impl Read,
    expected_duration: f64,
    pb: Option<&ProgressBar>,
    capture: bool,
) -> String {
    let mut reader = BufReader::new(stderr);
    let mut chunk = [0u8; 4096];
    let mut line_buf = Vec::with_capacity(256);
    let mut output = String::new();

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                for &b in &chunk[..n] {
                    if b == b'\r' || b == b'\n' {
                        if !line_buf.is_empty() {
                            let line = String::from_utf8_lossy(&line_buf);
                            if let Some(time) = extract_progress_time(&line) {
                                if let Some(pb) = pb {
                                    update_progress(pb, time, expected_duration);
                                }
                            }
                            if capture {
                                output.push_str(&line);
                                output.push('\n');
                            }
                            line_buf.clear();
                        }
                    } else {
                        line_buf.push(b);
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    // Flush remaining bytes
    if !line_buf.is_empty() {
        let line = String::from_utf8_lossy(&line_buf);
        if let Some(time) = extract_progress_time(&line) {
            if let Some(pb) = pb {
                update_progress(pb, time, expected_duration);
            }
        }
        if capture {
            output.push_str(&line);
            output.push('\n');
        }
    }
    output
}

/// Spawn ffmpeg, read stderr for `time=` progress, optionally update
/// a progress bar. Returns `true` if ffmpeg exited successfully.
fn run_ffmpeg_tracked(args: &[String], expected_duration: f64, pb: Option<&ProgressBar>) -> bool {
    let mut child = match Command::new("ffmpeg")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    if let Some(stderr) = child.stderr.take() {
        read_ffmpeg_stderr(stderr, expected_duration, pb, false);
    }

    matches!(child.wait(), Ok(s) if s.success())
}

/// Spawn ffmpeg, read stderr for both progress and full text content.
/// Returns `(success, full_stderr)`. Needed for loudnorm pass 1 where
/// the JSON measurement is printed to stderr.
fn run_ffmpeg_tracked_capture(
    args: &[String],
    expected_duration: f64,
    pb: Option<&ProgressBar>,
) -> (bool, String) {
    let mut child = match Command::new("ffmpeg")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return (false, String::new()),
    };

    let full_stderr = child
        .stderr
        .take()
        .map(|stderr| read_ffmpeg_stderr(stderr, expected_duration, pb, true))
        .unwrap_or_default();

    let success = matches!(child.wait(), Ok(s) if s.success());
    (success, full_stderr)
}

// ─── Two-Pass Loudnorm ─────────────────────────────────────────

/// EBU R128 loudness target parameters.
#[derive(Debug, Clone, Copy)]
pub struct LoudnessTarget {
    /// Integrated loudness in LUFS.
    pub i: f64,
    /// True peak limit in dBTP.
    pub tp: f64,
    /// Loudness range in LU.
    pub lra: f64,
}

/// Measurements from loudnorm pass 1.
#[derive(Debug, Clone)]
pub struct LoudnormMeasurement {
    pub input_i: f64,
    pub input_tp: f64,
    pub input_lra: f64,
    pub input_thresh: f64,
    pub target_offset: f64,
}

/// Pass 1: measure loudness characteristics.
///
/// Runs `ffmpeg -af loudnorm=...print_format=json -f null -` and
/// parses the JSON block that loudnorm prints to stderr.
/// Uses `rfind` to grab the **last** JSON block, avoiding any
/// metadata JSON that ffmpeg might print earlier.
///
/// If `pb` is provided and `expected_duration > 0`, updates the
/// progress bar in real-time based on ffmpeg's `time=` output.
pub fn measure_loudness(
    input: &Path,
    target: &LoudnessTarget,
    expected_duration: f64,
    pb: Option<&ProgressBar>,
    ffmpeg_threads: u32,
) -> Option<LoudnormMeasurement> {
    let filter = format!(
        "loudnorm=I={}:TP={}:LRA={}:print_format=json",
        target.i, target.tp, target.lra
    );

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-y".into(),
    ];
    if ffmpeg_threads > 1 {
        args.extend(["-threads".into(), ffmpeg_threads.to_string()]);
    }
    args.extend([
        "-i".into(),
        input.to_string_lossy().into(),
        "-af".into(),
        filter,
        "-f".into(),
        "null".into(),
        "-".into(),
    ]);

    let (_success, full_stderr) = run_ffmpeg_tracked_capture(&args, expected_duration, pb);

    // Parse the loudnorm JSON from the end of stderr
    let json_end = full_stderr.rfind('}')? + 1;
    let json_start = full_stderr[..json_end].rfind('{')?;
    if json_start >= json_end {
        return None;
    }
    let json_str = &full_stderr[json_start..json_end];
    let json: serde_json::Value = serde_json::from_str(json_str).ok()?;

    Some(LoudnormMeasurement {
        input_i: json["input_i"].as_str()?.parse().ok()?,
        input_tp: json["input_tp"].as_str()?.parse().ok()?,
        input_lra: json["input_lra"].as_str()?.parse().ok()?,
        input_thresh: json["input_thresh"].as_str()?.parse().ok()?,
        target_offset: json["target_offset"].as_str()?.parse().ok()?,
    })
}

/// Combined normalization settings — loudness target plus encoding params.
#[derive(Debug, Clone, Copy)]
pub struct NormConfig {
    pub target: LoudnessTarget,
    pub sample_rate: u32,
    pub ffmpeg_threads: u32,
}

/// Pass 2: apply loudnorm with measured values and `linear=true`.
///
/// Returns `(success, command_args)` — the command args are retained
/// so they can be logged on failure for debugging.
///
/// If `pb` is provided and `expected_duration > 0`, updates the
/// progress bar in real-time.
pub fn normalize_two_pass(
    input: &Path,
    output: &Path,
    config: &NormConfig,
    measurement: &LoudnormMeasurement,
    expected_duration: f64,
    pb: Option<&ProgressBar>,
) -> (bool, Vec<String>) {
    if let Some(p) = output.parent() {
        let _ = fs::create_dir_all(p);
    }

    let filter = format!(
        "loudnorm=I={}:TP={}:LRA={}:\
         measured_I={}:measured_TP={}:measured_LRA={}:\
         measured_thresh={}:offset={}:linear=true",
        config.target.i,
        config.target.tp,
        config.target.lra,
        measurement.input_i,
        measurement.input_tp,
        measurement.input_lra,
        measurement.input_thresh,
        measurement.target_offset
    );

    let mut cmd_args: Vec<String> = vec!["-y".into()];
    if config.ffmpeg_threads > 1 {
        cmd_args.extend(["-threads".into(), config.ffmpeg_threads.to_string()]);
    }
    cmd_args.extend([
        "-i".into(),
        input.to_string_lossy().into(),
        "-vn".into(),
        "-af".into(),
        filter,
        "-ar".into(),
        config.sample_rate.to_string(),
        "-ac".into(),
        "2".into(),
        output.to_string_lossy().into(),
    ]);

    let success = run_ffmpeg_tracked(&cmd_args, expected_duration, pb);
    (success, cmd_args)
}

// ─── Speed (atempo) ────────────────────────────────────────────

/// Build an atempo filter chain for the given speed.
///
/// FFmpeg's `atempo` filter only accepts values between 0.5 and 2.0
/// per instance, so we chain multiple filters for values outside
/// that range.
///
/// Examples:
/// - `speed=1.0` → `""` (empty, no filter needed)
/// - `speed=1.5` → `"atempo=1.500000"`
/// - `speed=3.0` → `"atempo=2.0,atempo=1.500000"`
/// - `speed=0.3` → `"atempo=0.5,atempo=0.600000"`
///
/// Returns empty string if speed ≈ 1.0.
pub fn build_atempo_chain(speed: f64) -> String {
    if (speed - 1.0).abs() < 0.001 {
        return String::new();
    }

    let mut remaining = speed;
    let mut parts: Vec<String> = vec![];

    while remaining > 2.0 + 0.001 {
        parts.push("atempo=2.0".into());
        remaining /= 2.0;
    }
    while remaining < 0.5 - 0.001 {
        parts.push("atempo=0.5".into());
        remaining /= 0.5;
    }

    parts.push(format!("atempo={remaining:.6}"));
    parts.join(",")
}

// ─── File Task ─────────────────────────────────────────────────

/// All information needed to produce one output file.
pub struct FileTask {
    /// Normalised WAV in the temp directory.
    pub normalized: PathBuf,
    /// Final output path.
    pub output: PathBuf,
    /// Output duration (after speed adjustment) — used for music plan & fades.
    pub duration: f64,
    /// Music pieces to overlay on this file.
    pub pieces: Vec<MusicPiece>,
}

// ─── Overlay + Encode ──────────────────────────────────────────

/// Configuration for the overlay + encode step.
pub struct OverlayConfig {
    pub volume: f64,
    pub format: OutputFormat,
    pub quality: u8,
    pub sample_rate: u32,
    pub speed: f64,
    pub music_fade_in: f64,
    pub music_fade_out: f64,
    /// Number of threads for each ffmpeg subprocess.
    /// When there are few large files, giving each ffmpeg more threads
    /// helps utilise the full CPU.  Computed as
    /// `max(1, pool_threads / min(pool_threads, num_files))`.
    pub ffmpeg_threads: u32,
}

/// Build and run a single ffmpeg command that mixes the normalised
/// voice file with the required music segments and writes the final
/// encoded output.
///
/// The voice track gets the `atempo` speed filter (if speed ≠ 1.0).
/// The music track is unaffected by speed — it plays at its original
/// tempo.
///
/// If `pb` is provided, updates it in real-time based on ffmpeg's
/// `time=` stderr output.
///
/// Returns `Ok(())` on success, or `Err((error_message, full_command))`
/// on failure so the caller can log the failing command.
pub fn overlay_music(
    task: &FileTask,
    config: &OverlayConfig,
    cancelled: &AtomicBool,
    pb: Option<&ProgressBar>,
) -> Result<(), (String, String)> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(("Cancelled".into(), String::new()));
    }

    if let Some(p) = task.output.parent() {
        let _ = fs::create_dir_all(p);
    }

    let afmt_str = afmt(config.sample_rate);
    let atempo = build_atempo_chain(config.speed);

    // Build the voice processing chain: format [+ speed]
    let voice_chain = if atempo.is_empty() {
        format!("[0:a]{afmt_str}[v]")
    } else {
        format!("[0:a]{afmt_str},{atempo}[v]")
    };

    // ── Trivial case: no music pieces ──────────────────────────
    // Still apply afmt for consistent sample rate + format output,
    // and atempo if speed ≠ 1.0.
    if task.pieces.is_empty() {
        let voice_filter = if atempo.is_empty() {
            afmt_str.clone()
        } else {
            format!("{afmt_str},{atempo}")
        };

        let mut cmd_args: Vec<String> = vec!["-y".into()];
        if config.ffmpeg_threads > 1 {
            cmd_args.extend(["-threads".into(), config.ffmpeg_threads.to_string()]);
        }
        cmd_args.extend(["-i".into(), task.normalized.to_string_lossy().into()]);
        cmd_args.extend(["-af".into(), voice_filter]);
        cmd_args.extend(config.format.encoder_args(config.quality));
        cmd_args.push(task.output.to_string_lossy().into());

        let ok = run_ffmpeg_tracked(&cmd_args, task.duration, pb);

        return if ok {
            Ok(())
        } else {
            Err((
                "ffmpeg failed".into(),
                format!("ffmpeg {}", cmd_args.join(" ")),
            ))
        };
    }

    // ── Build complex filter ───────────────────────────────────
    let mut cmd_args: Vec<String> = vec!["-y".into()];
    if config.ffmpeg_threads > 1 {
        cmd_args.extend(["-threads".into(), config.ffmpeg_threads.to_string()]);
    }

    // Input 0 – normalised voice
    cmd_args.push("-i".into());
    cmd_args.push(task.normalized.to_string_lossy().into());

    let mut inp: usize = 1; // next input index
    let mut parts: Vec<String> = vec![]; // filter graph lines
    let mut labels: Vec<String> = vec![]; // labels for concat
    let mut sil_n: usize = 0; // silence counter
    let mut cf_n: usize = 0; // crossfade counter

    for piece in &task.pieces {
        match piece {
            MusicPiece::Segment {
                file,
                start,
                duration,
            } => {
                // Add input with seek + duration (input-level seeking)
                cmd_args.extend([
                    "-ss".into(),
                    format!("{start:.6}"),
                    "-t".into(),
                    format!("{duration:.6}"),
                    "-i".into(),
                    file.to_string_lossy().into(),
                ]);
                let l = format!("m{inp}");
                // Resample + reformat + reset PTS for clean concat
                parts.push(format!("[{inp}:a]{afmt_str},asetpts=PTS-STARTPTS[{l}]"));
                labels.push(format!("[{l}]"));
                inp += 1;
            }
            MusicPiece::Silence { duration } => {
                let l = format!("z{sil_n}");
                // Generate silence with SAME format as segments so
                // concat sees uniform streams.
                parts.push(format!(
                    "anullsrc=r={}:cl=stereo[{l}r];\
                     [{l}r]atrim=0:{duration:.6},\
                     asetpts=PTS-STARTPTS,\
                     aformat=sample_fmts=fltp:channel_layouts=stereo[{l}]",
                    config.sample_rate
                ));
                labels.push(format!("[{l}]"));
                sil_n += 1;
            }
            MusicPiece::Crossfade {
                file1,
                start1,
                dur1,
                file2,
                start2,
                dur2,
                overlap,
            } => {
                // End of track 1
                cmd_args.extend([
                    "-ss".into(),
                    format!("{start1:.6}"),
                    "-t".into(),
                    format!("{dur1:.6}"),
                    "-i".into(),
                    file1.to_string_lossy().into(),
                ]);
                let l1 = format!("cf{cf_n}a");
                parts.push(format!("[{inp}:a]{afmt_str},asetpts=PTS-STARTPTS[{l1}]"));
                inp += 1;

                // Start of track 2
                cmd_args.extend([
                    "-ss".into(),
                    format!("{start2:.6}"),
                    "-t".into(),
                    format!("{dur2:.6}"),
                    "-i".into(),
                    file2.to_string_lossy().into(),
                ]);
                let l2 = format!("cf{cf_n}b");
                parts.push(format!("[{inp}:a]{afmt_str},asetpts=PTS-STARTPTS[{l2}]"));
                inp += 1;

                // Crossfade the two segments
                let l = format!("cf{cf_n}");
                parts.push(format!(
                    "[{l1}][{l2}]acrossfade=d={overlap:.6}:c1=tri:c2=tri[{l}]"
                ));
                labels.push(format!("[{l}]"));
                cf_n += 1;
            }
        }
    }

    // ── Assemble the music chain ───────────────────────────────
    let n = labels.len();

    let music_chain = if n == 1 {
        // Single piece — no concat needed
        labels[0].to_string()
    } else {
        // Multiple pieces — concat then continue
        format!("{}concat=n={n}:v=0:a=1[mc];[mc]", labels.join(""))
    };

    // ── Optional fade filters on the music bus ─────────────────
    let mut fade_filters = String::new();
    if config.music_fade_in > 0.001 {
        fade_filters.push_str(&format!("afade=t=in:d={:.6},", config.music_fade_in));
    }
    if config.music_fade_out > 0.001 {
        let fade_start = (task.duration - config.music_fade_out).max(0.0);
        fade_filters.push_str(&format!(
            "afade=t=out:st={:.6}:d={:.6},",
            fade_start, config.music_fade_out
        ));
    }

    // ── Complete filter graph ──────────────────────────────────
    //
    // Structure:
    //   [music segments] → concat → fade → volume → [mv]
    //   [voice] → afmt [→ atempo] → [v]
    //   [v] + [mv] → amix → alimiter → [out]
    //
    let filter = format!(
        "{parts};\
         {music_chain}{fade_filters}volume={vol:.6}[mv];\
         {voice_chain};\
         [v][mv]amix=inputs=2:duration=first:normalize=0,\
         alimiter=limit=1[out]",
        parts = parts.join(";"),
        vol = config.volume,
    );

    // ── Use filter_complex_script for very long filters ────────
    let use_script = filter.len() > 8000;
    let script_path = if use_script {
        let path = task.output.with_extension("filter.txt");
        if let Some(p) = path.parent() {
            let _ = fs::create_dir_all(p);
        }
        if let Err(e) = fs::write(&path, &filter) {
            return Err((format!("Failed to write filter script: {e}"), String::new()));
        }
        Some(path)
    } else {
        None
    };

    if let Some(ref script) = script_path {
        cmd_args.extend([
            "-filter_complex_script".into(),
            script.to_string_lossy().into(),
        ]);
    } else {
        cmd_args.extend(["-filter_complex".into(), filter.clone()]);
    }

    // ── Output mapping and encoding ────────────────────────────
    cmd_args.extend(["-map".into(), "[out]".into()]);
    cmd_args.extend(config.format.encoder_args(config.quality));
    cmd_args.push(task.output.to_string_lossy().into());

    // ── Run ffmpeg with progress tracking ──────────────────────
    let ok = run_ffmpeg_tracked(&cmd_args, task.duration, pb);

    // Clean up filter script file
    if let Some(script) = script_path {
        let _ = fs::remove_file(script);
    }

    if ok {
        Ok(())
    } else {
        Err((
            "ffmpeg failed".into(),
            format!("ffmpeg {}", cmd_args.join(" ")),
        ))
    }
}
