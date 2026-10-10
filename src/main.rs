use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq)]
enum ContentType {
    #[value(name = "2d")]
    TwoD,
    #[value(name = "3d")]
    ThreeD,
    #[value(name = "live_action", alias = "live-action")]
    LiveAction,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq)]
enum MutagenState {
    #[value(name = "on")]
    On,
    #[value(name = "off")]
    Off,
}

#[derive(Parser, Debug)]
#[command(name = "CT-AV1-FGS-ENGINE")]
#[command(version = "1.1.1")]
#[command(about = "High-performance AV1 Film Grain Synthesis Engine (Celluloid 70mm & Digital Hybrid) in Rust")]
struct Args {
    #[arg(short, long, help = "Path to .vpy script or video container (.mkv, .mp4, .y4m)")]
    input: PathBuf,

    #[arg(short, long, default_value = "70mm_grain.tbl", help = "Output .tbl file path")]
    output: PathBuf,

    #[arg(long, value_enum, help = "Force content type classification (2d, 3d, live_action)")]
    force_type: Option<ContentType>,

    #[arg(long, help = "Analyze and replicate existing real film grain from source (1:1 clone)")]
    tune_grain: bool,

    #[arg(long, help = "Exhaustive scene-by-scene search matching original source grain (Requires --tune-grain)")]
    fg_search_full: bool,

    #[arg(long, help = "Dynamic scene-adaptive 70mm grain search for clean/grainless content")]
    forced_fg_search_full: bool,

    #[arg(long, help = "Temporal rolling lookahead window in frames (Only valid with full search modes)")]
    lookahead: Option<usize>,

    #[arg(long, help = "Path to Av1an scenes file (supports JSON or CSV format)")]
    scenes: Option<PathBuf>,

    #[arg(long, default_value_t = 1.0, help = "Global grain intensity multiplier")]
    intensity: f32,

    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(0..=1), help = "Noise bias filter: 1 = isolate pure grain; 0 = allow digital/compression noise synthesis")]
    no_noise_bias: u8,

    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=2), help = "Digital noise tuning (1: Transparent digital, 2: Balanced 70mm hybrid). Only valid when --no-noise-bias is 0")]
    tune_grain_noise_digital: Option<u8>,

    #[arg(long, value_enum, default_value = "on", help = "Advanced Mutagen entropy/grain analysis module (on/off)")]
    mutagen: MutagenState,
}

const BIN_CENTERS: [u8; 8] = [16, 48, 80, 112, 144, 176, 208, 240];

#[derive(Clone, Debug)]
struct SceneSegment {
    start_frame: usize,
    end_frame: usize,
}

// -------------------------------------------------------------------------
// STREAM DE ENTRADA Y4M (SOPORTE 8, 10, 12 Y 16 BITS / HDR / BT.2020)
// -------------------------------------------------------------------------

struct Y4mStream {
    reader: BufReader<Box<dyn Read + Send>>,
    width: usize,
    height: usize,
    current_frame: usize,
    bit_depth: usize,
    bytes_per_sample: usize,
    luma_size: usize,
    chroma_size: usize,
}

impl Y4mStream {
    fn from_input(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Err(format!("File does not exist at path: '{}'.", path.display()));
        }

        let is_vpy = path.extension().and_then(|s| s.to_str()) == Some("vpy");

        let mut child = if is_vpy {
            Command::new("vspipe")
                .arg("-c")
                .arg("y4m")
                .arg(path)
                .arg("-")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to spawn vspipe: {e}"))?
        } else {
            Command::new("ffmpeg")
                .arg("-nostdin")
                .arg("-i")
                .arg(path)
                .arg("-map")
                .arg("0:v:0")
                .arg("-strict")
                .arg("-1") // Permite formatos Y4M de alta profundidad (10/12/16-bit)
                .arg("-f")
                .arg("yuv4mpegpipe")
                .arg("-")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?
        };

        let stdout: Box<dyn Read + Send> = Box::new(child.stdout.take().ok_or("Failed to capture stdout")?);
        let mut reader = BufReader::with_capacity(1024 * 1024, stdout);

