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

#[derive(Parser, Debug)]
#[command(name = "av1-fgs-engine")]
#[command(version = "0.2.0")]
#[command(about = "High-performance AV1 Film Grain Synthesis (filmgrn1) generator in Rust")]
struct Args {
    #[arg(short, long, help = "Path to .vpy script or video container (.mkv, .mp4, .y4m)")]
    input: PathBuf,

    #[arg(short, long, default_value = "70mm_grain.tbl", help = "Output .tbl file path")]
    output: PathBuf,

    #[arg(long, value_enum, help = "Force content type classification to eliminate false positives")]
    force_type: Option<ContentType>,

    #[arg(long, help = "Analyze and replicate existing real film grain from source (1:1 clone)")]
    tune_grain: bool,

    #[arg(long, help = "Exhaustive scene-by-scene search matching original source grain (Requires --tune-grain)")]
    fg_search_full: bool,

    #[arg(long, help = "Dynamic scene-adaptive 70mm grain search for clean/grainless content")]
    forced_fg_search_full: bool,

    #[arg(long, help = "Temporal rolling lookahead window in frames (Only valid with --fg-search-full or --forced-fg-search-full)")]
    lookahead: Option<usize>,

    #[arg(long, help = "Optional path to Av1an scenes.csv file for exact scene-boundary matching")]
    scenes: Option<PathBuf>,

    #[arg(long, default_value_t = 1.0, help = "Global grain intensity multiplier")]
    intensity: f32,
}

const BIN_CENTERS: [u8; 8] = [16, 48, 80, 112, 144, 176, 208, 240];

struct SceneSegment {
    start_frame: usize,
    end_frame: usize,
}

struct Y4mStream {
    reader: BufReader<Box<dyn Read + Send>>,
    width: usize,
    height: usize,
    current_frame: usize,
}

impl Y4mStream {
    fn from_input(path: &Path) -> Result<Self, String> {
        let is_vpy = path.extension().and_then(|s| s.to_str()) == Some("vpy");

        let child = if is_vpy {
            Command::new("vspipe")
                .arg(path)
                .arg("-")
                .arg("--y4m")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("Failed to spawn vspipe: {e}"))?
        } else {
            Command::new("ffmpeg")
                .arg("-v")
                .arg("quiet")
                .arg("-i")
                .arg(path)
                .arg("-f")
                .arg("yuv4mpegpipe")
                .arg("-pix_fmt")
                .arg("yuv420p")
                .arg("-")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?
        };

        let stdout: Box<dyn Read + Send> = Box::new(child.stdout.ok_or("Failed to capture stream stdout")?);
        let mut reader = BufReader::with_capacity(1024 * 1024, stdout);

        let mut header = String::new();
        reader.read_line(&mut header).map_err(|e| format!("Failed to read Y4M header: {e}"))?;

        if !header.starts_with("YUV4MPEG2") {
            return Err("Input stream is not valid Y4M format".to_string());
        }

        let mut width = 0;
        let mut height = 0;

        for token in header.split_whitespace() {
            if let Some(w) = token.strip_prefix('W') {
                width = w.parse().unwrap_or(0);
            } else if let Some(h) = token.strip_prefix('H') {
                height = h.parse().unwrap_or(0);
            }
        }

        if width == 0 || height == 0 {
            return Err("Video dimensions missing from Y4M stream".to_string());
        }

        Ok(Self {
            reader,
            width,
            height,
            current_frame: 0,
        })
    }

    fn read_next_luma(&mut self) -> Option<Vec<u8>> {
        let mut frame_tag = [0u8; 6];
        if self.reader.read_exact(&mut frame_tag).is_err() {
            return None;
        }

        let mut discard = Vec::new();
        let _ = self.reader.read_until(b'\n', &mut discard);

        let luma_size = self.width * self.height;
        let mut luma = vec![0u8; luma_size];
        if self.reader.read_exact(&mut luma).is_err() {
            return None;
        }

        let chroma_size = (self.width / 2) * (self.height / 2) * 2;
        let mut chroma_buf = vec![0u8; chroma_size];
        let _ = self.reader.read_exact(&mut chroma_buf);

        self.current_frame += 1;
        Some(luma)
    }

    fn skip_frames(&mut self, count: usize) -> bool {
        let frame_size = self.width * self.height + (self.width / 2) * (self.height / 2) * 2;
        let mut buf = vec![0u8; 6];

        for _ in 0..count {
            if self.reader.read_exact(&mut buf).is_err() {
                return false;
            }
            let mut discard = Vec::new();
            let _ = self.reader.read_until(b'\n', &mut discard);

            let mut skip_buf = vec![0u8; frame_size];
            if self.reader.read_exact(&mut skip_buf).is_err() {
                return false;
            }
            self.current_frame += 1;
        }
        true
    }
}

// -------------------------------------------------------------------------
// SPECTRAL NOISE ESTIMATION & AUTOCORRELATION
// -------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct GrainProfile {
    lag: u8,
    profile_sy: Vec<(u8, u8)>,
    avg_sigma: f32,
    mean_luma: f32,
}

