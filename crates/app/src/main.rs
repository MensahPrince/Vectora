//! Cutlass app binary: timeline-driven preview smoke test.
//!
//! ```text
//! cargo run -p app --release
//! ```

slint::include_modules!();
use decoder::{
    CpuFrame, DecodeOutcome, DecodedVideoFrame, Decoder, FrameData, PixelFormat, Rational,
};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// Commands sent from the UI thread to the decoder worker.
enum DecodeCommand {
    Next,
    Scrub(f32),
}

#[inline]
fn yuv_to_rgba_bt601(y: u8, u: u8, v: u8) -> Rgba8Pixel {
    let c = i32::from(y) - 16;
    let d = i32::from(u) - 128;
    let e = i32::from(v) - 128;
    let r = (298 * c + 409 * e + 128) >> 8;
    let g = (298 * c - 100 * d - 208 * e + 128) >> 8;
    let b = (298 * c + 516 * d + 128) >> 8;
    Rgba8Pixel {
        r: r.clamp(0, 255) as u8,
        g: g.clamp(0, 255) as u8,
        b: b.clamp(0, 255) as u8,
        a: 255,
    }
}

fn rgba_buffer_from_cpu_frame(
    cpu: &CpuFrame,
    width: u32,
    height: u32,
) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
    let w = width as usize;
    let h = height as usize;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);

    match cpu.format {
        PixelFormat::Rgba8 => {
            let plane = cpu.planes.first()?;
            let row = w * 4;
            let dst = buf.make_mut_bytes();
            if plane.stride == row {
                dst.copy_from_slice(&plane.data[..row * h]);
            } else {
                for y in 0..h {
                    let s = y * plane.stride;
                    let d = y * row;
                    dst[d..d + row].copy_from_slice(&plane.data[s..s + row]);
                }
            }
        }
        PixelFormat::Yuv420p => {
            let pixels = buf.make_mut_slice();
            let yp = cpu.planes.get(0)?;
            let up = cpu.planes.get(1)?;
            let vp = cpu.planes.get(2)?;
            for y in 0..h {
                for x in 0..w {
                    let yy = yp.data[y * yp.stride + x];
                    let u = up.data[(y / 2) * up.stride + (x / 2)];
                    let v = vp.data[(y / 2) * vp.stride + (x / 2)];
                    pixels[y * w + x] = yuv_to_rgba_bt601(yy, u, v);
                }
            }
        }
        PixelFormat::Nv12 => {
            let pixels = buf.make_mut_slice();
            let yp = cpu.planes.get(0)?;
            let uv = cpu.planes.get(1)?;
            for y in 0..h {
                for x in 0..w {
                    let yy = yp.data[y * yp.stride + x];
                    let uv_base = (y / 2) * uv.stride + (x / 2) * 2;
                    let u = uv.data[uv_base];
                    let v = uv.data[uv_base + 1];
                    pixels[y * w + x] = yuv_to_rgba_bt601(yy, u, v);
                }
            }
        }
        _ => return None,
    }

    Some(buf)
}

trait DecodedVideoFrameSlint {
    fn rgba_shared_buffer(&self) -> Option<SharedPixelBuffer<Rgba8Pixel>>;
}

impl DecodedVideoFrameSlint for DecodedVideoFrame {
    fn rgba_shared_buffer(&self) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        let cpu = match &self.data {
            FrameData::Cpu(c) => c,
            _ => return None,
        };
        rgba_buffer_from_cpu_frame(cpu, self.width, self.height)
    }
}

/// Convert a scrubber float (seconds) to a `Rational` at millisecond resolution.
/// Float-to-int *cast* truncates; this rounds and clamps to non-negative.
fn scrub_target(secs: f32) -> Rational {
    Rational::new((secs.max(0.0) * 1000.0).round() as i64, 1000)
        .expect("ms denominator is non-zero")
}

/// Convert + ship one decoded frame back to the UI thread.
fn deliver_frame(frame: DecodedVideoFrame, app_weak: &slint::Weak<AppWindow>) {
    let pts = frame.pts;
    let Some(pixels) = frame.rgba_shared_buffer() else {
        warn!(
            width = frame.width,
            height = frame.height,
            "frame conversion failed"
        );
        return;
    };
    info!(?pts, "frame delivered");
    let app_weak = app_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(app) = app_weak.upgrade() {
            app.set_preview_image(Image::from_rgba8(pixels));
        }
    });
}

/// Spawn a worker thread that owns the `Decoder` and pushes converted frames back
/// to the UI thread via `slint::invoke_from_event_loop`. Returns the command sender;
/// dropping the sender causes the worker to exit on its next `recv`.
fn spawn_decoder_worker(
    mut decoder: Decoder,
    app_weak: slint::Weak<AppWindow>,
) -> mpsc::Sender<DecodeCommand> {
    let (tx, rx) = mpsc::channel::<DecodeCommand>();
    thread::spawn(move || {
        while let Ok(first) = rx.recv() {
            // Coalesce backlog: latest command wins. Keeps scrubs responsive when
            // the UI fires faster than we can decode. `try_recv` returns immediately.
            let mut cmd = first;
            while let Ok(next) = rx.try_recv() {
                cmd = next;
            }

            match cmd {
                DecodeCommand::Scrub(secs) => {
                    let target = scrub_target(secs);
                    decoder.set_fast_scrub(true);
                    match decoder.seek_exact(target) {
                        Ok(DecodeOutcome::Frame(frame)) => deliver_frame(frame, &app_weak),
                        Ok(DecodeOutcome::Eof) => warn!(?target, "scrub past EOF"),
                        Err(e) => error!(?e, "scrub error"),
                    }
                }
                DecodeCommand::Next => match decoder.next_frame() {
                    Ok(DecodeOutcome::Frame(frame)) => deliver_frame(frame, &app_weak),
                    Ok(DecodeOutcome::Eof) => info!("eof"),
                    Err(e) => error!(?e, "decode error"),
                },
            }
        }
    });
    tx
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let decoder = Decoder::open(Path::new(
        // "assets/15881269_3840_2160_60fps_allintra_proxy.mp4",
        "assets/15881269_3840_2160_60fps.mp4",
    ))
    .expect("decoder open failed");

    let app = AppWindow::new().expect("slint window creation failed");
    let cmd_tx = spawn_decoder_worker(decoder, app.as_weak());

    {
        let cmd_tx = cmd_tx.clone();
        app.on_play_requested(move || {
            let _ = cmd_tx.send(DecodeCommand::Next);
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        app.on_scrub_requested(move |seconds| {
            let _ = cmd_tx.send(DecodeCommand::Scrub(seconds));
        });
    }

    app.run().expect("slint application startup failed");
    drop(cmd_tx); // worker exits on next recv
}
