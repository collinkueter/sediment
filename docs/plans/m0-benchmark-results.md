# M0 benchmark results — on-device streaming ASR

**Status:** Open (awaiting runs on real hardware). The spike that produces these
numbers is [`spikes/m0-capture-asr`](../../spikes/m0-capture-asr); it answers the
[ADR-0017](../adr/0017-voice-and-meeting-transcription.md) plan's M0 decision gate.

This note is the *output* of M0. It is intentionally a fill-in template until the
runs happen — the leaderboard does not decide the model; these measurements do.

## How to read it

- **RTF** (real-time factor) = compute ÷ audio. Must be **< 1.0**; target **≤ 0.5**
  so diarization + speaker-ID + the rest of the app still fit in real time.
- **TTFP** = time-to-first-partial.
- **WER** is eyeballed for the spike (does the transcript read correctly?), not
  formally scored — formal WER is out of M0 scope.

## Results

Use the same WAV clip (ideally a real meeting with crosstalk) across rows so RTF is
comparable. Add a row per machine × model × provider.

| Machine (chip / RAM) | Model | Variant | Provider | RTF | TTFP (ms) | decode p95 (ms) | WER (eyeball) | Notes |
|---|---|---|---|---|---|---|---|---|
| _e.g. M2 Pro / 16 GB_ | streaming-zipformer-en | fp32 | coreml | | | | | |
| _e.g. M2 Pro / 16 GB_ | streaming-zipformer-en | int8 | cpu | | | | | |
| _e.g. mid-range x86 laptop_ | streaming-zipformer-en | int8 | cpu | | | | | |
| _…_ | Parakeet-TDT (offline+VAD) | int8 | cpu | | _segment-granular_ | | | via upstream example |

## Decisions this note must produce

1. **Default local model + size** for V1 (which row clears RTF ≤ 0.5 with acceptable
   WER). Confirms or revises ADR-0017 Q5.
2. **CPU floor** — the hardware below which RTF ≥ ~0.8, where V1 should nudge the user
   toward the cloud STT opt-in (ADR-0017 §2) instead of degrading silently.
3. **Streaming wording** — does Parakeet's accuracy justify segment-granular partials,
   or does streaming-zipformer's continuous partials win? Update ADR-0017 §2/Q5 to
   match what the numbers say (see the spike README §5 — Parakeet is *not* natively
   streaming in sherpa-onnx, contrary to the ADR's current phrasing).

## Findings log

_(Record surprises here as you run — e.g. "int8 halved RTF for ~no WER change",
"coreml provider slower than cpu on this clip", "loopback capture needs entitlement
X on macOS". These feed back into M2/M3.)_
