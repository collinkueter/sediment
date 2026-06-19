# Sediment — Plan: voice and meeting transcription

**Status:** Proposed (2026-06-18) — see [ADR-0017](../adr/0017-voice-and-meeting-transcription.md).
**Predecessor:** current HEAD on `claude/voice-meetings-transcription-we16n5`.
Builds on the bundled ONNX runtime (ADR-0014), the conversational turn (ADR-0009),
push-grounding (ADR-0011), and the audit/undo + `Channel<T>` streaming machinery.

This is the build order for letting the formation **listen**: capture device
audio locally, transcribe and diarise on-device, name speakers via per-person
**Voiceprints**, let the user chat/note alongside the live transcript, and feed
the result through the existing Agent so attendees' Notes, Facts, and connections
are updated. ADR-0017 holds the rationale and the resolved tensions; this is the
sequence.

---

## Implementation status (2026-06-18)

Built and **verified in CI** (the default build + tests, 134 passing; the `audio`
feature additionally `cargo check`ed):

- **M1 — Session + Meeting note.** `core::session` (`MeetingSession`,
  `SessionRegistry`, `SessionEvent`), `core::meeting_note` (writer + section
  splices), `commands::session` (`session_start/push_segment/push_note/
  rename_speaker/stop`), `MeetingSessionBar.tsx`. The manual `push_segment` is the
  fake source that proves the spine.
- **M2 — capture→transcription pipeline.** `core::audio` (downmix + resampler),
  `core::transcription` (`Transcriber` seam + `MockTranscriber`), `core::capture`
  (`CaptureSource` + cpal `MicSource` behind the **`audio`** feature),
  `core::capture_pipeline` (orchestrator + `CaptureController` lifecycle). Segments
  flow through the shared `session::record_segment` path.
- **M4 (data layer) — Voiceprints.** `entity.voiceprint`/`voiceprint_n` schema +
  `enroll_voiceprint` (running centroid) + `match_voiceprint` (cosine, threshold).
- **M5 — live transcript grounding.** `meeting_note::recent_transcript_grounding`
  + `SessionRegistry::live_transcript_grounding`, injected by `chat_turn` below the
  Self/Working-Set slots.
- **M6 (core) — segment-windowing.** `meeting_note::transcript_windows`.
- **Naming speakers.** `meeting_note::rename_speaker` + `session_rename_speaker`
  (the "that was Sarah" hand-correction).

### Hardware / runtime handoff — landed on macOS (2026-06-18)

The native-runtime work below was built and verified on Apple Silicon (the default
`desktop-audio` feature, on by default for dev; CI stays on `--no-default-features`).
What remains is the distillation turn (needs the agent CLI) and on-device validation
of the Windows loopback path.

- **M3 — real on-device ASR. DONE.** `sherpa-onnx` behind **`local-asr`**;
  `LocalTranscriber: Transcriber` in `core::transcription` (streaming-zipformer,
  the M0-benched model). `core::asr_model` provisions the model files (download /
  import, validate-by-load, atomic promote) and `commands::asr` exposes readiness +
  acquisition. Verified end-to-end by an ignored test that transcribes the spike WAV
  verbatim (`transcription::tests::real_wav_transcribes`).
- **ORT coexistence gotcha (resolved).** `sherpa-onnx` statically links its own
  newer ONNX Runtime; `ort` (the bundled embedder) statically linked an older one —
  two runtimes in one binary, the older winning symbol resolution and crashing
  sherpa (SIGSEGV, "requested API version 24 … only 1,20 supported"). Fix: switch
  the embedder to `fastembed/ort-load-dynamic` under `local-asr` so `ort` loads its
  runtime at process start instead of static-linking; `core::ort_runtime` provisions
  a `libonnxruntime` dylib and sets `ORT_DYLIB_PATH`. Proven by
  `transcription::tests::embedder_and_asr_coexist` (embed + ASR in one process).
- **Loopback capture. DONE (macOS verified; Windows written, awaiting on-device
  validation).** `core::capture` adds `ScreenCaptureSource` (macOS ScreenCaptureKit
  system audio) and `WasapiLoopbackSource` (Windows WASAPI render-loopback), mixed
  with `MicSource` by a mic-driven `Mixer`/`MixedSource` (resample each → 16 kHz mono
  → sum). macOS needs `NSMicrophoneUsageDescription` (`Info.plist`) + a `/usr/lib/swift`
  rpath (`build.rs`) so the Swift-backed ScreenCaptureKit binary loads. Consent
  reminder (ADR-0017 §10) still TODO in the UI.
