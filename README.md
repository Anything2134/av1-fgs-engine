
<<<<<<< HEAD
# CT-AV1-FGS-ENGINE 
=======
# CT-AV1-FGS-ENGINE
>>>>>>> 0286232 (docs: update README with CT-AV1-FGS-ENGINE specifications, Mutagen module, and compilation guide)

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Format](https://img.shields.io/badge/AV1-filmgrn1-green.svg)](https://aomedia.org/)

**CT-AV1-FGS-ENGINE** is a modest, high-performance command-line utility written in Rust. It generates AOMedia/SVT-AV1 compliant Film Grain Synthesis (`filmgrn1`) metadata tables, bringing the physical micro-structure of **large-format 70mm celluloid film** and **hybrid digital grain** to modern AV1 encoding workflows.

---

## Purpose & Philosophy

AV1's **Film Grain Synthesis (FGS)** is a pivotal feature in video compression. Instead of wasting 40%–70% of the available bitrate attempting to encode high-frequency noise that traditional transform blocks often blur into macroblocking artifacts, FGS strips the noise, compresses the base image cleanly, and re-synthesizes organic grain deterministically inside the decoder on the GPU.

However, existing approaches often encounter practical challenges:
1. **Digital Photon Noise Simulators** (`--photon-noise`) generate uncorrelated white noise ($Lag = 0$), often producing sharp "salt-and-pepper" noise and chromatic specks that cause visible edge jitter (*line boiling*) in 2D animation.
2. **Encoder-side estimators** (`--film-grain` / `--denoise-noise-level`) introduce significant CPU overhead during pre-analysis and frequently lift deep shadow blacks.
3. **Static generic grain tables** remain invariant throughout an entire feature, failing to adapt when film stocks change (e.g., transitions between 35mm and IMAX 70mm) or when lighting shifts between high-key daylight and low-key night scenes.


 ### `CT-AV1-FGS-ENGINE` was created to solve these limitations. It models the microscopic, high-density physical structure of **large-format 70mm celluloid film**, delivering clean, organic grain that is virtually imperceptible to the human eye while providing natural dithering against 8/10-bit color banding.
=======
**CT-AV1-FGS-ENGINE** seeks to bridge this gap. Operating as a fast, external pre-pass in Rust, it analyzes video streams, separates real grain from digital compression artifacts, and writes temporally accurate `filmgrn1` parameter tables without penalizing encoder throughput.
>>>>>>> 0286232 (docs: update README with CT-AV1-FGS-ENGINE specifications, Mutagen module, and compilation guide)

---

## What's New in v1.1.0

### 1. The Mutagen Analysis Module
`mutagen` is an integrated analytical engine designed to isolate true entropy from noise artifacts:
- **Patch-Based Residual Decomposition**: Employs an optimized spatial approach to distinguish organic film grain from high-frequency compression edges and macroblock boundary discontinuities.
- **Median Absolute Deviation (MAD)**: Calculates residual dispersion using $\sigma_{\text{MAD}} = 1.4826 \times \text{median}(|R - \text{median}(R)|)$, maintaining resilience against outliers, line art, and high-contrast edges.
- **De-biasing Filter (`--no-noise-bias`)**: Actively rejects compression ringing and sensor thermal noise to avoid false-positive grain modeling.

### 2. High Bit-Depth & HDR Ingestion (10-bit up to 16-bit)
- Fully parses high bit-depth Y4M streams (`yuv420p10le`, `yuv420p12le`, `yuv420p16le`) with native little-endian two-byte sample unpacking.
- Compatible with **BT.2020**, **HDR10 (SMPTE ST 2084 / PQ)**, **HLG (BT.2100)**, and **Dolby Vision** sources.
- Normalizes sample distributions while preserving floating-point precision, ensuring HDR specular highlights (>100–203 nits) do not distort the piecewise linear $sY$ scaling curve.

### 3. Digital Noise & Hybrid Modeling (`--tune-grain-noise-digital`)
- **Mode 1 (Transparent Digital)**: Synthesizes high-frequency noise matching the measured camera sensor profile.
- **Mode 2 (Balanced 70mm Hybrid)**: Blends digital sensor noise with organic 70mm celluloid characteristics. Ideal when post-filtering with VapourSynth (`Degrain`) to restore natural texture without leaving flat or waxy surfaces.

---

## Key Features

- **70mm Celluloid Modeling**: Emulates fine silver halide grain via autoregressive spatial correlation ($Lag = 2$ or $3$), maintaining zero chroma noise pollution (`sCb 0`, `sCr 0`).
- **2D Animation & Line Art Protection**: Tapers synthesis in near-black luminance intervals ($<32$) to safeguard dark outlines against *line boiling*.
- **Temporal Search & Scene Adaptation**:
  - `--fg-search-full`: Tracks variable grain across multiple film stocks.
  - `--forced-fg-search-full`: Modulates subtle 70mm baseline grain across scene cuts for clean/grainless content.
- **Temporal Lookahead Window (`--lookahead`)**: Smooths variance transitions between consecutive cuts to prevent grain popping.
- **Universal Scene Ingestion (`--scenes`)**: Parses both Av1an JSON and traditional CSV scene files with monotonic chronological deduplication.
- **Zero-Copy Streaming**: Ingests directly from VapourSynth (`.vpy` via `vspipe`) and multimedia containers (`ffmpeg`) via Y4M pipes.

---

## Compilation & Installation

### Prerequisites
- [Rust & Cargo](https://www.rust-lang.org/) (version 1.70 or newer).
- `ffmpeg` installed and available in your `$PATH`.
- (Optional) `vapoursynth` and `vspipe` for `.vpy` script processing.

### Building from Source

```bash
# Clone the repository
git clone https://github.com/Anything2134/CT-AV1-FGS-ENGINE.git
<<<<<<< HEAD
cd CT-AV1-FGS-ENGINE 
=======
cd CT-AV1-FGS-ENGINE
>>>>>>> 0286232 (docs: update README with CT-AV1-FGS-ENGINE specifications, Mutagen module, and compilation guide)

# Build the release binary with optimizations
cargo build --release

<<<<<<< HEAD
# The compiled binary will be located at:
# target/release/CT-AV1-FGS-ENGINE 

```
---

=======
# Install globally in ~/.cargo/bin
cargo install --path .

```
## To create a system-wide symlink in /usr/local/bin

```
ln -sf "$(pwd)/target/release/ct-av1-fgs-engine" /usr/local/bin/ct-av1-fgs-engine
```
Verify your installation:
```
ct-av1-fgs-engine --help
```

### CLI Parameter Reference


| Parameter                        | Type / Values             | Default           | Description                                                                                                      |
| :------------------------------- | :------------------------ | :---------------: | :--------------------------------------------------------------------------------------------------------------- |
| **`-i, --input`**                | Path (`PathBuf`)          | *Required*        | Path to input file: video container (`.mkv`, `.mp4`) or VapourSynth script (`.vpy`).                             |
| **`-o, --output`**               | Path (`PathBuf`)          | `70mm_grain.tbl`  | Destination path for the generated `filmgrn1` parameter table.                                                   |
| **`--force-type`**               | `2d`, `3d`, `live_action` | Auto              | Overrides the spatial variance classifier to apply profile-specific edge thresholds and curves.                  |
| **`--intensity`**                | Float                     | `1.0`             | Global linear scaling multiplier for synthesized grain amplitude.                                                |
| **`--tune-grain`**               | Flag                      | Disabled          | Activates deep noise profiling to clone real source grain ($1:1$).                                               |
| **`--fg-search-full`**           | Flag                      | Disabled          | Scene-by-scene temporal grain search. *Requires `--tune-grain`*.                                                 |
| **`--forced-fg-search-full`**    | Flag                      | Disabled          | Dynamic scene-adaptive subtle 70mm grain search for clean or grainless content.                                  |
| **`--lookahead`**                | Integer (`usize`)         | Disabled          | Rolling temporal window in frames for inter-scene variance smoothing.                                            |
| **`--scenes`**                   | Path (`PathBuf`)          | Optional          | Path to Av1an scene cut file (supports both JSON and CSV formats).                                               |
| **`--no-noise-bias`**            | `0`, `1`                  | `1`               | Noise bias filter: `1` = isolates pure grain; `0` = permits digital/compression noise synthesis.                 |
| **`--tune-grain-noise-digital`** | `1`, `2`                  | `2` (when bias=0) | Digital noise tuning mode (`1`: Transparent digital, `2`: Balanced 70mm hybrid). *Requires `--no-noise-bias 0`*. |
| **`--mutagen`**                  | `on`, `off`               | `on`              | Toggles the Mutagen analysis module (`off` reverts to basic spatial variance analysis).                          |

Practical Examples

1. Clean Animation / CGI (Dynamic Scene-Adaptive 70mm Grain)

Applies a subtle, almost imperceptible baseline that dynamically responds to the
lighting of each scene without causing line boiling:

```
ct-av1-fgs-engine \
    -i anime.vpy \
    -o subtle_70mm.tbl \
    --force-type 2d \
    --forced-fg-search-full \
    --lookahead 8 \
    --scenes scenes.json
```

2. Multi-Format Film with Real Grain (e.g., 35mm & IMAX 70mm)

Clones original grain variations across cuts, switching autoregressive lag and
scaling points accurately:
```
ct-av1-fgs-engine \
    -i feature_film.mkv \
    -o film_clone.tbl \
    --force-type live_action \
    --tune-grain \
    --fg-search-full \
    --lookahead 12 \
    --scenes scenes.csv
```
3. Compensating for Heavy Denoising (Balanced Hybrid Synthesis)

When aggressive filtering in VapourSynth removes noise but leaves flat textures,
Mode 2 re-injects a balanced 70mm hybrid texture:
```
ct-av1-fgs-engine \
    -i filtered.vpy \
    -o hybrid_restored.tbl \
    --force-type 2d \
    --no-noise-bias 0 \
    --tune-grain-noise-digital 2 \
    --intensity 1.4 \
    --forced-fg-search-full \
    --scenes scenes.json
```
Encoder Integration

SVT-AV1
```
SvtAv1EncApp -i input.y4m --fgs-table 70mm_grain.tbl -b output.ivf --preset 4 --crf 24
```
Av1an
```
av1an -i input.vpy -e svt-av1 -s scenes.json -v " --crf 24 --fgs-table 70mm_grain.tbl " -o output.mkv

```
>>>>>>> 0286232 (docs: update README with CT-AV1-FGS-ENGINE specifications, Mutagen module, and compilation guide)

```
## License

This project is licensed under the GNU General Public License v3.0 (GPL-3.0). See the LICENSE file for details.




## Powered By Gemini 3.8 flash.
