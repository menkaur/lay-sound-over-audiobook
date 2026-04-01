# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`overlay-music` is a Rust CLI tool that overlays background music onto audiobook/podcast files with EBU R128 loudness normalization. It shells out to `ffmpeg`/`ffprobe` for all audio processing — there is no Rust audio decoding.

## Build & Run

```bash
cargo build --release          # release binary at target/release/overlay-music
cargo run --release -- --help  # run with args
cargo install --path .         # install to ~/.cargo/bin
```

No tests exist in this project. Requires `ffmpeg` and `ffprobe` in PATH.

## Architecture

Single-binary CLI with all source in `src/`. The binary name is `overlay-music` (set in Cargo.toml `[package] name`).

### Processing Pipeline (orchestrated in `main.rs`)

1. **Discovery** (`discovery.rs`) — recursively finds audio files, natural-sorts them via `natord`
2. **Chapter splitting** (`chapters.rs`) — optionally splits m4b/m4a by embedded chapter metadata (via `ffprobe -print_format json`)
3. **Normalization** (`ffmpeg.rs`) — two-pass EBU R128 loudnorm with `linear=true` (first pass measures, second pass applies)
4. **Music planning** (`plan.rs`) — builds a `MusicPlan` for each file: which music segments to use, with crossfade/pause. A global music cursor `(track_index, position)` carries across files for seamless playback
5. **Overlay** (`ffmpeg.rs`) — constructs complex FFmpeg filter graphs: voice atempo + music concat + amix + alimiter + encode
6. **Cover/images** (`cover.rs`, `main.rs`) — extracts embedded cover art and copies image files to output

### Key Design Patterns

- **All audio work is FFmpeg subprocesses** — `std::process::Command` calls throughout `ffmpeg.rs`. No audio crate dependencies.
- **Duration cache** (`cache.rs`) — JSON file `.audio_duration_cache.json` persists ffprobe duration results across runs to avoid re-probing.
- **Parallel processing** via `rayon` — thread count configurable via `--threads`.
- **Graceful Ctrl+C** — `ctrlc` crate sets an `AtomicBool`; checked between stages. Writes JSON log and cleans up on exit.
- **Music normalization caching** — normalized music files are written to a separate directory and reused across runs; duration tolerance checks (`NORM_DURATION_TOLERANCE_S`, `NORM_DURATION_TOLERANCE_PCT`) detect stale cached files.

### Module Responsibilities

- `ffmpeg.rs` — the largest module. Contains `FileTask` struct, `normalize_file`, `overlay_music`, FFmpeg filter graph construction, and `OutputFormat` enum (ogg/mp3/opus/flac).
- `plan.rs` — `MusicPlan` and `MusicSegment` types. Handles crossfade vs pause logic, music track wrapping.
- `log.rs` — `JsonLog` and `LogEntry` serde structs for `--log` output.
- `progress.rs` — `indicatif` progress bar helpers.
- `time.rs` — minimal ISO 8601 timestamp without pulling in `chrono`.

### CLI Parsing

Uses `clap` derive API. The `Args` struct in `main.rs` defines all flags. Output format is an enum `OutputFormat` in `ffmpeg.rs` with `ValueEnum` derive.
