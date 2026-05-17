//! Smoke test: system FFmpeg is visible to `ffmpeg-next`.

fn main() {
    ffmpeg_next::init().expect("ffmpeg init");
    println!("avutil version: {:#08x}", ffmpeg_next::util::version());
}