fn analyze_frame_luma(luma: &[u8], width: usize, height: usize, ctype: ContentType) -> (GrainProfile, f32) {
    let block_size = 16;
    let max_edge = match ctype {
        ContentType::TwoD => 2.8f32,
        ContentType::ThreeD => 4.2f32,
        ContentType::LiveAction => 5.5f32,
    };

    let blocks_x = width / block_size;
    let blocks_y = height / block_size;

    let total_pixels = (width * height) as f32;
    let frame_mean_luma = luma.iter().map(|&p| p as f32).sum::<f32>() / total_pixels;

    let block_results: Vec<(u8, f32, f32)> = (0..blocks_y)
        .into_par_iter()
        .flat_map(|by| {
            let mut local = Vec::new();
            for bx in (0..blocks_x).step_by(2) {
                let y_start = by * block_size;
                let x_start = bx * block_size;

                let mut sum = 0.0f32;
                let mut edge_acc = 0.0f32;

                for y in 0..block_size {
                    for x in 0..block_size {
                        let p = luma[(y_start + y) * width + (x_start + x)] as f32;
                        sum += p;

                        if x + 1 < block_size {
                            let px = luma[(y_start + y) * width + (x_start + x + 1)] as f32;
                            edge_acc += (p - px).abs();
                        }
                        if y + 1 < block_size {
                            let py = luma[(y_start + y + 1) * width + (x_start + x)] as f32;
                            edge_acc += (p - py).abs();
                        }
                    }
                }

                let edge_score = edge_acc / (block_size * block_size) as f32;
                if edge_score > max_edge {
                    continue;
                }

                let mean = sum / (block_size * block_size) as f32;
                let mut sq_diff = 0.0f32;
                let mut autocorr_acc = 0.0f32;

                for y in 0..block_size {
                    for x in 0..block_size {
                        let r = luma[(y_start + y) * width + (x_start + x)] as f32 - mean;
                        sq_diff += r * r;

                        if x + 1 < block_size {
                            let rx = luma[(y_start + y) * width + (x_start + x + 1)] as f32 - mean;
                            autocorr_acc += r * rx;
                        }
                    }
                }

                let variance = sq_diff / (block_size * block_size) as f32;
                let sigma = variance.sqrt();
                let rho = if sq_diff > 1.0 { autocorr_acc / sq_diff } else { 0.0 };

                let nearest_bin = BIN_CENTERS
                    .iter()
                    .min_by_key(|&&c| ((c as f32) - mean).abs() as i32)
                    .copied()
                    .unwrap_or(128);

                local.push((nearest_bin, sigma, rho));
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

    let lag: u8 = if avg_rho > 0.38 {
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

        let val = if med_sigma > 0.5 {
            (med_sigma * 2.3).round().clamp(1.0, 255.0) as u8
        } else {
            1
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

fn classify_frame_auto(luma: &[u8], width: usize, height: usize) -> ContentType {
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
                        let p = luma[(y_start + y) * width + (x_start + x)] as f32;
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

// -------------------------------------------------------------------------
// SUBTLE & IMPERCEPTIBLE BASELINE PROFILES (70mm EMULATION)
// -------------------------------------------------------------------------

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

// -------------------------------------------------------------------------
// STRICT FILMGRN1 SYNTAX WRITER
// -------------------------------------------------------------------------

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
    let f = File::open(path).map_err(|e| format!("Failed to open scenes.csv: {e}"))?;
    let reader = BufReader::new(f);
    let mut segments = Vec::new();

    for line in reader.lines() {
        let l = line.map_err(|e| e.to_string())?;
        let parts: Vec<&str> = l.split(',').collect();
        if parts.len() >= 2 {
            if let (Ok(s), Ok(e)) = (parts[0].trim().parse::<usize>(), parts[1].trim().parse::<usize>()) {
                segments.push(SceneSegment {
                    start_frame: s,
                    end_frame: e,
                });
            }
        }
    }
    if segments.is_empty() {
        return Err("No valid scene segments found in scenes.csv".to_string());
    }
    Ok(segments)
}

// -------------------------------------------------------------------------
// ENTRYPOINT
// -------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.lookahead.is_some() && !args.fg_search_full && !args.forced_fg_search_full {
        eprintln!("[-] Error: --lookahead requires either --fg-search-full or --forced-fg-search-full.");
        std::process::exit(1);
    }

    if args.fg_search_full && !args.tune_grain {
        eprintln!("[-] Error: --fg-search-full requires --tune-grain to calibrate source grain.");
        std::process::exit(1);
    }

    println!("[*] Initializing AV1 Film Grain Synthesis Engine (Rust)...");
    let mut stream = Y4mStream::from_input(&args.input)?;
    println!("    - Stream format: {}x{}", stream.width, stream.height);

    let mut out_file = File::create(&args.output)?;
    writeln!(out_file, "filmgrn1")?;

    let first_frame = stream.read_next_luma().ok_or("Failed to read initial luma frame")?;
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
            println!("    [+] Loading exact scene cuts from: {}", sc_path.display());
            parse_av1an_scenes(sc_path)?
        } else {
            println!("    [!] No scenes.csv provided. Fallback to 120-frame chunk intervals...");
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

            if let Some(frame) = stream.read_next_luma() {
                let (current_prof, _) = analyze_frame_luma(&frame, stream.width, stream.height, content_type);

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
                    if smoothed_profile.avg_sigma >= 1.15 {
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
                        "    - Segment {} [Frames {}..{}]: Mean Luma = {:.1} | Noise Sigma = {:.2}",
                        idx + 1,
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
        println!("[*] Generating static global grain table (infinite range)...");
        let (prof, _) = analyze_frame_luma(&first_frame, stream.width, stream.height, content_type);

        if args.tune_grain && prof.avg_sigma >= 1.15 {
            println!("    [+] Source grain detected (Sigma: {:.2}). Cloning profile...", prof.avg_sigma);
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
