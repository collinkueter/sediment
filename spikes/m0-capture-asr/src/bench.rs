//! The measurement the M0 decision gate needs: real-time factor (is on-device ASR
//! faster than real time on this hardware?), time-to-first-partial, and per-chunk
//! decode latency percentiles.

use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Bench {
    start: Option<Instant>,
    first_partial: Option<Duration>,
    /// Per-feed compute durations (accept_waveform + decode drain).
    feed_latencies: Vec<Duration>,
    /// Total compute time across all feeds (excludes any real-time pacing sleeps).
    compute: Duration,
    /// 16 kHz samples fed (defines audio duration).
    samples_16k: usize,
}

impl Bench {
    pub fn start(&mut self) {
        self.start = Some(Instant::now());
    }

    /// Record one feed step. `dur` is the compute time for that step; `text` is the
    /// hypothesis after it (used to detect the first non-empty partial).
    pub fn record_feed(&mut self, dur: Duration, n_samples: usize, text: &str) {
        self.compute += dur;
        self.feed_latencies.push(dur);
        self.samples_16k += n_samples;
        if self.first_partial.is_none() && !text.trim().is_empty() {
            if let Some(s) = self.start {
                self.first_partial = Some(s.elapsed());
            }
        }
    }

    pub fn audio_seconds(&self) -> f64 {
        self.samples_16k as f64 / super::audio::TARGET_RATE as f64
    }

    /// RTF = compute time / audio duration. < 1.0 means faster than real time.
    pub fn rtf(&self) -> f64 {
        let a = self.audio_seconds();
        if a > 0.0 {
            self.compute.as_secs_f64() / a
        } else {
            f64::NAN
        }
    }

    fn percentile(&self, p: f64) -> Duration {
        if self.feed_latencies.is_empty() {
            return Duration::ZERO;
        }
        let mut v = self.feed_latencies.clone();
        v.sort();
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[idx]
    }

    pub fn report(&self, label: &str) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!("\n──────── M0 ASR bench: {label} ────────");
        println!("audio duration       : {:.2} s", self.audio_seconds());
        println!("compute time         : {:.2} s", self.compute.as_secs_f64());
        println!(
            "real-time factor RTF : {:.3}  ({})",
            self.rtf(),
            if self.rtf() < 1.0 {
                "faster than real time ✓"
            } else {
                "SLOWER than real time ✗"
            }
        );
        match self.first_partial {
            Some(d) => println!("time-to-first-partial: {:.0} ms", ms(d)),
            None => println!("time-to-first-partial: (no partial produced)"),
        }
        println!("feed decode latency  : p50 {:.1} ms | p95 {:.1} ms | max {:.1} ms | n={}",
            ms(self.percentile(0.50)),
            ms(self.percentile(0.95)),
            ms(self.percentile(1.0)),
            self.feed_latencies.len(),
        );
        println!("─────────────────────────────────────────");
    }
}
