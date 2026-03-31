//! Progress bar helpers using the `indicatif` crate.

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
