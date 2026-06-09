//! Parallel proxy build smoke test: transcode every video in `assets/` into
//! `proxy/<name>.mp4`.

use cutlass_decoder::{
    DecodeOptions, DecodedFrame, Decoder, HwAccel, PixelFormat, hw_accel_from_env,
};
use cutlass_encoder::{ProxyBuildOptions, ProxyConfig, build_proxy_with};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Instant;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

type AnyError = Box<dyn Error + Send + Sync>;

const HW_ACCEL: &str = "NONE";

fn setup_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
}

fn _decode_options() -> DecodeOptions {
    let mut options = DecodeOptions::default();
    options = options.hw_accel(hw_accel_from_env(&HW_ACCEL));
    options
}

const ASSETS_DIR: &str = "assets";
const PROXY_DIR: &str = "proxy";

/// VideoToolbox caps concurrent compression sessions; 42 at once hits err -12903.
///
///
const DEFAULT_MAX_LANES: usize = 6;

fn is_video_asset(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| matches!(ext.as_str(), "mp4" | "webm" | "mov" | "mkv"))
}

fn collect_video_assets(dir: &Path) -> Result<Vec<PathBuf>, AnyError> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && is_video_asset(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn proxy_output_path(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .expect("asset path has a filename")
        .to_string_lossy();
    Path::new(PROXY_DIR).join(format!("{stem}.mp4"))
}

fn max_lanes() -> usize {
    std::env::var("CUTLASS_PROXY_LANES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_LANES)
        .max(1)
}

fn run_proxy_builds(
    jobs: Vec<(PathBuf, PathBuf)>,
    proxy_config: ProxyConfig,
    opts: ProxyBuildOptions,
) -> Result<(), AnyError> {
    let lane_count = max_lanes().min(jobs.len());
    let (job_tx, job_rx) = mpsc::channel();
    for job in jobs {
        job_tx.send(job)?;
    }
    drop(job_tx);
    let job_rx = Arc::new(Mutex::new(job_rx));

    thread::scope(|s| {
        let handles: Vec<_> = (0..lane_count)
            .map(|lane| {
                let job_rx = Arc::clone(&job_rx);
                s.spawn(move || -> Result<(), AnyError> {
                    loop {
                        // Lock only around recv — a guard in `while let` lives until
                        // the end of the loop body and would serialize every lane.
                        let job = job_rx.lock().unwrap().recv();
                        let Ok((src, out)) = job else { break };
                        info!(lane, ?src, "building proxy");
                        build_proxy_with(&src, &out, proxy_config, opts, None)?;
                    }
                    Ok(())
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap()?;
        }
        Ok(())
    })
}

fn run() -> Result<(), AnyError> {
    let assets_dir = Path::new(ASSETS_DIR);
    std::fs::create_dir_all(PROXY_DIR)?;

    let jobs: Vec<(PathBuf, PathBuf)> = collect_video_assets(assets_dir)?
        .into_iter()
        .map(|src| {
            let out = proxy_output_path(&src);
            (src, out)
        })
        .collect();
    if jobs.is_empty() {
        return Err(format!("no video assets found in {ASSETS_DIR}/").into());
    }

    let proxy_config = ProxyConfig {
        target_height: 540,
        quality: 23,
        bitrate: 2_000_000,
        hardware: true,
    };

    let opts = ProxyBuildOptions {
        decode: HwAccel::Auto,
        decode_threads: 0,
        encode_threads: 0,
    };

    let job_count = jobs.len();
    let lane_count = max_lanes().min(job_count);
    info!(
        jobs = job_count,
        lanes = lane_count,
        "building proxies into {PROXY_DIR}/"
    );
    let now = Instant::now();
    run_proxy_builds(jobs, proxy_config, opts)?;
    info!(
        proxies = job_count,
        elapsed = ?now.elapsed(),
        "proxy build complete"
    );

    // let mut reader = Decoder::open_with(source, decode_options())?;

    // let n = 30;
    // let t = Duration::from_secs(n);

    // let now = Instant::now();
    // // let frame = reader.seek_dirty_to_frame(t)?.unwrap();
    // let frame = reader.seek_to_frame(t)?.expect("frame after seek");

    // info!("total time: {:?}", now.elapsed());

    // _write_decoded_frame_to_png(&frame, &format!("frame{}", n))?;

    Ok(())
}

fn main() {
    setup_tracing();
    if let Err(e) = run() {
        warn!(error = %e, "decode failed");
        std::process::exit(1);
    }
}

fn _write_decoded_frame_to_png(f: &DecodedFrame, name: &str) -> Result<(), Box<dyn Error>> {
    let w = f.width as usize;
    let h = f.height as usize;
    let mut rgba = vec![0u8; w * h * 4];

    match f.format {
        PixelFormat::Rgba8 => {
            let plane = &f.planes[0];
            for y in 0..h {
                let row = y * plane.stride;
                for x in 0..w {
                    let src = row + x * 4;
                    let dst = (y * w + x) * 4;
                    rgba[dst..dst + 4].copy_from_slice(&plane.data[src..src + 4]);
                }
            }
        }
        PixelFormat::Yuv420p => {
            let y_plane = &f.planes[0];
            let u_plane = &f.planes[1];
            let v_plane = &f.planes[2];
            for y in 0..h {
                for x in 0..w {
                    let yv = y_plane.data[y * y_plane.stride + x];
                    let uv_row = (y / 2) * u_plane.stride;
                    let uv_col = x / 2;
                    let u = u_plane.data[uv_row + uv_col];
                    let v = v_plane.data[(y / 2) * v_plane.stride + uv_col];
                    let (r, g, b) = yuv_to_rgb(yv, u, v);
                    let dst = (y * w + x) * 4;
                    rgba[dst] = r;
                    rgba[dst + 1] = g;
                    rgba[dst + 2] = b;
                    rgba[dst + 3] = 255;
                }
            }
        }
        PixelFormat::Nv12 => {
            let y_plane = &f.planes[0];
            let uv_plane = &f.planes[1];
            for y in 0..h {
                for x in 0..w {
                    let yv = y_plane.data[y * y_plane.stride + x];
                    let uv = (y / 2) * uv_plane.stride + (x / 2) * 2;
                    let u = uv_plane.data[uv];
                    let v = uv_plane.data[uv + 1];
                    let (r, g, b) = yuv_to_rgb(yv, u, v);
                    let dst = (y * w + x) * 4;
                    rgba[dst] = r;
                    rgba[dst + 1] = g;
                    rgba[dst + 2] = b;
                    rgba[dst + 3] = 255;
                }
            }
        }
    }

    let img = image::RgbaImage::from_raw(f.width, f.height, rgba)
        .ok_or("invalid frame dimensions for PNG")?;
    img.save(format!("{}.png", name))?;

    fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
        let y = (i32::from(y) - 16) * 298; // 1.164 << 8
        let u = i32::from(u) - 128;
        let v = i32::from(v) - 128;
        let r = ((y + 459 * v) >> 8).clamp(0, 255) as u8; // 1.793
        let g = ((y - 55 * u - 136 * v) >> 8).clamp(0, 255) as u8; // 0.213 / 0.533
        let b = ((y + 541 * u) >> 8).clamp(0, 255) as u8; // 2.112
        (r, g, b)
    }

    Ok(())
}