        let mut header = String::new();
        if let Err(e) = reader.read_line(&mut header) {
            let mut err_msg = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_msg);
            }
            return Err(format!("Error reading Y4M stream: {e}\nDetails:\n{err_msg}"));
        }

        if !header.starts_with("YUV4MPEG2") {
            let mut err_msg = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut err_msg);
            }
            return Err(format!(
                "Invalid Y4M header.\nDetails:\n{}",
                if err_msg.trim().is_empty() { "Process terminated without output." } else { err_msg.trim() }
            ));
        }

        let mut width = 0;
        let mut height = 0;
        let mut bit_depth = 8;

        for token in header.split_whitespace() {
            if let Some(w) = token.strip_prefix('W') {
                width = w.parse().unwrap_or(0);
            } else if let Some(h) = token.strip_prefix('H') {
                height = h.parse().unwrap_or(0);
            } else if let Some(c) = token.strip_prefix('C') {
                if c.contains("p10") || c.contains("10") {
                    bit_depth = 10;
                } else if c.contains("p12") || c.contains("12") {
                    bit_depth = 12;
                } else if c.contains("p16") || c.contains("16") {
                    bit_depth = 16;
                }
            }
        }

        if width == 0 || height == 0 {
            return Err("Video dimensions not detected in Y4M header".to_string());
        }

        let bytes_per_sample = if bit_depth > 8 { 2 } else { 1 };
        let luma_size = width * height * bytes_per_sample;
        let chroma_size = (width / 2) * (height / 2) * 2 * bytes_per_sample;

        Ok(Self {
            reader,
            width,
            height,
            current_frame: 0,
            bit_depth,
            bytes_per_sample,
            luma_size,
            chroma_size,
        })
    }

    fn read_frame_header(&mut self) -> Result<(), ()> {
        let mut line = Vec::new();
        match self.reader.read_until(b'\n', &mut line) {
            Ok(n) if n > 0 && line.starts_with(b"FRAME") => Ok(()),
            _ => Err(()),
        }
    }

    fn read_next_luma_normalized(&mut self) -> Option<Vec<f32>> {
        if self.read_frame_header().is_err() {
            return None;
        }

        let mut raw_luma = vec![0u8; self.luma_size];
        if self.reader.read_exact(&mut raw_luma).is_err() {
            return None;
        }

        if skip_exact_bytes(&mut self.reader, self.chroma_size).is_err() {
            return None;
        }

        self.current_frame += 1;

        let total_pixels = self.width * self.height;
        let mut normalized = Vec::with_capacity(total_pixels);

        if self.bytes_per_sample == 1 {
            for &p in &raw_luma {
                normalized.push(p as f32);
            }
        } else {
            let max_val = ((1u32 << self.bit_depth) - 1) as f32;
            for chunk in raw_luma.chunks_exact(2) {
                let val = u16::from_le_bytes([chunk[0], chunk[1]]) as f32;
                normalized.push((val / max_val) * 255.0);
            }
        }

        Some(normalized)
    }

    fn skip_frames(&mut self, count: usize) -> bool {
        let frame_data_bytes = self.luma_size + self.chroma_size;
        for _ in 0..count {
            if self.read_frame_header().is_err() {
                return false;
            }
            if skip_exact_bytes(&mut self.reader, frame_data_bytes).is_err() {
                return false;
            }
            self.current_frame += 1;
        }
        true
    }
}

fn skip_exact_bytes<R: BufRead>(reader: &mut R, mut bytes_to_skip: usize) -> std::io::Result<()> {
    while bytes_to_skip > 0 {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "Unexpected EOF"));
        }
        let consume_len = buffer.len().min(bytes_to_skip);
        reader.consume(consume_len);
        bytes_to_skip -= consume_len;
    }
    Ok(())
}

// -------------------------------------------------------------------------
// MÓDULO MUTAGEN: ESTIMACIÓN AVANZADA DE RUIDO Y DESCOMPOSICIÓN MAD
// -------------------------------------------------------------------------

mod mutagen {
    use super::*;

    pub struct MutagenEngine {
        pub no_noise_bias: bool,
        pub digital_mode: Option<u8>,
    }

    impl MutagenEngine {
        pub fn new(no_noise_bias: bool, digital_mode: Option<u8>) -> Self {
            Self {
                no_noise_bias,
                digital_mode,
            }
        }

