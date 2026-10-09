use clap::Parser;
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Parser, Debug)]
#[command(name = "av1-fgs-engine")]
#[command(about = "Motor de síntesis de grano AV1 (filmgrn1) ultrarrápido en Rust", long_about = None)]
struct Args {
    #[arg(short, long, help = "Ruta al script .vpy o video (.mkv, .mp4, .y4m)")]
    input: PathBuf,

    #[arg(short, long, default_value = "70mm_grain.tbl", help = "Archivo .tbl de salida")]
    output: PathBuf,

    #[arg(long, help = "Mide y clona el perfil de grano real de la fuente")]
    tune_grain: bool,

    #[arg(long, help = "Búsqueda temporal completa a lo largo de todo el video")]
    fg_search_full: bool,

    #[arg(long, help = "Ruta opcional a escenas.csv (de av1an) para saltarse la detección interna")]
    scenes: Option<PathBuf>,

    #[arg(long, default_value_t = 1.0, help = "Multiplicador de intensidad de grano")]
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
                .map_err(|e| format!("Error ejecutando vspipe: {e}"))?
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
                .map_err(|e| format!("Error ejecutando ffmpeg: {e}"))?
        };

        let stdout: Box<dyn Read + Send> = Box::new(child.stdout.ok_or("No se pudo capturar stdout del stream")?);
        let mut reader = BufReader::with_capacity(512 * 1024, stdout);

        let mut header = String::new();
        reader.read_line(&mut header).map_err(|e| format!("Error leyendo cabecera Y4M: {e}"))?;

        if !header.starts_with("YUV4MPEG2") {
            return Err("Stream inválido; no es formato Y4M".to_string());
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
            return Err("Dimensiones no encontradas en el stream Y4M".to_string());
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
// ANÁLISIS ESPECTRAL DE RUIDO Y AUTOCORRELACIÓN
// -------------------------------------------------------------------------

struct GrainProfile {
    lag: u8,
    profile_sy: Vec<(u8, u8)>,
    avg_sigma: f32,
}

fn analyze_grain_continuous(luma: &[u8], width: usize, height: usize, intensity: f32) -> GrainProfile {
    let block_size = 16;
    let max_edge = 3.8f32;

    let blocks_x = width / block_size;
    let blocks_y = height / block_size;

    // Procesamiento paralelo de bloques en Rayon
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
        0.2
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
            (med_sigma * 2.3 * intensity).round().clamp(1.0, 255.0) as u8
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

    GrainProfile {
        lag,
        profile_sy,
        avg_sigma,
    }
}

// -------------------------------------------------------------------------
// GENERADOR DE BLOQUES FILMGRN1
// -------------------------------------------------------------------------

fn write_grain_block(
    out: &mut File,
    start_f: usize,
    end_f: usize,
    profile: &GrainProfile,
    is_clean_fallback: bool,
) -> std::io::Result<()> {
    let lag = profile.lag;

    let c_y: Vec<i32> = match lag {
        3 => vec![2, 1, -1, -3, 1, 4, 6, 4, 1, -2, 2, 8, 14, 8, 2, -2, 1, 4, 6, 4, 1, -3, -1, 1],
        2 => vec![3, 5, 3, -2, 6, 16, 6, -2, 3, 5, 3, -1],
        _ => vec![8, 20, 8, 4],
    };

    let num_pos_luma = 2 * (lag as usize) * ((lag as usize) + 1);
    let num_pos_chroma = num_pos_luma + 1;

    let c_cb_cr = vec!["0"; num_pos_chroma].join(" ");
    let c_y_str = c_y.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");

    let sy_str = if is_clean_fallback {
        "9  16 1 32 1 64 2 96 3 128 4 160 3 192 2 224 1 235 1".to_string()
    } else {
        let count = profile.profile_sy.len();
        let pts = profile
            .profile_sy
            .iter()
            .map(|(l, v)| format!("{l} {v}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{count}  {pts}")
    };

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

// -------------------------------------------------------------------------
// DETECCIÓN DE ESCENAS
// -------------------------------------------------------------------------

fn parse_av1an_scenes(path: &Path) -> Result<Vec<SceneSegment>, String> {
    let f = File::open(path).map_err(|e| format!("Error abriendo escenas.csv: {e}"))?;
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
        return Err("El archivo escenas.csv no contiene cortes válidos".to_string());
    }

    Ok(segments)
}

// -------------------------------------------------------------------------
// ENTRYPOINT PRINCIPAL
// -------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.fg_search_full && !args.tune_grain {
        eprintln!("[-] Error: El parámetro --fg-search-full exige obligatoriamente --tune-grain.");
        std::process::exit(1);
    }

    println!("[*] Inicializando motor de síntesis de grano en Rust...");
    let mut stream = Y4mStream::from_input(&args.input)?;
    println!("    - Dimensiones del stream: {}x{}", stream.width, stream.height);

    let mut out_file = File::create(&args.output)?;
    writeln!(out_file, "filmgrn1")?;

    if args.fg_search_full {
        println!("[*] Modo --fg-search-full activo: Búsqueda continua multiceluloide...");

        let segments = if let Some(ref sc_path) = args.scenes {
            println!("    [+] Cargando cortes desde escenas.csv (Av1an): {}", sc_path.display());
            parse_av1an_scenes(sc_path)?
        } else {
            println!("    [!] No se pasó escenas.csv. Segmentando dinámicamente cada 120 cuadros...");
            (0..50000)
                .step_by(120)
                .map(|start| SceneSegment {
                    start_frame: start,
                    end_frame: start + 120,
                })
                .collect()
        };

        for (idx, seg) in segments.iter().enumerate() {
            let mid_point = (seg.start_frame + seg.end_frame) / 2;

            if stream.current_frame < mid_point {
                let to_skip = mid_point - stream.current_frame;
                if !stream.skip_frames(to_skip) {
                    break;
                }
            }

            if let Some(frame) = stream.read_next_luma() {
                let profile = analyze_grain_continuous(&frame, stream.width, stream.height, args.intensity);
                let is_clean = profile.avg_sigma < 1.15;

                if idx % 10 == 0 || is_clean {
                    println!(
                        "    - Segmento {} [Frames {}..{}]: Sigma = {:.2} | Lag = {} | {}",
                        idx + 1,
                        seg.start_frame,
                        seg.end_frame,
                        profile.avg_sigma,
                        profile.lag,
                        if is_clean { "Limpio (Línea base)" } else { "Grano continuo" }
                    );
                }

                write_grain_block(&mut out_file, seg.start_frame, seg.end_frame, &profile, is_clean)?;
            } else {
                break;
            }
        }
    } else {
        println!("[*] Modo estático global activo...");
        if let Some(frame) = stream.read_next_luma() {
            let profile = if args.tune_grain {
                analyze_grain_continuous(&frame, stream.width, stream.height, args.intensity)
            } else {
                GrainProfile {
                    lag: 2,
                    profile_sy: vec![],
                    avg_sigma: 0.0,
                }
            };

            let is_clean = !args.tune_grain || profile.avg_sigma < 1.15;
            write_grain_block(&mut out_file, 0, 18446744073709551615, &profile, is_clean)?;
        }
    }

    println!("[✓] Tabla generada exitosamente en: {}", args.output.display());
    Ok(())
}
