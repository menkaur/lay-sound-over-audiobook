//! Music overlay planner — computes which music segments to overlay
//! on each input file, maintaining a seamless global cursor across
//! the shuffled playlist.

use rand::seq::SliceRandom;
use rand::thread_rng;
use std::path::PathBuf;

/// A single piece of the music overlay for one input file.
#[derive(Debug, Clone)]
pub enum MusicPiece {
    /// A segment from a music file.
    Segment {
        file: PathBuf,
        start: f64,
        duration: f64,
    },
    /// Generated silence between tracks.
    Silence { duration: f64 },
    /// Crossfade between the end of one track and the start of another.
    Crossfade {
        file1: PathBuf,
        start1: f64,
        dur1: f64,
        file2: PathBuf,
        start2: f64,
        dur2: f64,
        overlap: f64,
    },
}

/// Build a music overlay plan for every input file.
///
/// - `input_durations`: output duration of each input file (after speed adjustment).
/// - `music_files` / `music_durations`: aligned vecs of music paths and durations.
/// - `pause`: silence between music tracks (ignored if `crossfade > 0`).
/// - `crossfade`: crossfade duration between music tracks.
///
/// The global cursor `(tidx, tpos)` carries across input files so that
/// music playback is seamless: when file N ends, file N+1 picks up
/// exactly where the music left off.
pub fn build_music_plan(
    input_durations: &[f64],
    music_files: &[PathBuf],
    music_durations: &[f64],
    pause: f64,
    crossfade: f64,
) -> Vec<Vec<MusicPiece>> {
    assert!(!music_files.is_empty());
    assert_eq!(music_files.len(), music_durations.len());

    let mut rng = thread_rng();
    let mut playlist: Vec<usize> = (0..music_files.len()).collect();
    playlist.shuffle(&mut rng);

    let mut tidx: usize = 0;
    let mut tpos: f64 = 0.0;
    let mut plans = Vec::with_capacity(input_durations.len());

    for &dur in input_durations {
        let mut remaining = dur;
        let mut pieces: Vec<MusicPiece> = Vec::new();

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

            // Track boundary reached
            if music_durations[playlist[tidx]] - tpos < 0.001 {
                let prev_mi = playlist[tidx];
                tidx += 1;
                tpos = 0.0;

                if remaining > 0.001 {
                    if tidx >= playlist.len() {
                        playlist.shuffle(&mut rng);
                        tidx = 0;
                    }

                    let mut did_crossfade = false;

                    if crossfade > 0.001 {
                        let next_mi = playlist[tidx];
                        let cf = crossfade
                            .min(remaining)
                            .min(music_durations[prev_mi])
                            .min(music_durations[next_mi]);

                        if cf > 0.01 {
                            // Shorten preceding segment to avoid double-playing tail
                            if let Some(MusicPiece::Segment { duration, .. }) = pieces.last_mut() {
                                if *duration > cf + 0.001 {
                                    *duration -= cf;
                                    remaining += cf;
                                }
                            }

                            pieces.push(MusicPiece::Crossfade {
                                file1: music_files[prev_mi].clone(),
                                start1: (music_durations[prev_mi] - cf).max(0.0),
                                dur1: cf,
                                file2: music_files[next_mi].clone(),
                                start2: 0.0,
                                dur2: cf,
                                overlap: cf,
                            });
                            tpos = cf;
                            remaining -= cf;
                            did_crossfade = true;
                        }
                    }

                    if !did_crossfade && pause > 0.001 {
                        let p = remaining.min(pause);
                        pieces.push(MusicPiece::Silence { duration: p });
                        remaining -= p;
                    }
                }
            }
        }
        plans.push(pieces);
    }
    plans
}