        pub fn analyze_frame(
            &self,
            luma: &[f32],
            width: usize,
            height: usize,
            ctype: ContentType,
        ) -> (GrainProfile, f32) {
            let block_size = 16;
            let blocks_x = width / block_size;
            let blocks_y = height / block_size;

            let max_edge = match ctype {
                ContentType::TwoD => 2.8f32,
                ContentType::ThreeD => 4.2f32,
                ContentType::LiveAction => 5.5f32,
            };

            let total_pixels = (width * height) as f32;
            let frame_mean_luma = luma.iter().sum::<f32>() / total_pixels;

            let block_results: Vec<(u8, f32, f32)> = (0..blocks_y)
                .into_par_iter()
                .flat_map(|by| {
                    let mut local = Vec::new();
                    for bx in (0..blocks_x).step_by(2) {
                        let y_start = by * block_size;
                        let x_start = bx * block_size;

                        let mut sum = 0.0f32;
                        let mut edge_acc = 0.0f32;
                        let mut block_boundary_diff = 0.0f32;

                        for y in 0..block_size {
                            for x in 0..block_size {
                                let p = luma[(y_start + y) * width + (x_start + x)];
                                sum += p;

                                if x + 1 < block_size {
                                    let px = luma[(y_start + y) * width + (x_start + x + 1)];
                                    edge_acc += (p - px).abs();
                                }
                                if y + 1 < block_size {
                                    let py = luma[(y_start + y + 1) * width + (x_start + x)];
                                    edge_acc += (p - py).abs();
                                }

                                if x == 7 || x == 8 || y == 7 || y == 8 {
                                    block_boundary_diff += (p - sum / (y * block_size + x + 1).max(1) as f32).abs();
                                }
                            }
                        }

                        let edge_score = edge_acc / (block_size * block_size) as f32;
                        if edge_score > max_edge {
                            continue;
                        }

                        if self.no_noise_bias && block_boundary_diff > 45.0 {
                            continue;
                        }

                        let mean = sum / (block_size * block_size) as f32;
                        let mut residuals = Vec::with_capacity(block_size * block_size);
                        let mut sq_diff = 0.0f32;
                        let mut autocorr_acc = 0.0f32;

                        for y in 0..block_size {
                            for x in 0..block_size {
                                let r = luma[(y_start + y) * width + (x_start + x)] - mean;
                                residuals.push(r.abs());
                                sq_diff += r * r;

                                if x + 1 < block_size {
                                    let rx = luma[(y_start + y) * width + (x_start + x + 1)] - mean;
                                    autocorr_acc += r * rx;
                                }
                            }
                        }

                        residuals.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        let mad = residuals[residuals.len() / 2];
                        let sigma_mad = mad * 1.4826;

                        let rho = if sq_diff > 1.0 { autocorr_acc / sq_diff } else { 0.0 };

                        let nearest_bin = BIN_CENTERS
                            .iter()
                            .min_by_key(|&&c| ((c as f32) - mean).abs() as i32)
                            .copied()
                            .unwrap_or(128);

                        local.push((nearest_bin, sigma_mad, rho));
                    }
                    local
                })
                .collect();

            let mut sigmas_per_bin: [Vec<f32>; 8] = Default::default();
            let mut rhos: Vec<f32> = Vec::new();

            for (bin_c, sigma, rho) in block_results {
                if let Some(pos) = BIN_CENTERS.iter().position(|&c| c == bin_c) {
                    sigmas_per_bin[pos].push(sigma);
                }
                if rho > 0.0 {
                    rhos.push(rho);
                }
            }

            let avg_rho = if !rhos.is_empty() {
                rhos.iter().sum::<f32>() / rhos.len() as f32
            } else {
                0.20
            };

            let lag: u8 = if let Some(1) = self.digital_mode {
                1
            } else if avg_rho > 0.38 {
                1
            } else if avg_rho > 0.22 {
                2
            } else {
                3
            };

            let mut profile_sy = Vec::new();
            let mut valid_sigmas = Vec::new();

            for (i, &center) in BIN_CENTERS.iter().enumerate() {
                let mut list = sigmas_per_bin[i].clone();
                list.sort_by(|a, b| a.partial_cmp(b).unwrap());

                let med_sigma = if !list.is_empty() {
                    list[list.len() / 2]
                } else {
                    0.0
                };

                if med_sigma > 0.3 {
                    valid_sigmas.push(med_sigma);
                }

                let val = match self.digital_mode {
                    Some(1) => (med_sigma * 2.8).round().clamp(1.0, 255.0) as u8,
                    Some(2) => {
                        let celluloid_base = match center {
                            16 | 240 => 1.0,
                            48 | 208 => 2.5,
                            80 | 176 => 4.0,
                            _ => 5.0,
                        };
                        let hybrid = (med_sigma * 1.4 + celluloid_base * 0.6).round();
                        hybrid.clamp(1.0, 255.0) as u8
                    }
                    _ => {
                        if med_sigma > 0.5 {
                            (med_sigma * 2.3).round().clamp(1.0, 255.0) as u8
                        } else {
                            1
                        }
                    }
                };

                profile_sy.push((center, val));
            }

            let avg_sigma = if !valid_sigmas.is_empty() {
                valid_sigmas.iter().sum::<f32>() / valid_sigmas.len() as f32
            } else {
                0.0
            };

            (
                GrainProfile {
                    lag,
                    profile_sy,
                    avg_sigma,
                    mean_luma: frame_mean_luma,
                },
                avg_rho,
            )
        }
    }
}

