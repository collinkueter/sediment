//! ADR-0017 M0 spike — on-device streaming ASR latency/RTF on real hardware.
//!
//! Two input modes:
//!   --wav <path>   deterministic, reproducible bench (runs anywhere with the
//!                  native ASR lib built); feeds the file as 16 kHz mono chunks.
//!   --mic          live microphone capture for the qualitative "feel".
//!
//! The decision-gate number is RTF (must be < 1.0, ideally << 1.0 to leave room
//! for diarization + the rest of the app) and time-to-first-partial.
//!
//! This is throwaway de-risking code (see docs/plans/voice-and-meeting-transcription.md
//! M0). It does NOT touch the Sediment app and is not a workspace member.

mod asr;
mod audio;
mod bench;

use anyhow::Result;
use asr::{Asr, ModelPaths};
use bench::Bench;
use clap::Parser;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(about = "M0 spike: measure on-device streaming ASR latency/RTF")]
struct Args {
    /// WAV file to transcribe (deterministic benchmark). Mutually exclusive with --mic.
    #[arg(long)]
    wav: Option<String>,

    /// Capture from the default microphone instead of a file.
    #[arg(long, default_value_t = false)]
    mic: bool,

    /// Seconds to capture in --mic mode.
    #[arg(long, default_value_t = 20)]
    seconds: u64,

    /// Pace the WAV at real time (sleep between chunks) instead of max speed.
    /// Max speed (default) measures pure compute RTF — the feasibility number.
    #[arg(long, default_value_t = false)]
    realtime: bool,

    /// Chunk size fed to the recognizer, in milliseconds.
    #[arg(long, default_value_t = 100)]
    chunk_ms: u64,

    // --- streaming zipformer transducer model files (see README for downloads) ---
    #[arg(long)]
    encoder: String,
    #[arg(long)]
    decoder: String,
    #[arg(long)]
    joiner: String,
    #[arg(long)]
    tokens: String,
    /// ONNX execution provider: "cpu", "coreml" (macOS), "cuda".
    #[arg(long, default_value = "cpu")]
    provider: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let model = ModelPaths {
        encoder: args.encoder.clone(),
        decoder: args.decoder.clone(),
        joiner: args.joiner.clone(),
        tokens: args.tokens.clone(),
        provider: args.provider.clone(),
    };

    println!("loading model (provider={}) …", args.provider);
    let load_start = Instant::now();
    let mut asr = Asr::new(&model)?;
    println!("model ready in {:.2} s", load_start.elapsed().as_secs_f64());

    match (args.wav.as_deref(), args.mic) {
        (Some(path), false) => run_wav(&mut asr, path, &args),
        (None, true) => run_mic(&mut asr, &args),
        _ => {
            anyhow::bail!("choose exactly one input: --wav <path> OR --mic");
        }
    }
}

fn run_wav(asr: &mut Asr, path: &str, args: &Args) -> Result<()> {
    println!("loading {path} → 16 kHz mono …");
    let samples = audio::load_wav_16k_mono(path)?;
    let chunk = (audio::TARGET_RATE as u64 * args.chunk_ms / 1000) as usize;
    let chunk_dur = Duration::from_millis(args.chunk_ms);

    let mut b = Bench::default();
    b.start();
    let mut last_text = String::new();

    for window in samples.chunks(chunk.max(1)) {
        let t = Instant::now();
        let text = asr.feed(window);
        b.record_feed(t.elapsed(), window.len(), &text);
        if text != last_text && !text.trim().is_empty() {
            print!("\r{text}");
            use std::io::Write;
            std::io::stdout().flush().ok();
            last_text = text;
        }
        if args.realtime {
            std::thread::sleep(chunk_dur);
        }
    }
    let tail = asr.finish();
    if !tail.trim().is_empty() {
        println!("\rfinal: {tail}");
    } else {
        println!();
    }
    b.report(&format!(
        "wav, {}, chunk={}ms, {}",
        args.provider,
        args.chunk_ms,
        if args.realtime { "realtime-paced" } else { "max-speed" }
    ));
    Ok(())
}

fn run_mic(asr: &mut Asr, args: &Args) -> Result<()> {
    println!("capturing mic for {} s … speak now.", args.seconds);
    let (_stream, rx, native_rate) = audio::start_mic_16k_mono()?;
    println!("(device native rate {native_rate} Hz → resampled to 16 kHz)");

    let mut b = Bench::default();
    b.start();
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut last_text = String::new();

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(window) => {
                let t = Instant::now();
                let text = asr.feed(&window);
                b.record_feed(t.elapsed(), window.len(), &text);
                if text != last_text && !text.trim().is_empty() {
                    print!("\r{text}");
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                    last_text = text;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    let tail = asr.finish();
    println!("\rfinal: {tail}");
    b.report(&format!("mic, {}, native {native_rate}Hz", args.provider));
    Ok(())
}
