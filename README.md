# 🎧 overlay-music

**Overlay background music onto audiobook and podcast files with professional loudness normalization.**

overlay-music processes a directory of audio files by normalizing voice audio (EBU R128 two-pass loudnorm), shuffling and seamlessly overlaying background music, and encoding to your chosen output format. Music plays continuously across files, picking up exactly where it left off — voice stays prominent while music provides ambiance.

Built in Rust for speed. Processes files in parallel with full FFmpeg filter graph control.

---

## ✨ Features

- **Two-pass EBU R128 loudness normalization** — voice tracks are measured then normalized with `linear=true` for transparent, broadcast-quality results
- **Seamless music overlay** — background music is shuffled and plays continuously across files with no gaps or repeats
- **Crossfade or pause between music tracks** — smooth transitions or configurable silence at track boundaries
- **Voice speed control** — speed up narration (e.g. 1.5×) while music stays at its original tempo
- **Chapter-aware splitting** — split m4b/m4a audiobooks by embedded chapter metadata into individual files
- **Multiple output formats** — OGG Vorbis, MP3, Opus, or FLAC
- **Music fade-in/out** — optional fades at the start and end of each output file
- **Cover image handling** — extracts embedded cover art, copies existing images from source directories
- **Resume support** — skip already-processed files to resume interrupted runs
- **Music normalization caching** — normalized music files are cached to a separate directory and reused across runs; use `--force-normalize-music` to redo after changing targets
- **Real-time ffmpeg progress** — normalization and encoding progress bars update in real-time from ffmpeg's `time=` output
- **Music loudness spot-check** — automatically checks if music files differ from the target loudness and suggests `--normalize-music` if needed
- **Concurrent normalization** — music normalization and book normalization run in parallel; per-file overlay starts as soon as that file's book norm is done and all music is ready
- **Adaptive multithreading** — with few large files, spare threads are given to each ffmpeg subprocess for faster encoding/decoding
- **Duration caching** — `.audio_duration_cache.json` speeds up repeated runs
- **Dry-run mode** — preview what would happen without writing any files
- **JSON logging** — machine-readable log for automation pipelines
- **Graceful Ctrl+C** — finishes current tasks, writes logs, cleans up temp files

---

## 📦 Installation

### Prerequisites

