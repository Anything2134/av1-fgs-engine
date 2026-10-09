
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
