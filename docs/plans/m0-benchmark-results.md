# M0 benchmark results — on-device streaming ASR

**Status:** First runs recorded (2026-06-18, Apple M3 Pro). The decision gate is
**cleared with large headroom** — streaming-zipformer (fp32) holds RTF ≈ 0.05 on
this machine, ~10× under the ≤ 0.5 target. The spike that produces these numbers is
[`spikes/m0-capture-asr`](../../spikes/m0-capture-asr); it answers the
[ADR-0017](../adr/0017-voice-and-meeting-transcription.md) plan's M0 decision gate.
Still open: an int8 row, the Parakeet-TDT comparison, and a low-end / x86 floor
machine.

This note is the *output* of M0 — the measurements decide the model, not the
leaderboard.

## How to read it

- **RTF** (real-time factor) = compute ÷ audio. Must be **< 1.0**; target **≤ 0.5**
  so diarization + speaker-ID + the rest of the app still fit in real time.
- **TTFP** = time-to-first-partial.
- **WER** is eyeballed for the spike (does the transcript read correctly?), not
  formally scored — formal WER is out of M0 scope.

## Results

Use the same WAV clip across rows so RTF is comparable. Add a row per machine ×
model × provider. The clips below are the model repo's `test_wavs` (clean,
single-speaker English read aloud — a real meeting with crosstalk would stress WER
harder, but RTF/TTFP — the feasibility numbers M0 exists to settle — hold regardless).

| Machine (chip / RAM) | Model | Variant | Provider | RTF | TTFP (ms) | decode p95 (ms) | WER (eyeball) | Notes |
|---|---|---|---|---|---|---|---|---|
| Apple M3 Pro / 18 GB | streaming-zipformer-en-2023-06-26 | fp32 | cpu | 0.053 | 56 | 17.3 | clean (verbatim) | 16.7 s clip, max-speed; model ready in 1.15 s |
| Apple M3 Pro / 18 GB | streaming-zipformer-en-2023-06-26 | fp32 | coreml | 0.053 | 54 | 17.2 | clean (verbatim) | identical to cpu — fp32 graph runs on CPU EP, no CoreML speedup |
| Apple M3 Pro / 18 GB | streaming-zipformer-en-2023-06-26 | fp32 | cpu | 0.053 | 55 | 17.4 | clean (verbatim) | 2nd clip (6.6 s), confirms RTF is clip-independent |
| _TODO low-end / x86 laptop_ | streaming-zipformer-en | int8 | cpu | | | | | the CPU-floor row |
| _TODO_ | streaming-zipformer-en | int8 | cpu | | | | | int8 vs fp32 on this machine |
| _TODO_ | Parakeet-TDT (offline+VAD) | int8 | cpu | | _segment-granular_ | | | via upstream example |

## Decisions this note must produce

1. **Default local model + size** for V1 — _settled for Apple Silicon:_
   streaming-zipformer fp32 clears RTF ≤ 0.5 with ~10× headroom (0.053) and verbatim
   accuracy on clean speech, so it is a safe V1 default on M-series. int8 is not
   *needed* here (it would only widen the margin); it stays a TODO for the low-end
   floor where the headroom is the question. Confirms ADR-0017 Q5's choice of
   streaming-zipformer as the streaming path.
2. **CPU floor** — _still open._ The M3 Pro is far above the floor; the row that
   matters is a low-end / x86 laptop where RTF approaches ~0.8 and V1 should nudge
   toward the cloud STT opt-in (ADR-0017 §2). Needs a second machine.
3. **Streaming wording** — streaming-zipformer gives genuinely continuous partials:
   the bench printed a fresh partial roughly every chunk (~17 ms decode, n=168 over
   the 16.7 s clip), first words at 56 ms. The "true sub-second partials" wording in
   ADR-0017 §2/Q5 holds for zipformer. The Parakeet-TDT comparison (segment-granular
   under VAD — see spike README §5) is still TODO before locking the model *choice*,
   but the wording does not need to change for the zipformer path.

## Findings log

- **fp32 is already ~10× under target on M3 Pro** (RTF 0.053). int8 is not required
  on Apple Silicon for feasibility; reserve it for the low-end floor measurement.
- **CoreML provider gives no speedup here** — `--provider coreml` measured identical
  to `--provider cpu` (0.053). The downloaded fp32 zipformer graph executes on the
  CPU EP regardless; a CoreML benefit (if any) would need a CoreML-targeted/int8
  export. Don't assume `coreml` helps without measuring.
- **TTFP ~55 ms and clip-independent**; per-chunk decode p95 ~17 ms, max ~20 ms —
  comfortably real-time per 100 ms chunk, leaving budget for diarization + speaker-ID.
- **Model load ~1.15 s** (cold, fp32) — one-time per session, not per chunk.
- **Spike fix:** `OnlineRecognizer::create` returns `Option`, not `Result`, in
  sherpa-onnx 1.13.3 (the crate was scaffolded on a headless Linux box and had never
  been compiled). Fixed in `src/asr.rs` to `.ok_or_else(...)?`. The rest of the
  online-ASR API in the spike matched 1.13.3 as written.
- **Build needs no CMake on macOS** — the `sherpa-onnx` 1.13 crate pulled a prebuilt
  native lib; the README's CMake/C++ prerequisite is only the from-source fallback.