- **M4 runtime — diarization + identification. DONE.** `core::diarization::Diarizer`
  extracts a per-segment ECAPA embedding (`SpeakerEmbeddingExtractor`) and assigns a
  speaker by nearest-centroid clustering, seeded from enrolled Voiceprints
  (`MemoryStore::all_voiceprints`) so a known voice is auto-named. `session_rename_speaker`
  persists the named speaker's centroid via `enroll_voiceprint_named` (progressive
  enrolment). Verified by `capture_pipeline::tests::real_pipeline_wav_to_segments`.
- **M6 — distillation turn. DONE.** `core::distillation::distill_meeting` runs on
  `session_stop` (spawned in the background so Stop returns at once), grounding the
  cold Claude Code engine on the segment-windowed transcript and instructing it to
  record Facts / update People notes / open Tasks / capture Decisions — distil, not
  dump, with named attribution gated on a clear speaker (Gap B). It reuses the
  `chat_turn` snapshot→diff→audit path, so the whole turn is one undoable entry; the
  one-line receipt + `turn_id` stream back as a `SessionEvent::Distilled` that the
  capture bar surfaces with an Undo (Q2). Runs only when something was transcribed.
  *Needs the Claude Code CLI at runtime; the orchestration + pure helpers are tested,
  the live agent turn validates on a real meeting.*

### Post-V1 refinements (2026-06-19) — ADR-0017 [Amendment](../adr/0017-voice-and-meeting-transcription.md#amendment-2026-06-19--two-pass-accuracy-live-naming-voice-clips)

Built and **verified** (both feature sets clippy-clean under `-D warnings`; 158 tests
pass on the default build, 156 on `--no-default-features`; frontend `tsc`/Biome/Vite
green):

- **Two-pass transcription. DONE.** Live streaming + an offline second pass at stop:
  `core::audio::split_on_silence` → `transcription::OfflineTranscriber`
  (`sherpa-onnx` `OfflineRecognizer`, NeMo Parakeet-TDT) → re-diarize each segment with
  the existing `Diarizer` seeded from named live speakers → `meeting_note::replace_transcript`,
  then distillation reads the refined transcript. `asr_model` provisions the offline
  model like the streaming one; streaming decode bumped to `modified_beam_search`.
  *The offline model files are confirmed on hardware (M0-style) before locking.*
- **Voice clips. DONE.** `audio::write_wav_i16` → `People/.voices/<Name>.wav`,
  `entity.voice_clip` + `memory::{set_voice_clip,voice_clip_path}`, `read_voice_clip`
  command; written for named speakers (live rename or second pass); played from the
  post-meeting speaker panel. Full audio stays memory-only (§9).
- **Live current-speaker naming + heard-name suggestion. DONE.** `core::name_detect`
  (self-introduction heuristic) → `SessionEvent::SpeakerNameSuggested`; the recording
  bar tracks the current speaker and surfaces a one-tap rename. `SessionEvent::TranscriptRefined`
  reloads an open note after the rewrite.

---

## Context for a fresh session

Today there is **no audio path at all** (Explore confirmed: no mic, no upload, no
STT). Input is typed turns via `commands/chat.rs::chat_turn`; knowledge lands as
Entities/Facts in SurrealDB (`core/memory.rs`) and Markdown Notes in the formation;
the bundled embedder (`core/bundled_embed.rs`) already runs ONNX via `ort`;
streaming to the UI uses Tauri `Channel<T>`; the `meeting` `entity_type` already
exists in the schema but is unused.

The work adds: a **capture** subsystem, a **transcription** subsystem, a
**diarization + speaker-ID** subsystem, a **Session** lifecycle that ties them
together and streams to a transient live UI, a **Meeting note** writer, and a
**distillation** turn that reuses the Agent. Everything stays local by default.

---

## Milestones

### M0 — Spike: prove local capture + ASR on real hardware *(de-risk first)*