- [Rust](https://rustup.rs/) (1.70+)
- [FFmpeg](https://ffmpeg.org/download.html) (must be in PATH — both `ffmpeg` and `ffprobe`)

### Build from source

```bash
git clone https://github.com/menkaur/overlay-music.git
cd overlay-music
cargo build --release
```

The binary will be at `target/release/overlay-music`. Optionally install it:

```bash
cargo install --path .
```

### Verify

```bash
overlay-music --version
overlay-music --help
```

---

## 🚀 Quick Start

```bash
# Basic usage — OGG output with default settings
overlay-music --input ./audiobook --music ./ambient

# Short flags work too
overlay-music -i ./audiobook -m ./ambient
```

This will:
1. Scan `./audiobook` for audio files (recursively, natural sort order)
2. Scan `./ambient` for background music files
3. Normalize all voice files (two-pass loudnorm)
4. Shuffle music and overlay it at 1/3 volume
5. Encode to OGG and write to `./audiobook_processed/`

---

## 📖 Usage Examples

### MP3 output with 1.5× voice speed

```bash
overlay-music -i ./audiobook -m ./music -f mp3 -q 2 --speed 1.5
```

### FLAC lossless with music normalization and crossfades

```bash
overlay-music -i ./audiobook -m ./music -f flac -q 8 \
  --normalize-music --crossfade 3.0
```

### Split m4b audiobook into chapters with music fades

```bash
overlay-music -i ./books -m ./ambient --split-chapters \
  --music-fade-in 2.0 --music-fade-out 2.0
```

### Custom loudness targets with very quiet music

```bash
overlay-music -i ./podcast -m ./bgm -l 6.0 \
  --loudness-i -14.0 --loudness-tp -1.0 --loudness-lra 7.0
```

### Resume an interrupted run with JSON log

```bash
overlay-music -i ./audiobook -m ./music --resume --log run.json
```

### Dry run — preview without processing

```bash
overlay-music -i ./audiobook -m ./music --dry-run
```

### Opus at 160kbps with 8 threads and 2s pause between music tracks

```bash
overlay-music -i ./episodes -m ./music -f opus -q 6 -t 8 --pause 2.0
```

### 2× voice speed, quieter music, 48kHz sample rate

```bash
overlay-music -i ./lectures -m ./ambient --speed 2.0 -l 5.0 \
  --sample-rate 48000
```

### Skip input normalization (overlay on original files)

```bash
overlay-music -i ./audiobook -m ./music --normalize-input=false
```

### Normalize music to a custom directory

```bash
overlay-music -i ./audiobook -m ./music --normalize-music \
  --normalize-music-output ./normalized_bgm
```

### Re-normalize music after changing loudness targets

```bash
overlay-music -i ./audiobook -m ./music --normalize-music \
  --force-normalize-music
```

### Custom output directory

```bash
overlay-music -i ./raw -m ./music -o ./finished
```

---

## ⚙️ Options Reference

| Flag | Long | Default | Description |
|------|------|---------|-------------|
| `-i` | `--input` | *required* | Input directory containing audio files |
| `-m` | `--music` | *required* | Music directory containing background music |
| `-o` | `--output` | `<input>_processed/` | Output directory |
| `-l` | `--loudness-drop` | `3.0` | Music volume divisor (music vol = 1/this). Higher = quieter music |
| `-t` | `--threads` | `48` | Parallel threads. With few large files, spare threads are given to each ffmpeg subprocess |
| `-p` | `--pause` | `0.0` | Silence between music tracks (seconds) |
| | `--crossfade` | `0.0` | Crossfade between music tracks (seconds). Supersedes `--pause` |
| `-f` | `--format` | `ogg` | Output format: `ogg`, `mp3`, `opus`, `flac` |
| `-q` | `--quality` | `6` | Encoder quality (0–10 for ogg/mp3/opus, 0–12 for flac) |
| | `--sample-rate` | `44100` | Output sample rate in Hz |
| | `--loudness-i` | `-16.0` | Target integrated loudness (LUFS) |
| | `--loudness-tp` | `-1.5` | True peak limit (dBTP) |
| | `--loudness-lra` | `11.0` | Loudness range target (LU) |
| | `--music-fade-in` | `0.0` | Fade in music at start of each file (seconds) |
| | `--music-fade-out` | `0.0` | Fade out music at end of each file (seconds) |
| | `--normalize-input` | `true` | Normalize input audio (set `false` to overlay on originals) |
| | `--normalize-music` | `false` | Also normalize music tracks to the same loudness target |
| | `--normalize-music-output` | `<music>_normalized/` | Output directory for normalized music files |
| | `--force-normalize-music` | `false` | Re-normalize all music even if cached versions exist |
| | `--resume` | `false` | Skip files whose output already exists |
| | `--dry-run` | `false` | Preview without writing files |
| | `--split-chapters` | `true` | Split m4b/m4a by embedded chapter metadata |
| | `--log` | *none* | Write JSON log to this file |
| | `--speed` | `1.0` | Voice speed multiplier (0.5–100.0). Music is unaffected |

---

## 🎵 Supported Formats

### Input (voice & music)

MP3, WAV, OGG, FLAC, AAC, M4A, M4B, WMA, Opus, WebM

### Output

| Format | Encoder | Quality meaning |
|--------|---------|-----------------|
| OGG | libvorbis | 0 (lowest) – 10 (highest) |
| MP3 | libmp3lame | 0 (best) – 10 (smallest) |
| Opus | libopus | Maps to 64–224 kbps |
| FLAC | flac | 0 (fast) – 12 (smallest) |

---

## 🔧 How It Works

```
Input Files          Background Music
     │                      │
     ▼                      ▼
 ┌────────┐          ┌────────────┐
 │ Probe  │          │   Probe    │
 │ + Cache│          │  + Shuffle │
 └───┬────┘          └─────┬──────┘
     │                      │
     │                      ▼
     │               ┌────────────┐
     │               │ Spot-check │
     │               │ loudness   │
     │               └─────┬──────┘
     │                      │
     ▼                      ▼
 ┌─────────────────────────────────────┐
 │        CONCURRENT PIPELINE          │
 │                                     │
 │  ┌───────────────┐  ┌───────────┐  │
 │  │ Music norm    │  │ Per-file: │  │
 │  │ (dedicated    │  │  extract  │  │
 │  │  thread)      │  │  chapter  │  │
 │  │       │       │  │  → norm   │  │
 │  │       ▼       │  │  → wait   │  │
 │  │ Build music ──┼──│→ overlay  │  │
 │  │ plan  (ready  │  │  → encode │  │
 │  │  signal)      │  │ (rayon)   │  │
 │  └───────────────┘  └───────────┘  │
 │                                     │
 │  FFmpeg filter graph per file:      │
 │    Voice → [atempo] ──┐            │
 │    Music → concat →   │            │
 │      fade → volume ─┐ │            │
 │           amix ◄─────┘ │            │
 │             ◄──────────┘            │
 │        alimiter → encode            │
 └──────────┬──────────────────────────┘
            │
            ▼
      Output Files
    + cover.jpg
    + copied images
```

### Key Design Decisions

- **Concurrent normalization** — music normalization runs in a dedicated OS thread while book normalization runs in the rayon pool; per-file overlay starts as soon as that file's book norm is done and all music is ready
- **Adaptive ffmpeg threading** — when there are fewer files than threads, spare threads are given to each ffmpeg subprocess (`threads / min(threads, files)`). E.g., 48 threads with 3 files → 16 threads per ffmpeg; 48 threads with 200 files → 1 thread per ffmpeg
- **Two-pass loudnorm with `linear=true`** — avoids the pumping artifacts of single-pass normalization
- **`amix=duration=first:normalize=0`** — voice determines output length; no automatic volume halving
- **`alimiter=limit=1`** — prevents clipping when voice and music peaks coincide
- **Voice speed via `atempo`** — changes tempo without pitch shift; music is unaffected
- **Music plan uses adjusted duration** — when voice is sped up, music is planned for the shorter output duration
- **Global music cursor** — `(track_index, position)` carries across files for seamless playback
- **Input-level seeking** — `-ss` and `-t` placed before `-i` for fast seeking

---

## 📂 Directory Structure

### Input

```
audiobook/
├── Chapter 01.mp3
├── Chapter 02.mp3
├── subfolder/
│   ├── Chapter 03.mp3
│   └── cover.jpg          ← copied to output
└── folder.png              ← copied to output
```

### Output

```
audiobook_processed/
├── Chapter 01.ogg
├── Chapter 02.ogg
├── subfolder/
│   ├── Chapter 03.ogg
│   └── cover.jpg          ← copied from input
├── folder.png              ← copied from input
└── cover.jpg               ← extracted from audio metadata
```

---

## 🖼️ Cover Image Handling

overlay-music handles cover images in two ways:

1. **Copies all image files** (jpg, png, bmp, gif, webp, tiff, svg) from the input directory tree to the output, preserving relative paths
2. **Extracts embedded cover art** from audio files (common in m4b, mp3, flac) and saves as `cover.jpg` in each output subdirectory

This ensures Android audiobook players (Smart AudioBook Player, Listen Audiobook Player, etc.) can find and display cover art. Images that already exist in the output are skipped, making the process resume-friendly.

---

## ⏩ Voice Speed

The `--speed` flag changes the playback speed of the voice track without affecting pitch (using FFmpeg's `atempo` filter). Background music always plays at its original tempo.

```bash
# 1.5× faster narration
overlay-music -i ./book -m ./music --speed 1.5

# 2× speed
overlay-music -i ./book -m ./music --speed 2.0

# Slow down to 75%
overlay-music -i ./book -m ./music --speed 0.75
```

When speed is applied:
- A 60-minute file at 1.5× becomes 40 minutes of output
- Music is planned for the 40-minute output duration
- Music transitions between files remain seamless

---

## 📋 JSON Log

Use `--log run.json` to produce a machine-readable log:

```json
{
  "started": "2025-03-31T10:30:00Z",
  "finished": "2025-03-31T10:45:23Z",
  "input_dir": "./audiobook",
  "music_dir": "./music",
  "output_dir": "./audiobook_processed",
  "settings": {
    "loudness_drop": 3.0,
    "threads": 48,
    "pause": 0.0,
    "crossfade": 0.0,
    "format": "ogg",
    "quality": 6,
    "sample_rate": 44100,
    "loudness_i": -16.0,
    "loudness_tp": -1.5,
    "loudness_lra": 11.0,
    "music_fade_in": 0.0,
    "music_fade_out": 0.0,
    "normalize_input": true,
    "normalize_music": false,
    "normalize_music_output": null,
    "split_chapters": true,
    "speed": 1.0
  },
  "music_files": 12,
  "music_duration_s": 3600.0,
  "input_files": 24,
  "input_duration_s": 28800.0,
  "processed": [
    { "file": "Chapter 01.mp3" },
    { "file": "Chapter 02.mp3" }
  ],
  "skipped": [],
  "failed": []
}
```

---

## 🔄 Resuming

If processing is interrupted (Ctrl+C, crash, power loss), use `--resume` to skip files that already exist in the output directory:

```bash
overlay-music -i ./audiobook -m ./music --resume
```

The duration cache (`.audio_duration_cache.json`) is saved after each stage, so re-runs skip expensive ffprobe calls for unchanged files.

---

## 🏗️ Project Structure

```
overlay-music/
├── Cargo.toml
└── src/
    ├── main.rs          # Thin orchestrator — phase sequencing, Ctrl+C, log init
    ├── cache.rs         # Duration cache (JSON persistence)
    ├── chapters.rs      # Chapter detection via ffprobe
    ├── cli.rs           # CLI argument parsing, validation, chapter expansion, resume
    ├── cover.rs         # Cover image extraction
    ├── discovery.rs     # File discovery and path helpers
    ├── ffmpeg.rs        # FFmpeg workers (normalize, overlay, filter graphs)
    ├── images.rs        # Image copying and cover extraction orchestration
    ├── log.rs           # JSON log structures
    ├── music_norm.rs    # Music normalization with caching
    ├── pipeline.rs      # Per-file streaming pipeline (parallel processing)
    ├── plan.rs          # Music overlay planner
    ├── progress.rs      # Progress bar helpers (per-worker bars)
    └── time.rs          # Lightweight ISO 8601 timestamps
```

---

## 📄 License

MIT

---

## 🙏 Acknowledgments

- [FFmpeg](https://ffmpeg.org/) — the backbone of all audio processing
- [clap](https://github.com/clap-rs/clap) — command-line argument parsing
- [rayon](https://github.com/rayon-rs/rayon) — parallel processing
- [indicatif](https://github.com/console-rs/indicatif) — progress bars
- [walkdir](https://github.com/BurntSushi/walkdir) — recursive directory traversal
- [natord](https://github.com/lifthrasiir/rust-natord) — natural sort order