// -------------------------------------------------------------------------
// ANÁLISIS DE RESPALDO (MUTAGEN OFF)
// -------------------------------------------------------------------------

fn fallback_analyze_frame(luma: &[f32], width: usize, height: usize) -> (GrainProfile, f32) {
    let block_size = 16;
    let blocks_x = width / block_size;
    let blocks_y = height / block_size;
    let total_pixels = (width * height) as f32;
    let frame_mean_luma = luma.iter().sum::<f32>() / total_pixels;

    let mut sigmas = Vec::new();
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let y_start = by * block_size;
            let x_start = bx * block_size;
            let mut sum = 0.0f32;
            for y in 0..block_size {
                for x in 0..block_size {
                    sum += luma[(y_start + y) * width + (x_start + x)];
                }
            }
            let mean = sum / 256.0;
            let mut sq_diff = 0.0f32;
            for y in 0..block_size {
                for x in 0..block_size {
                    let r = luma[(y_start + y) * width + (x_start + x)] - mean;
                    sq_diff += r * r;
                }
            }
            sigmas.push((sq_diff / 256.0).sqrt());
        }
    }

    sigmas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let avg_sigma = sigmas[sigmas.len() / 2];

    let profile_sy = BIN_CENTERS
        .iter()
        .map(|&c| (c, (avg_sigma * 2.0).round().clamp(1.0, 255.0) as u8))
        .collect();

    (
        GrainProfile {
            lag: 2,
            profile_sy,
            avg_sigma,
            mean_luma: frame_mean_luma,
        },
        0.25,
    )
}

#[derive(Clone, Debug)]
struct GrainProfile {
    lag: u8,
    profile_sy: Vec<(u8, u8)>,
    avg_sigma: f32,
    mean_luma: f32,
}

fn classify_frame_auto(luma: &[f32], width: usize, height: usize) -> ContentType {
    let block_size = 16;
    let blocks_x = width / block_size;
    let blocks_y = height / block_size;

    let flat_blocks = (0..blocks_y)
        .into_par_iter()
        .map(|by| {
            let mut count = 0;
            for bx in 0..blocks_x {
                let y_start = by * block_size;
                let x_start = bx * block_size;
                let mut sum = 0.0f32;
                let mut sq_sum = 0.0f32;

                for y in 0..block_size {
                    for x in 0..block_size {
                        let p = luma[(y_start + y) * width + (x_start + x)];
                        sum += p;
                        sq_sum += p * p;
                    }
                }
                let mean = sum / 256.0;
                let var = (sq_sum / 256.0) - (mean * mean);
                if var < 7.0 {
                    count += 1;
                }
            }
            count
        })
        .sum::<usize>();

    let total = blocks_x * blocks_y;
    let flat_ratio = (flat_blocks as f32) / (total as f32);

    if flat_ratio > 0.38 {
        ContentType::TwoD
    } else if flat_ratio > 0.18 {
        ContentType::ThreeD
    } else {
        ContentType::LiveAction
    }
}