Before committing the architecture, validate the riskiest unknowns on a real
machine (ADR-0008's "verify against the binary, don't assume"). **Scaffolded:**
[`spikes/m0-capture-asr`](../../spikes/m0-capture-asr) — a standalone, disposable
crate (not part of the app build) wiring the official `sherpa-onnx` 1.13 streaming
recognizer over a WAV (deterministic) or the mic, with an RTF / TTFP / decode-latency
harness. Results template: [`m0-benchmark-results.md`](m0-benchmark-results.md).

- ASR path (the decision gate) is wired: `cpal` mic + WAV → resample to 16 kHz mono
  → streaming-zipformer → RTF + time-to-first-partial.
- **System loopback** (macOS ScreenCaptureKit, Windows WASAPI) is *not* in the spike
  — it can't compile on the Linux scaffold box and isn't needed to measure ASR
  latency. It moves to **M2** (crates: `screencapturekit`, `wasapi`).
- **Finding while scaffolding (feeds the decision gate):** in sherpa-onnx,
  NVIDIA **Parakeet-TDT is *offline* + VAD-simulated streaming**, not natively online
  — only streaming-zipformer gives continuous sub-second partials. M0 must bench both
  and decide whether Parakeet's accuracy justifies *segment-granular* partials, then
  reconcile ADR-0017 §2/Q5's "true streaming" wording.
- **Decision gate:** pick the default model + size that holds RTF ≤ 0.5 with
  acceptable WER; confirm the CPU floor below which V1 nudges to the cloud opt-in;
  settle the zipformer-vs-Parakeet streaming question.

Output: the filled-in benchmark note in `docs/plans/`; the spike crate is disposable.

### M1 — Session lifecycle + Meeting note (text-only, no audio yet)

Get the *shape* right before the hard audio work, so the rest builds on a stable
spine:

- `core/session.rs`: a `MeetingSession` (id, started_at, title, audio-offset clock,
  rolling `Vec<TranscriptSegment>`, attendee set). Start/stop are explicit Tauri
  commands — **user-initiated, bounded** (ADR-0017 §3).
- `commands/session.rs`: `session_start`, `session_stop`, streaming `Channel<SessionEvent>`
  (reuse the `chat_turn` streaming pattern). Events: `segment`, `attendeeChanged`,
  `status`.
- Meeting note writer: create `Meetings/<YYYY-MM-DD HHmm> — <title>.md` with the
  `## Attendees / ## Notes / ## Transcript / ## Action items / ## Decisions`
  sections (ADR-0017 §5); reserve `entity:meeting`; index it like any Note.
- Feed **fake** segments through the pipe end-to-end (UI → note) to validate the
  spine without audio.

### M2 — Local capture wired into the Session

- Promote the M0 spike to `core/audio_capture.rs`: a `CaptureStream` trait with
  `MacCapture` (ScreenCaptureKit) and `WindowsCapture` (WASAPI loopback)
  implementations; mic via `cpal`; mix + resample to 16 kHz mono.
- OS-permission onboarding (mic + macOS Screen Recording) on first Session, mirroring
  the embedder/CLI first-run probes.
- Ring-buffer audio; **transcribe-and-delete by default** (ADR-0017 §9); per-formation
  "keep recording" opt-in writing `Meetings/.audio/<session>.wav`.
- Consent reminder on Session start (ADR-0017 §10).

### M3 — Transcription engine (local default)

- `core/transcription.rs`: thin `TranscriptionEngine` trait (ADR-0017 §2, open Q1) —
  `LocalTranscriber` wired (chosen M0 model on the `ort` runtime ADR-0014 ships),
  `CloudTranscriber` stubbed.
- Stream partial + final **Transcript segments** into the Session; append finals to
  the Meeting note's `## Transcript` with audio offsets.
- Model fetch-on-first-use + cache, exactly like the bundled embedder.

### M4 — Diarization + speaker identification (Voiceprints)

- `core/diarization.rs`: streaming segmentation+embedding (sherpa-onnx/diart-style)
  → `speaker_local_id` per segment.
- `core/voiceprint.rs`: extract a speaker **embedding** per segment (ECAPA/x-vector
  ONNX); cosine-match against enrolled Voiceprints.
- Storage: a `voiceprint` vector field on the `person` Entity (beside its existing
  `embedding`) — **no new store** (ADR-0017 §6). Add to `core/memory.rs` schema.
- **Self** voice enrolment (consented, one-time); **progressive** enrolment for
  others: when the user names an unknown speaker, attach that segment's embedding as
  a Voiceprint to the person Entity. Auto-label above threshold, "Unknown speaker N"
  below — **suggest, never assert** (per-edit undo).
- Running-centroid voiceprint (ADR-0017 open Q4).

### M5 — Live chat alongside the meeting (time-aligned)

- Extend `chat_turn` grounding: when a Session is open, push the **rolling transcript
  window** as a grounding slot (ADR-0011 §2) under a budget cap (open Q3), ranked
  below Self/Working-Set.
- Stamp every live turn and typed note line with the Session **audio offset**; write
  them into the Meeting note `## Notes` on the shared timeline (ADR-0017 §8).
- Live UI: transient capture overlay/bar (ADR-0017 §4) showing the rolling transcript
  + a notes/chat field; **disappears on Session stop**.

### M6 — End-of-Session distillation turn

- On `session_stop`, run a **segment-windowed** distillation turn (ADR-0017 §7):
  chunk `## Transcript` by speaker-turn/topic, ground per window, and let the Agent
  record Facts about attendees, update People Notes, open Tasks/loops for action
  items (ADR-0007), and surface connections — all through the existing tools, fully
  **audited and undoable**.
- Quiet by default: auto-distil, surface a one-line summary + undo (ADR-0017 open Q2).
- Prompt work in `prompts/conversation-agent.md`: meeting-aware distillation rider
  (attendees, decisions, action items; suppress per-utterance Facts — distil, don't
  dump).

### M6.5 — Quick-capture (Pocket-style): voice *and* typed (ADR-0017 Appendix A)

Fast-follow once the Session spine (M1–M6) exists — mostly UI, no new subsystems:

- Global hotkey + menu-bar item opening a small capture popover from anywhere
  (Tauri global-shortcut + a lightweight always-available window).
- **Voice mode:** start a *lightweight Session* (no meeting framing) reusing
  M2–M6; on close, land a `Captures/<date>.md` entry (or append to the Daily note
  `## Notes`, ADR-0010) and run a memo-scale distillation turn (M6).
- **Typed mode:** one-field quick-note → `chat_turn` (ADR-0009) or Daily-note
  append. The explicit "with typing, not just voice" path — one surface, two input
  modes, one conversation behind both.
- Summary-style presets (brief / decisions-only / narrative) as prompt options
  (ADR-0017 Appendix A, extends §7).
- *(Optional, later)* a read-only **graph view** of an entity's neighbourhood —
  the persistent answer to Pocket's per-recording "mind map" (no new model, just
  a view over existing Entities/Facts).

