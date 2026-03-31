//! JSON log structures — machine-readable output for automation pipelines.
//! Written on normal exit, early exit, and Ctrl+C via `cleanup_and_exit`.

use serde::Serialize;

#[derive(Serialize, Default)]
pub struct JsonLog {
    pub started: String,
    pub finished: Option<String>,
    pub input_dir: String,
    pub music_dir: String,
    pub output_dir: String,
    pub settings: LogSettings,
    pub music_files: usize,
    pub music_duration_s: f64,
    pub input_files: usize,
    pub input_duration_s: f64,
    pub processed: Vec<LogEntry>,
    pub skipped: Vec<String>,
    pub failed: Vec<LogEntry>,
}

#[derive(Serialize, Default)]
pub struct LogSettings {
    pub loudness_drop: f64,
    pub threads: usize,
    pub pause: f64,
    pub crossfade: f64,
    pub format: String,
    pub quality: u8,
    pub sample_rate: u32,
    pub loudness_i: f64,
    pub loudness_tp: f64,
    pub loudness_lra: f64,
    pub music_fade_in: f64,
    pub music_fade_out: f64,
    pub normalize_input: bool,
    pub normalize_music: bool,
    pub normalize_music_output: Option<String>,
    pub split_chapters: bool,
    pub speed: f64,
}

#[derive(Serialize, Clone)]
pub struct LogEntry {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}
