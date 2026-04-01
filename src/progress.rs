//! Progress bar helpers using the `indicatif` crate.
//!
//! Two main APIs:
//!
//! * [`create_bar`] — simple phase-level bar (files completed / total).
//! * [`WorkerBars`] — per-worker bars showing real-time ffmpeg progress
//!   (file name + time position) alongside an overall counter.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

/// Create a styled progress bar and add it to the `MultiProgress` group.
pub fn create_bar(mp: &MultiProgress, total: u64, msg: &str) -> ProgressBar {
    let pb = mp.add(ProgressBar::new(total));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("█▓░"),
    );
    pb.set_message(msg.to_string());
    pb
}

// ─── Per-Worker Bars ──────────────────────────────────────────

/// Maximum number of visible per-worker progress bars.
const MAX_WORKER_BARS: usize = 16;

/// Per-worker progress bars for the streaming pipeline.
///
/// An overall bar tracks total files processed, while a pool of
/// smaller worker bars shows what each rayon thread is doing in
/// real-time — file name, processing phase, and ffmpeg time progress.
///
/// # Usage
///
/// ```ignore
/// let w = WorkerBars::new(&mp, 100, 8, "Processing");
///
/// files.par_iter().for_each(|f| {
///     w.begin_phase("ch03.m4b", 300_000, "norm₁");
///     ffmpeg::measure_loudness(..., w.current_pb());
///
///     w.begin_phase("ch03.m4b", 300_000, "norm₂");
///     ffmpeg::normalize_two_pass(..., w.current_pb());
///
///     w.begin_phase("ch03.m4b", 300_000, "mix");
///     ffmpeg::overlay_music(..., w.current_pb());
///
///     w.complete_file();
/// });
///
/// w.finish_all("Done");
/// ```
pub struct WorkerBars {
    pub overall: ProgressBar,
    bars: Vec<ProgressBar>,
}

impl WorkerBars {
    /// Create a new set of worker bars added to `mp`.
    ///
    /// * `total` — total number of files to process.
    /// * `num_threads` — rayon global thread pool size.
    /// * `msg` — label for the overall bar.
    pub fn new(mp: &MultiProgress, total: u64, num_threads: usize, msg: &str) -> Self {
        let overall = mp.add(ProgressBar::new(total));
        overall.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}  {elapsed_precise}",
                )
                .unwrap()
                .progress_chars("█▓░"),
        );
        overall.set_message(msg.to_string());
        overall.enable_steady_tick(std::time::Duration::from_millis(200));

        let n = num_threads.min(total as usize).min(MAX_WORKER_BARS);
        let style = ProgressStyle::default_bar()
            .template("    ▸ [{bar:20.green/dim}] {msg}")
            .unwrap()
            .progress_chars("━╸─");

        let bars: Vec<ProgressBar> = (0..n)
            .map(|_| {
                let pb = mp.add(ProgressBar::new(0));
                pb.set_style(style.clone());
                pb
            })
            .collect();

        Self { overall, bars }
    }

    /// Get the worker bar for the current rayon thread.
    fn worker_bar(&self) -> Option<&ProgressBar> {
        if self.bars.is_empty() {
            return None;
        }
        let idx = rayon::current_thread_index().unwrap_or(0) % self.bars.len();
        Some(&self.bars[idx])
    }

    /// Begin a new processing phase for a file on the current worker.
    ///
    /// Resets the worker bar and sets it up for ffmpeg progress tracking.
    ///
    /// * `name` — short file name for display.
    /// * `duration_ms` — expected duration in milliseconds (bar length).
    /// * `phase` — label like `"norm₁"`, `"norm₂"`, `"mix"`.
    pub fn begin_phase(&self, name: &str, duration_ms: u64, phase: &str) {
        if let Some(pb) = self.worker_bar() {
            pb.set_length(duration_ms);
            pb.set_position(0);
            pb.set_message(format!("{phase} {name}"));
        }
    }

    /// Get the current worker's progress bar reference.
    ///
    /// Pass this to ffmpeg functions (`measure_loudness`, `normalize_two_pass`,
    /// `overlay_music`) so they can update position from `time=` output.
    pub fn current_pb(&self) -> Option<&ProgressBar> {
        self.worker_bar()
    }

    /// Mark the current file as complete and advance the overall counter.
    pub fn complete_file(&self) {
        if let Some(pb) = self.worker_bar() {
            pb.set_message(String::new());
            pb.set_position(0);
            pb.set_length(0);
        }
        self.overall.inc(1);
    }

    /// Finish and clear all bars.
    pub fn finish_all(&self, msg: &str) {
        for pb in &self.bars {
            pb.finish_and_clear();
        }
        self.overall.finish_with_message(msg.to_string());
    }
}