### M7 — Polish, settings, docs

- Settings: transcription engine (local default / cloud opt-in BYOK with warning),
  keep-audio toggle, default mic/loopback devices, voiceprint management (list/forget).
- Error/edge handling: device change mid-Session, permission revocation, silence,
  cross-talk degradation.
- Update README, CONTEXT.md (lift **Session / Transcript segment / Voiceprint**), and
  Windows/macOS permission notes (`docs/windows.md`).

---

## Sequencing rationale

M0 de-risks the two things most likely to sink the feature (loopback capture,
on-device latency) before any architecture is committed. M1 nails the Session/Note
*shape* with fake data so M2–M4's hard native work has a stable target. Speaker-ID
(M4) deliberately lands **after** plain transcription (M3) works — a labelled-but-
imperfect transcript is already useful, and Voiceprints reuse infra M3 doesn't need.
Live chat (M5) and distillation (M6) are the payoff and come last because they only
need the transcript to exist, however attributed. Quick-capture (M6.5, ADR-0017
Appendix A — the Pocket-style voice/typed capture) is a fast-follow that reuses the
whole stack as UI + a hotkey. Each milestone is independently shippable and leaves
the app working.

## Risks / watch-items

- **The §3/§4 line.** Keep the Session bounded/user-initiated/ephemeral as built —
  resist any drift toward auto-join, always-on listening, or a persistent pane.
- **Compute.** On-device ASR+diarization is heavy; the cloud opt-in is the escape
  valve, but the default must stay usable on mid-range hardware (drove M0).
- **Privacy surface area.** Audio + OS permissions + (optional) cloud STT are the
  largest expansion of the threat surface the app has made — defaults must stay
  local, transcribe-and-delete, and consent-aware.
