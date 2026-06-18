# M0 spike — on-device streaming ASR latency/RTF

De-risking spike for [ADR-0016](../../docs/adr/0016-voice-and-meeting-transcription.md)
(see plan [M0](../../docs/plans/voice-and-meeting-transcription.md)). It answers the
decision gate: **is on-device streaming ASR fast enough on real hardware**, and what
is the time-to-first-partial? Record the numbers in
[`docs/plans/m0-benchmark-results.md`](../../docs/plans/m0-benchmark-results.md).

> **Run this on your Mac / Windows machine, not in CI.** It was scaffolded on a
> headless Linux box with no audio hardware; the numbers that matter only exist on
> the hardware Sediment will actually run on. The crate is standalone (not part of
> the app build) and disposable.

## What it measures

- **RTF** (real-time factor) = compute time ÷ audio duration. Must be **< 1.0**;
  we want **≪ 1.0** so diarization + speaker-ID + the rest of the app still fit.
- **Time-to-first-partial** — how long until the first words appear.
- **Per-chunk decode latency** — p50 / p95 / max.

## Scope (and what's deliberately out)

In: streaming-zipformer ASR over a WAV (deterministic) or the mic (live feel), the
resample-to-16 kHz pipeline, the bench harness.

Out: **system-output loopback capture** (the meeting audio — macOS ScreenCaptureKit,
Windows WASAPI). That is the platform half of ADR-0016 §1 and belongs to plan **M2**;
it cannot compile on the Linux box this was scaffolded on, and measuring ASR
latency/RTF only needs *some* 16 kHz stream, which mic + WAV give. The loopback crates
to reach for in M2: `screencapturekit` (macOS 13+) and `wasapi` (Windows).

## 1. Build prerequisites

The `sherpa-onnx` crate links the native sherpa-onnx / onnxruntime library. If the
build can't find a prebuilt binary it builds from source and needs **CMake** + a C++
toolchain. See <https://docs.rs/sherpa-onnx>. On macOS you can pass `--provider coreml`
once it builds; `cpu` is the safe default everywhere.

## 2. Download a streaming model

A streaming **zipformer transducer** (natively online). English demo model:

```bash
cd spikes/m0-capture-asr
curl -SL -O https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26/resolve/main/encoder-epoch-99-avg-1-chunk-16-left-128.onnx
curl -SL -O https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26/resolve/main/decoder-epoch-99-avg-1-chunk-16-left-128.onnx
curl -SL -O https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26/resolve/main/joiner-epoch-99-avg-1-chunk-16-left-128.onnx
curl -SL -O https://huggingface.co/csukuangfj/sherpa-onnx-streaming-zipformer-en-2023-06-26/resolve/main/tokens.txt
# Verify the exact filenames after download (they vary by model): ls *.onnx
```

The pretrained-model index (other sizes, int8, multilingual) is at
<https://k2-fsa.github.io/sherpa/onnx/pretrained_models/index.html>. Try the **int8**
encoder/joiner variants too — they usually cut RTF substantially.

## 3. Run

WAV (the reproducible measurement — use a real meeting-style clip with crosstalk):

```bash
cargo run --release --bin m0 -- \
  --wav sample-meeting.wav \
  --encoder encoder-epoch-99-avg-1-chunk-16-left-128.onnx \
  --decoder decoder-epoch-99-avg-1-chunk-16-left-128.onnx \
  --joiner  joiner-epoch-99-avg-1-chunk-16-left-128.onnx \
  --tokens  tokens.txt \
  --provider cpu
```

Mic (live feel, 20 s):

```bash
cargo run --release --bin m0 -- --mic --seconds 20 \
  --encoder ... --decoder ... --joiner ... --tokens tokens.txt --provider cpu
```

Useful flags: `--chunk-ms 100` (feed granularity), `--realtime` (pace the WAV at real
time instead of max-speed), `--provider coreml|cuda`.

## 4. Record results

Fill in [`docs/plans/m0-benchmark-results.md`](../../docs/plans/m0-benchmark-results.md)
for each machine/model/provider. The decision gate wants:

1. The **default model size** (full vs int8) that holds RTF ≪ 1.0 with acceptable WER.
2. The **CPU floor** below which V1 should nudge the user toward the cloud opt-in.

## 5. The Parakeet comparison (ADR-0016 Q5)

ADR-0016 locked "sherpa-onnx/Parakeet" expecting *true streaming*. **Finding from
scaffolding:** in sherpa-onnx, **Parakeet-TDT is an *offline* model run under VAD-based
*simulated* streaming** (interim results ~every 0.2 s *within* a speech segment) — only
streaming-zipformer is natively online. So the real Q5 trade is:

- **streaming-zipformer** → true sub-second partials (this spike).
- **Parakeet-TDT (offline + VAD)** → likely higher accuracy, but partials arrive at
  *speech-segment* granularity, not continuously.

Benchmark Parakeet with sherpa-onnx's own example and compare interim latency:
`rust-api-examples/examples/parakeet_tdt_simulate_streaming_microphone.rs`. If
zipformer's accuracy is acceptable, the "true streaming" wording in ADR-0016 §2/Q5
stands; if we need Parakeet's accuracy, update the ADR to say partials are
segment-granular. **This is the decision M0 exists to settle with data.**