fn get_subtle_70mm_profile(ctype: ContentType, scene_mean_luma: f32) -> (u8, Vec<(u8, u8)>) {
    let lag = match ctype {
        ContentType::TwoD => 2,
        ContentType::ThreeD => 2,
        ContentType::LiveAction => 3,
    };

    let luma_weight = (scene_mean_luma / 128.0).clamp(0.6, 1.4);

    let raw_sy: Vec<(u8, u8)> = match ctype {
        ContentType::TwoD => {
            vec![
                (16, 1),
                (32, 1),
                (64, (2.0 * luma_weight).round() as u8),
                (96, (3.0 * luma_weight).round() as u8),
                (128, (4.0 * luma_weight).round() as u8),
                (160, (3.0 * luma_weight).round() as u8),
                (192, 2),
                (224, 1),
                (235, 1),
            ]
        }
        ContentType::ThreeD => {
            vec![
                (16, 1),
                (32, 2),
                (64, (3.0 * luma_weight).round() as u8),
                (96, (4.0 * luma_weight).round() as u8),
                (128, (5.0 * luma_weight).round() as u8),
                (160, (4.0 * luma_weight).round() as u8),
                (192, 3),
                (224, 1),
                (240, 1),
            ]
        }
        ContentType::LiveAction => {
            vec![
                (16, 1),
                (28, 2),
                (48, (3.0 * luma_weight).round() as u8),
                (72, (5.0 * luma_weight).round() as u8),
                (96, (6.0 * luma_weight).round() as u8),
                (120, (7.0 * luma_weight).round() as u8),
                (144, (6.0 * luma_weight).round() as u8),
                (168, (5.0 * luma_weight).round() as u8),
                (192, 4),
                (216, 2),
                (232, 1),
                (240, 1),
            ]
        }
    };

    let cleaned_sy = raw_sy.into_iter().map(|(l, v)| (l, v.clamp(1, 255))).collect();
    (lag, cleaned_sy)
}

fn write_fgs_entry(
    out: &mut File,
    start_f: usize,
    end_f: usize,
    lag: u8,
    points_sy: &[(u8, u8)],
    intensity: f32,
) -> std::io::Result<()> {
    let c_y: Vec<i32> = match lag {
        3 => vec![2, 1, -1, -3, 1, 4, 6, 4, 1, -2, 2, 8, 14, 8, 2, -2, 1, 4, 6, 4, 1, -3, -1, 1],
        2 => vec![3, 5, 3, -2, 6, 16, 6, -2, 3, 5, 3, -1],
        _ => vec![8, 20, 8, 4],
    };

    let num_pos_luma = 2 * (lag as usize) * ((lag as usize) + 1);
    let num_pos_chroma = num_pos_luma + 1;

    let c_cb_cr = vec!["0"; num_pos_chroma].join(" ");
    let c_y_str = c_y.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");

    let scaled_points: Vec<String> = points_sy
        .iter()
        .map(|&(l, v)| {
            let scaled_v = ((v as f32) * intensity).round().clamp(1.0, 255.0) as u8;
            format!("{l} {scaled_v}")
        })
        .collect();

    let sy_str = format!("{}  {}", scaled_points.len(), scaled_points.join(" "));
    let seed: u16 = (rand_seed(start_f) % 64000 + 1000) as u16;

    writeln!(out, "E {start_f} {end_f} 1 {seed} 1")?;
    writeln!(out, "\tp {lag} 6 0 8 1 1 0 0 0 0 0 0")?;
    writeln!(out, "\tsY {sy_str}")?;
    writeln!(out, "\tsCb 0")?;
    writeln!(out, "\tsCr 0")?;
    writeln!(out, "\tcY {c_y_str}")?;
    writeln!(out, "\tcCb {c_cb_cr}")?;
    writeln!(out, "\tcCr {c_cb_cr}")?;

    Ok(())
}

