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
- **Parallel processing** — configurable thread count for fast batch processing
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
4. Shuffle music and overlay it at 1/4 volume
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
| `-l` | `--loudness-drop` | `4.0` | Music volume divisor (music vol = 1/this). Higher = quieter music |
| `-t` | `--threads` | `48` | Number of parallel processing threads |
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
| | `--normalize-music` | `false` | Also normalize music tracks |
| | `--resume` | `false` | Skip files whose output already exists |
| | `--dry-run` | `false` | Preview without writing files |
| | `--split-chapters` | `false` | Split m4b/m4a by embedded chapter metadata |
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
     ▼                      │
 ┌────────────┐             │
 │ Split by   │             │
 │ Chapters   │             │
 │ (optional) │             │
 └───┬────────┘             │
     │                      │
     ▼                      │
 ┌────────────┐             │
 │ Two-Pass   │             │
 │ Loudnorm   │             │
 │ (EBU R128) │             │
 └───┬────────┘             │
     │                      │
     ▼                      ▼
 ┌──────────────────────────────┐
 │     Build Music Plan         │
 │  (seamless cursor across     │
 │   files, crossfade/pause)    │
 └──────────┬───────────────────┘
            │
            ▼
 ┌──────────────────────────────┐
 │   FFmpeg Filter Graph        │
 │                              │
 │  Voice: afmt → [atempo] ──┐ │
 │                            │ │
 │  Music: segments → concat  │ │
 │    → fade → volume ──────┐│ │
 │                          ││ │
 │              amix ◄──────┘│ │
 │                ◄──────────┘ │
 │                │            │
 │           alimiter          │
 │                │            │
 │             encode          │
 └──────────┬───────────────────┘
            │
            ▼
      Output Files
    + cover.jpg
    + copied images
```

### Key Design Decisions

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
    "loudness_drop": 4.0,
    "threads": 48,
    "format": "ogg",
    "speed": 1.5
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
    ├── main.rs          # CLI, orchestration, image copying
    ├── cache.rs         # Duration cache (JSON persistence)
    ├── chapters.rs      # Chapter detection via ffprobe
    ├── cover.rs         # Cover image extraction
    ├── discovery.rs     # File discovery and path helpers
    ├── ffmpeg.rs        # FFmpeg workers (normalize, overlay, progress)
    ├── log.rs           # JSON log structures
    ├── plan.rs          # Music overlay planner
    ├── progress.rs      # Progress bar helpers
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


### References

1. **GitHub - Mrhuma/Mrhumas-Music-Overlay: A program for showing which song is playing in YouTube Music Desktop Player or Spotify. · GitHub**. [https://github.com](https://github.com/Mrhuma/Mrhumas-Music-Overlay)
2. **GitHub - TomasBisciak/Windows-Loudness-Equalization-toggle: This application creates a TOGGLE KEY, to enable and disable this feature as needed by pressing just one key, since you won’t want to use it while you listen to music and other content where audio levels are preferred to be unbalanced.**. [https://github.com](https://github.com/TomasBisciak/Windows-Loudness-Equalization-toggle)
3. **GitHub - gentoo-audio/audio-overlay: Gentoo overlay for music production · GitHub**. [https://github.com](https://github.com/gentoo-audio/audio-overlay)
4. **GitHub - kklobe/normalize: an audio file volume normalizer · GitHub**. [https://github.com](https://github.com/kklobe/normalize)
5. **8.1 oreo - Music app overlay with control on volume level change - Android Enthusiasts Stack Exchange**. [https://android.stackexchange.com](https://android.stackexchange.com/questions/204075/music-app-overlay-with-control-on-volume-level-change)
6. **Normalize volume level with PulseAudio · GitHub**. [https://gist.github.com](https://gist.github.com/lightrush/4fc5b36e01db8fae534b0ea6c16e347f)
7. **GitHub - grisys83/LoudnessCompensator · GitHub**. [https://github.com](https://github.com/grisys83/LoudnessCompensator)
8. **Sound mixer "loudness equalization" on a per-app basis? Solved - Windows 10 Forums**. [https://www.tenforums.com](https://www.tenforums.com/sound-audio/191227-sound-mixer-loudness-equalization-per-app-basis.html)
9. **Free - Music on stream - A web based current song / now playing overlay | OBS Forums**. [https://obsproject.com](https://obsproject.com/forum/resources/music-on-stream-a-web-based-current-song-now-playing-overlay.1920/)
10. **How to play sounds in github?**. [https://stackoverflow.com](https://stackoverflow.com/questions/70813487/how-to-play-sounds-in-github)
