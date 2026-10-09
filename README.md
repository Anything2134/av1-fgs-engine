
# av1-fgs-engine

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

An automated, high-performance Film Grain Synthesis (FGS) table generator written in Rust for **SVT-AV1**, **aomenc**, and **Av1an**.

---

## Overview

AV1 introduces **Film Grain Synthesis (FGS)**, an innovative feature that removes grain during video encoding to save bitrate and synthesizes it back at the decoder using an autoregressive (AR) mathematical model.

Traditional approaches often rely on:
1. **Digital Photon Noise** (`--photon-noise`), which injects independent, harsh white noise ($Lag = 0$) and thermal color specks into chroma channels.
2. **Encoder-side estimation** (`--film-grain`), which incurs heavy CPU denoising penalties and frequently crushes fine animation lines (*line boiling*) or lifts shadow blacks.

`av1-fgs-engine` was created to solve these limitations. It models the microscopic, high-density physical structure of **large-format 70mm celluloid film**, delivering clean, organic grain that is virtually imperceptible to the human eye while providing natural dithering against 8/10-bit color banding.

Written entirely in **Rust** with native SIMD auto-vectorization and multithreaded Rayon parallelism, `av1-fgs-engine` processes frames directly in memory at high speeds.

---

## Key Features

- **70mm Celluloid Emulation**: Autoregressive correlation ($Lag = 2$ or $3$) with strict luma-focused haloid density curves and clean, zero-noise chroma channels (`sCb 0`, `sCr 0`).
- **Versatile Spectral Profiling**: Continuous noise measurement across 8 luminance bins without hardcoded stock restrictions.
- **VapourSynth Native Ingestion**: Streams directly from `.vpy` scripts via `vspipe`, analyzing post-filter, tonemapped, and resized frames.
- **Av1an Seamless Chunking**: Native ingestion of `scenes.csv` to ensure multi-worker encoders share identical grain boundaries without temporal drift.
- **Temporal Lookahead Window**: Temporal smoothing buffer to eliminate grain popping between camera cuts.
- **Strict AOM/SVT-AV1 Compliance**: 100% compliant with the `filmgrn1` format specification.

---

## Installation & Compilation

Ensure you have Rust (`cargo`), `ffmpeg`, and optionally `vapoursynth` (`vspipe`) installed.

```bash
# Clone the repository
git clone https://github.com/Anything2134/av1-fgs-engine.git
cd av1-fgs-engine

# Compile the optimized binary
cargo build --release

# The compiled binary will be located at:
# target/release/av1-fgs-engine

```
---

## Command-Line Interface (CLI)

```

Usage: av1-fgs-engine [OPTIONS] --input <INPUT>

Options:
  -i, --input <INPUT>                Path to .vpy script or video container (.mkv, .mp4, .y4m)
  -o, --output <OUTPUT>              Output .tbl file path [default: 70mm_grain.tbl]
      --force-type <2d|3d|live_action> Force content classification
      --tune-grain                   Analyze and replicate existing real film grain (1:1 clone)
      --fg-search-full               Exhaustive scene-by-scene search matching original grain (Requires --tune-grain)
      --forced-fg-search-full        Dynamic scene-adaptive 70mm grain search for clean/grainless content
      --lookahead <FRAMES>           Temporal rolling lookahead window in frames
      --scenes <SCENES>              Optional path to Av1an scenes.csv file
      --intensity <FLOAT>            Global grain intensity multiplier [default: 1.0]
  -h, --help                         Print help
  -V, --version                      Print version

```

  ## Parameter Reference

  Input & Output
-i, --input <PATH>: Specifies the video file or VapourSynth script. If a .vpy file is passed, vspipe is spawned automatically; otherwise, ffmpeg pipes uncompressed Y4M frames into memory.
-o, --output <PATH>: Target destination for the generated filmgrn1 parameter table (defaults to 70mm_grain.tbl).

Content Classification
--force-type <2d | 3d | live_action>: Overrides the automatic spatial variance classifier.
2d: Designed for anime and traditional animation. Compresses noise in dark lines to protect line art and prevents buzzing in flat color fields.
3d: Calibrated for CGI and video game renders. Optimizes midtone dithering to break 8/10-bit banding gradients.
live_action: Full photochemical sensitometric curve matching Kodak Vision / Super Panavision 70 stocks.

Grain Profiling & Temporal Search
--tune-grain: Activates deep block-based Fourier variance analysis. Measures local standard deviation (
σ
σ
) across 8 luminance intervals (
16
,
48
,
80
,
112
,
144
,
176
,
208
,
240
16,48,80,112,144,176,208,240
). If existing grain is detected, it replicates the source's exact grain structure. If the source is clean (
σ
<
1.15
σ<1.15
), it automatically falls back to the imperceptible 70mm baseline.

--fg-search-full: Scans the entire video scene by scene to track variable grain across different shots (e.g., films intercutting between 35mm and 70mm IMAX). Requires --tune-grain.

--forced-fg-search-full: Designed specifically for clean, grainless sources (e.g., modern digital animation, pristine CGI). Instead of generating a single static block for the entire feature, it dynamically modulates the subtle 70mm grain curve according to the luminance and contrast of each scene while remaining nearly imperceptible.

--lookahead <FRAMES>: Enables a rolling temporal window across adjacent scenes. Computes a moving average of noise variance (
σ
σ
) and autocorrelation (
ρ
ρ
) to smooth out transitions and eliminate single-frame grain spikes. Only valid with --fg-search-full or --forced-fg-search-full.

--scenes <PATH>: Points to an Av1an scenes.csv file. Directly aligns grain parameters with the exact chunk boundaries used during parallel encoding.

--intensity <FLOAT>: Fine-tunes the amplitude of synthesized grain (default: 1.0). Use 0.5–0.8 for extra-subtle results or 1.2–1.5 for higher vintage prominence.

## Practical Examples

1. Clean Anime or CGI (Dynamic, Scene-Adaptive Subtle Grain)

```
./target/release/av1-fgs-engine \
    -i script.vpy \
    -o anime_clean.tbl \
    --force-type 2d \
    --forced-fg-search-full \
    --lookahead 8 \
    --scenes scenes.csv
```
2. Multi-Format Film (e.g., IMAX 70mm & 35mm like Interstellar)

```
 ./target/release/av1-fgs-engine \
    -i interstellar.mkv \
    -o interstellar_grain.tbl \
    --force-type live_action \
    --tune-grain \
    --fg-search-full \
    --lookahead 12 \
    --scenes scenes.csv
```

3. Fast Static Single-Pass (Instant Analysis)

```
 ./target/release/av1-fgs-engine \
    -i source.mkv \
    -o static_subtle.tbl \
    --force-type 2d

```

## Integration with Encoders 

SVT-AV1

```
SvtAv1EncApp -i input.y4m --fgs-table 70mm_grain.tbl -b output.ivf --preset 4 --crf 24

```

Av1an
 ```
av1an -i script.vpy -e svt-av1 -s scenes.csv -v " --crf 24 --fgs-table 70mm_grain.tbl " -o output.mkv

```

## License

This project is licensed under the GNU General Public License v3.0 (GPL-3.0). See the LICENSE file for details.




## Powered By Gemini 3.8 flash.