fn rand_seed(salt: usize) -> usize {
    let mut x = salt.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

fn parse_av1an_scenes(path: &Path) -> Result<Vec<SceneSegment>, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("Failed to open scenes file: {e}"))?;
    let mut raw_segments = Vec::new();

    if content.contains("\"scenes\"") || content.trim_start().starts_with('{') {
        let mut current_start: Option<usize> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.contains("\"start_frame\"") {
                if let Some(val_str) = trimmed.split(':').nth(1) {
                    let clean = val_str.trim().trim_end_matches(',').trim();
                    current_start = clean.parse::<usize>().ok();
                }
            } else if trimmed.contains("\"end_frame\"") {
                if let Some(val_str) = trimmed.split(':').nth(1) {
                    let clean = val_str.trim().trim_end_matches(',').trim();
                    if let (Some(s), Ok(e)) = (current_start, clean.parse::<usize>()) {
                        raw_segments.push(SceneSegment {
                            start_frame: s,
                            end_frame: e,
                        });
                        current_start = None;
                    }
                }
            }
        }
    } else {
        for line in content.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = if l.contains(',') {
                l.split(',').collect()
            } else {
                l.split_whitespace().collect()
            };

            if parts.len() >= 2 {
                if let (Ok(s), Ok(e)) = (parts[0].trim().parse::<usize>(), parts[1].trim().parse::<usize>()) {
                    raw_segments.push(SceneSegment {
                        start_frame: s,
                        end_frame: e,
                    });
                }
            }
        }
    }

    if raw_segments.is_empty() {
        return Err("No valid scene cuts found in scenes file".to_string());
    }

    let mut cleaned_segments: Vec<SceneSegment> = Vec::new();
    let mut highest_end = 0;

    for seg in raw_segments {
        if seg.start_frame >= highest_end && seg.end_frame > seg.start_frame {
            highest_end = seg.end_frame;
            cleaned_segments.push(seg);
        }
    }

    Ok(cleaned_segments)
}

// -------------------------------------------------------------------------
// ENTRYPOINT PRINCIPAL
// -------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.no_noise_bias == 1 && args.tune_grain_noise_digital.is_some() {
        eprintln!("[-] Error: --tune-grain-noise-digital can ONLY be used when --no-noise-bias is 0.");
        std::process::exit(1);
    }

    let digital_noise_mode = if args.no_noise_bias == 0 {
        Some(args.tune_grain_noise_digital.unwrap_or(2))
    } else {
        None
    };

    if args.lookahead.is_some() && !args.fg_search_full && !args.forced_fg_search_full {
        eprintln!("[-] Error: --lookahead requires either --fg-search-full or --forced-fg-search-full.");
        std::process::exit(1);
    }

    if args.fg_search_full && !args.tune_grain {
        eprintln!("[-] Error: --fg-search-full requires --tune-grain.");
        std::process::exit(1);
    }

    println!("[*] Initializing CT-AV1-FGS-ENGINE (Rust Engine v1.1.1)...");
    let mut stream = Y4mStream::from_input(&args.input)?;
    println!(
        "    - Stream: {}x{} | {}-bit depth ({} bytes/sample)",
        stream.width, stream.height, stream.bit_depth, stream.bytes_per_sample
    );

    let mutagen_engine = mutagen::MutagenEngine::new(args.no_noise_bias == 1, digital_noise_mode);

    if args.mutagen == MutagenState::Off {
        eprintln!("[!] WARNING: Mutagen module is OFF. Using fallback analysis (higher false-positive risk).");
    }

    let mut out_file = File::create(&args.output)?;
    writeln!(out_file, "filmgrn1")?;

    let first_frame = stream.read_next_luma_normalized().ok_or("Failed to read initial luma frame")?;
    let content_type = if let Some(forced) = args.force_type {
        println!("[*] Content type forced by user: {:?}", forced);
        forced
    } else {
        let auto = classify_frame_auto(&first_frame, stream.width, stream.height);
        println!("[*] Content type auto-detected: {:?}", auto);
        auto
    };

    let lookahead_window = args.lookahead.unwrap_or(0);

    if args.fg_search_full || args.forced_fg_search_full {
        let mode_desc = if args.fg_search_full {
            "Full source grain search & replication (--fg-search-full)"
        } else {
            "Scene-adaptive subtle 70mm grain search (--forced-fg-search-full)"
        };
        println!("[*] Active temporal mode: {}", mode_desc);

        let segments = if let Some(ref sc_path) = args.scenes {
            let segs = parse_av1an_scenes(sc_path)?;
            println!("    [+] Loading exact scene cuts from: {}", sc_path.display());
            println!("    [+] Clean chronological scene segments to process: {}", segs.len());
            segs
        } else {
            println!("    [!] No scenes file provided. Fallback to 120-frame intervals...");
            (0..50000)
                .step_by(120)
                .map(|start| SceneSegment {
                    start_frame: start,
                    end_frame: start + 120,
                })
                .collect()
        };

        let mut previous_profiles: Vec<GrainProfile> = Vec::new();

        for (idx, seg) in segments.iter().enumerate() {
            let mid_point = (seg.start_frame + seg.end_frame) / 2;

            if stream.current_frame < mid_point {
                let to_skip = mid_point - stream.current_frame;
                if !stream.skip_frames(to_skip) {
                    break;
                }
            }

            if let Some(frame) = stream.read_next_luma_normalized() {
                let (current_prof, _) = if args.mutagen == MutagenState::On {
                    mutagen_engine.analyze_frame(&frame, stream.width, stream.height, content_type)
                } else {
                    fallback_analyze_frame(&frame, stream.width, stream.height)
                };

                let smoothed_profile = if lookahead_window > 0 {
                    previous_profiles.push(current_prof.clone());
                    if previous_profiles.len() > lookahead_window {
                        previous_profiles.remove(0);
                    }
                    let avg_sigma = previous_profiles.iter().map(|p| p.avg_sigma).sum::<f32>() / (previous_profiles.len() as f32);
                    let avg_luma = previous_profiles.iter().map(|p| p.mean_luma).sum::<f32>() / (previous_profiles.len() as f32);
                    GrainProfile {
                        lag: current_prof.lag,
                        profile_sy: current_prof.profile_sy.clone(),
                        avg_sigma,
                        mean_luma: avg_luma,
                    }
                } else {
                    current_prof
                };

                if args.fg_search_full {
                    if smoothed_profile.avg_sigma >= 1.15 || digital_noise_mode.is_some() {
                        write_fgs_entry(&mut out_file, seg.start_frame, seg.end_frame, smoothed_profile.lag, &smoothed_profile.profile_sy, args.intensity)?;
                    } else {
                        let (lag, subtle_sy) = get_subtle_70mm_profile(content_type, smoothed_profile.mean_luma);
                        write_fgs_entry(&mut out_file, seg.start_frame, seg.end_frame, lag, &subtle_sy, args.intensity)?;
                    }
                } else {
                    let (lag, dynamic_sy) = get_subtle_70mm_profile(content_type, smoothed_profile.mean_luma);
                    write_fgs_entry(&mut out_file, seg.start_frame, seg.end_frame, lag, &dynamic_sy, args.intensity)?;
                }

                if idx % 10 == 0 || idx == segments.len() - 1 {
                    println!(
                        "    - Segment {}/{} [Frames {}..{}]: Mean Luma = {:.1} | Noise Sigma = {:.2}",
                        idx + 1,
                        segments.len(),
                        seg.start_frame,
                        seg.end_frame,
                        smoothed_profile.mean_luma,
                        smoothed_profile.avg_sigma
                    );
                }
            } else {
                break;
            }
        }
    } else {
        println!("[*] Generating static global grain table...");
        let (prof, _) = if args.mutagen == MutagenState::On {
            mutagen_engine.analyze_frame(&first_frame, stream.width, stream.height, content_type)
        } else {
            fallback_analyze_frame(&first_frame, stream.width, stream.height)
        };

        if (args.tune_grain && prof.avg_sigma >= 1.15) || digital_noise_mode.is_some() {
            println!("    [+] Noise/grain detected (Sigma: {:.2}). Synthesizing...", prof.avg_sigma);
            write_fgs_entry(&mut out_file, 0, 18446744073709551615, prof.lag, &prof.profile_sy, args.intensity)?;
        } else {
            println!("    [*] Applying subtle 70mm baseline default profile...");
            let (lag, subtle_sy) = get_subtle_70mm_profile(content_type, prof.mean_luma);
            write_fgs_entry(&mut out_file, 0, 18446744073709551615, lag, &subtle_sy, args.intensity)?;
        }
    }

    println!("[✓] FGS table successfully written to: {}", args.output.display());
    Ok(())
}
