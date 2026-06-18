# ADR-0016: Voice and meeting transcription — the formation listens

**Status:** Proposed (2026-06-18) — the design captured here before any code, in
the ADR-first discipline of ADR-0008/0009. New domain terms (**Session**,
**Transcript segment**, **Voiceprint**) to be lifted into [CONTEXT.md](../../CONTEXT.md)
on acceptance. Build order in [docs/plans/voice-and-meeting-transcription.md](../plans/voice-and-meeting-transcription.md).
**Extends:** [ADR-0009](0009-conversational-agent.md) (a meeting is a new *source*
that lands in the **single conversation**, not a new mode), [ADR-0011](0011-working-set-and-push-grounding.md)
(the live transcript is grounding pushed into the turn, like the Working Set),
[ADR-0010](0010-daily-logs-and-recurring.md) (a **Meeting note** is a dated,
event-shaped Note alongside the Daily note — Events, not Facts, until the Agent
distils them).
**Reuses:** [ADR-0014](0014-bundled-in-process-embedder.md) (the bundled ONNX
runtime that already ships — speaker embeddings are *one more ONNX model* on it),
the Entity/Note/Fact model (a meeting is already an `entity_type`; a **Voiceprint**
is one more vector on a `person` entity, beside the `embedding` it already carries),
the snapshot/audit/undo machinery, the Tauri `Channel<T>` streaming used by
`chat_turn`.
**Keeps:** the local-only / "never leaves your machine" promise; no-daemon; BYOK
generation; no persistent second surface.

## Context

Sediment's primary input is conversation, but the richest conversations a user
has are not *with the agent* — they are the **meetings, calls, and rooms** the
user is already in. Today that knowledge is lost: the user must remember a call,
then re-narrate it to the agent from memory, hours later, degraded. The product
goal — *an assistant that gets to know you and the people around you* — is
starved of its best signal.

The ask: let the formation **listen**. Transcribe a meeting in real time;
recognise *who* is speaking and attach it to the person it already knows; let the
user **chat and take notes alongside the live transcript**, time-aligned to what
was said; and then feed all of it through the existing pipeline so attendees'
**Notes are updated**, **Facts** are recorded with provenance, and **connections**
are surfaced — the same knowledge-building the Agent already does for typed turns,
now fed by speech.

This is the first feature that pulls **audio** and **real-time, long-running
capture** into an app whose every commitment was written for *typed turns over
local Markdown*. Three of those commitments are in genuine tension and this ADR
must resolve them honestly, not wish them away:

1. **"Never leaves your machine"** (the landing-page promise, ADR-0014/0015). Most
   state-of-the-art ASR and speaker-recognition is a *cloud API*. Naively reaching
   for Deepgram/AssemblyAI would stream every meeting the user attends to a vendor
   — breaking the one promise the product is built on.
2. **No daemon, no second surface** (ADR-0009/0011 ruled out "the background daemon,
   the new-day/app-open catch-up turn… in favour of the in-reply rider"). A live
   meeting is *intrinsically* a long-running, real-time process with its own live
   UI — the exact shape ADR-0011 declined.
3. **BYOK generation, no new bill** (ADR-0008). Transcription is a *new class* of
   dependency the user does not already have installed and authenticated, unlike
   the Claude Code / Copilot CLI.

The market validates the local path. The closest analog to Sediment's ethos is
**Granola**: it does **not** join calls as a bot, captures **device audio**
(your mic + system output) locally, transcribes in real time, and **deletes the
audio** immediately — "no recordings stored." The bot-based tools (Otter's
OtterPilot, Fireflies' "Notetaker" guest) announce themselves to every
participant and upload A/V to a vendor cloud — categorically wrong for Sediment.
We follow Granola's capture posture and go further on privacy: **on-device
transcription by default**.

State of the art surveyed (2026):

- **On-device ASR.** `whisper.cpp` with Metal runs `large-v3` at ~5× real-time on
  an M2 Pro; `tiny`/`base` beat real-time on any modern CPU. NVIDIA **Parakeet** /
  `sherpa-onnx` give strong streaming ASR that runs on the **same ONNX runtime
  Sediment already bundles** (ADR-0014). This makes a fully-local default
  *feasible today*, not aspirational.
- **Cloud ASR (opt-in only).** For users who explicitly trade privacy for
  accuracy/latency: Deepgram Flux (lowest end-of-speech latency), ElevenLabs
  Scribe v2 Realtime (~150 ms first-partial, 90+ languages), AssemblyAI
  (transcript intelligence). Behind a per-formation opt-in, never the default.
- **Diarization** (*who spoke when*, unnamed). `pyannote` 3.1 (~10–19% DER) is the
  community standard; **diart** (pyannote segmentation + embedding) is the
  reference for *online/streaming* diarization. ONNX builds run in-process.
- **Speaker identification** (*which known person*, named). The voiceprint pattern:
  extract a speaker **embedding** (ECAPA-TDNN / x-vector, available as ONNX) per
  segment, compare by cosine to **enrolled** voiceprints. Picovoice **Eagle** is a
  production on-device option (Azure Speaker Recognition retired Sep 2025, Amazon
  Voice ID May 2026 — the industry is moving *on-device*, with us). A speaker
  embedding is *the same shape* as the note/entity embeddings we already store and
  index — this is not a new primitive, it is the existing one aimed at a voice.

## Decision

### 1. Capture device audio locally — no bot, no upload (the Granola posture)

A **Session** captures two local streams and never a remote one: the user's
**microphone** (via `cpal`, already a natural Rust choice) and **system output
loopback** (the meeting platform's audio) — **macOS ScreenCaptureKit** (13+),
**Windows WASAPI loopback**. The two streams are mixed/resampled to 16 kHz mono
PCM in Rust and fed to the transcriber. We **never** join the call as a
participant and **never** send audio to a meeting platform. This works with *any*
call surface (Zoom/Meet/Teams/in-person) because we listen to the *device*, not
the platform — Granola's key property.

Capture is gated on OS permission (macOS mic + Screen Recording; Windows mic).
The first Session walks the user through granting these, the same one-time posture
as the embedder/CLI probes on first run.

### 2. Transcribe on-device by default; cloud is an explicit, per-formation opt-in

The default **Transcription engine** is **local** — `whisper.cpp` (via `whisper-rs`)
or a `sherpa-onnx` streaming model on the **ONNX runtime ADR-0014 already ships**.
This keeps the "never leaves your machine" promise *literally* true for audio, the
most sensitive data the app has ever touched. A model is fetched on first use and
cached, exactly like the bundled embedder (ADR-0014) and `ort`'s ONNX runtime.

A **cloud engine** (Deepgram/AssemblyAI/ElevenLabs) is offered **only** behind an
explicit per-formation setting with a plain-language warning ("your meeting audio
will be streamed to <vendor>"). It is BYOK — the user's own key — never a Sediment
bill, matching ADR-0008's "no second bill" stance.

Whether this earns a `TranscriptionEngine` trait now is a real seam question
(ADR-0015 §6 / ADR-0008/0012): two *real* engines (local default + cloud opt-in)
is exactly the "two real implementations" bar that let `ConversationEngine` earn
its trait. We adopt a **thin** `TranscriptionEngine` trait from the start for that
reason — but ship V1 with the local engine wired and the cloud engine stubbed, so
the seam is proven by the default path before the opt-in lands.

### 3. A Session is a bounded, user-initiated capture — not a daemon

This is the load-bearing resolution of tension #2. ADR-0011 ruled out the
*background* daemon — the unbidden, always-watching process. A **Session** is its
opposite: **explicitly started and stopped by the user**, **bounded** in time,
**visible** while it runs, and **foreground** to the user's intent (they are *in*
the meeting). It never runs unbidden and never watches in the background. The
streaming transcription loop is a Tokio task scoped to the open Session and torn
down on stop — the same lifecycle as the 300 s `chat_turn` task, just longer-lived
and user-owned. No always-on listener, no calendar auto-join, no
"new-day/app-open" pass. When in doubt the bias is **off**: a Session must be a
deliberate act.

### 4. The live transcript is an ephemeral capture surface that *collapses into the
conversation* — not a persistent second surface

Tension #2's second half. ADR-0009/0011 forbid a *persistent* competing surface
(the Write/Ask split, a second durable pane). A live meeting needs *some* live UI —
but it is **transient capture chrome**, like an overlay/recording bar, that exists
only while the Session is open. When the Session ends, that surface **disappears**
and everything it produced **lands in the two durable surfaces that already exist**:
the **single conversation** (ADR-0009) and the **note viewer**. The durable model
of the app is unchanged — conversation + notes. The meeting is a *source that
flows into them*, exactly as ADR-0009 frames every input ("a meeting is just a new
source of Notes; the rest of the system already turns Notes into knowledge").

### 5. A meeting is a Meeting note + a meeting Entity — reuse, don't invent

On Session start, Sediment creates a **Meeting note** at `Meetings/<YYYY-MM-DD HHmm> — <title>.md`
and a reserved `entity:meeting` (the `meeting` `entity_type` **already exists** in
the schema). The note is an ordinary Note — viewable, Obsidian-editable, indexed,
revertable — structured as Sections:

```
## Attendees      - [[Sarah Chen]], [[Self]], (Unknown speaker 3)
## Notes          - the user's own typed notes + agent chat, time-anchored
## Transcript     - speaker-labelled, timestamped segments
## Action items   - distilled by the Agent (become Tasks, ADR-0007)
## Decisions      - distilled by the Agent
```

The Meeting note is **event-shaped**, the Daily-note half of the Event-vs-Fact
line (ADR-0010): the *transcript itself is not Facts*. Facts are what the **Agent
distils** from it afterward (§7), recorded with the usual bi-temporal /
contradiction discipline (ADR-0004) and `source_chat_id` provenance pointing at
the meeting.

### 6. Name speakers with **Voiceprints** — diarization + identification, on the
runtime we already ship

Two layers, both local, both ONNX (ADR-0014's runtime):

- **Diarization** segments the stream into *who-spoke-when* (Speaker 1/2/3),
  streaming, via a `sherpa-onnx`/diart-style segmentation+embedding model. Output:
  **Transcript segments** `{ start, end, speaker_local_id, text }`.
- **Identification** puts a *name* on a local speaker by extracting a speaker
  **embedding** (ECAPA-TDNN / x-vector, ONNX) for the segment and cosine-matching
  it against **enrolled Voiceprints**. A **Voiceprint** is a speaker embedding
  vector **stored on the `person` Entity** — the same primitive as the `embedding`
  field a person entity already carries, just a different vector. No new store, no
  new concept; the existing graph holds it.

**Enrollment is progressive and lazy** — the ADR-0015 ethos ("an Entity can exist
before its Note does", "no onboarding wizard"). The **Self**'s voice is enrolled
once with consent (the user is always on every call — high-value anchor). Others
enrol *in the flow*: when an unknown speaker is identified and the user tells the
Agent "that was Sarah" (typed in the live Notes, or after), the segment's embedding
is attached as a Voiceprint to `entity:sarah_chen`, and future meetings recognise
her. A match above threshold auto-labels; below threshold stays "Unknown speaker N"
until named. Identity is **suggested, never asserted** — a wrong guess is one edit
from corrected, like every other Agent output.

### 7. Knowledge-building runs through the *existing* Agent — speech is just a
bigger turn

The whole point: a transcribed meeting must **update the people's Notes** and
**build connections**, not just sit as a transcript. It does so through the
**existing conversational turn** (ADR-0009) — no parallel extraction pipeline
(ADR-0003/0006 were retired into the Agent for exactly this reason). Two moments:

- **Live, alongside the meeting (§4).** The user can chat with the Agent *during*
  the Session. Those are ordinary `chat_turn`s with one addition: the **rolling
  transcript window is pushed as grounding** (ADR-0011), so "what does Sarah mean
  by the Q3 number?" is answered against what was *just said*, time-aligned. The
  user's notes get context because they are written *beside* the transcript on the
  same timeline.
- **On Session end, a distillation turn.** The Agent reads the Meeting note and
  does what it already does: records Facts about attendees (`record_fact` —
  "Sarah now leads the migration" supersedes the old fact, ADR-0004), updates each
  attendee's People Note with its file tools, opens loops/Tasks for action items
  (ADR-0007), and surfaces connections (`related_facts`, `search_notes`). Because a
  full transcript can blow the context budget, distillation runs **segment-windowed**
  (chunk the `## Transcript` by speaker-turn/topic, ground per-window) — the same
  budgeting discipline ADR-0011 applies to injected context, applied to a long
  source. Output is **audited and undoable** per Fact, like any turn.

### 8. Time-alignment is an offset, recorded as the spine of the Meeting note

Every artifact in a Session carries an **audio offset** (ms from Session start):
each **Transcript segment**, each live chat turn, each typed note line. The Meeting
note's `## Notes` and `## Transcript` are two views of **one timeline**; the UI can
interleave them and click a note to jump to *what was being said at that moment*.
This is the "the user's notes have context" requirement, made concrete as a single
monotonic offset — cheap, deterministic, no new model.

### 9. Audio is transcribe-and-delete by default; retention is opt-in

Following Granola: captured PCM is held only in a ring buffer long enough to
transcribe, then **discarded** — the durable artifacts are the **text** transcript
and the derived knowledge, never the audio. A per-formation "keep recording" opt-in
(off by default) writes audio to `Meetings/.audio/<session>.wav` for users who
want playback, with the same plain-language framing as the cloud-STT opt-in.

### 10. Consent is a first-class concern, surfaced, not buried

Recording others has legal weight (one- vs two-party-consent jurisdictions). V1
shows a clear in-app reminder on Session start ("you are responsible for consent
to record") and supports an audible/visible cue posture; it does **not** silently
normalise covert recording. This is called out here so it is a designed decision,
not an afterthought.

## Consequences

- **Positive** — the formation gains its richest input source (live speech) while
  keeping every core promise: on-device by default (§2), no bot/no upload (§1), no
  daemon (§3), no persistent second surface (§4), reuse of Entity/Note/Fact and the
  ONNX runtime / audit / streaming machinery (§5, §6). The landing-page promise
  stays literally true for audio.
- **Positive** — speaker identification is *not* a new subsystem: a **Voiceprint**
  is one more vector on a person Entity (§6), riding ADR-0014's runtime. Progressive,
  lazy enrolment matches ADR-0015's no-wizard ethos.
- **Positive** — knowledge-building reuses the Agent wholesale (§7); a meeting is "a
  bigger turn," so attendee Notes, Facts, contradictions, Tasks, and connections all
  flow through one audited, undoable path. No second extraction pipeline.
- **Negative** — first **audio** dependency and first **native OS-permission**
  surface (mic, screen-recording): real cross-platform capture work
  (ScreenCaptureKit / WASAPI loopback) and a larger native footprint than the
  typed-text app has needed. New failure modes (device changes, permission revocation
  mid-Session).
- **Negative** — on-device ASR + diarization is **compute-heavy**; quality/latency
  scale with the user's hardware (a low-end CPU gets `tiny`-grade transcripts). The
  cloud opt-in (§2) is the escape valve, at a privacy cost the user must choose.
- **Negative** — diarization and identification are **probabilistic**; mislabels and
  "Unknown speaker N" will happen. Mitigated by suggest-not-assert (§6) and per-Fact
  undo, but the live transcript will not be perfectly attributed.
- **Negative** — §3/§4 walk a real line: a Session is the closest the app comes to
  the daemon/second-surface ADR-0011 declined. The bounded/user-initiated/ephemeral
  framing holds it on the right side, but it must be *built* that way, not drift.
- **Out of scope (V1)** — calendar auto-join / always-on listening (would re-open
  the daemon ADR-0011 declined); a bot that joins calls (wrong posture, §1);
  multi-device / shared-room capture; real-time translation; video; speaker
  *diarization quality* tuning beyond a solid default model.

## Open questions

1. **Transcription engine seam (§2).** Ship the `TranscriptionEngine` trait in V1
   (local wired, cloud stubbed), or stay concrete until the cloud engine is actually
   built? Leaning trait-now, since two real engines clear ADR-0008/0012's bar — but
   the cloud path may reveal the seam is wrong-shaped (streaming vs batch, partials).
2. **Distillation trigger (§7).** Always auto-run the end-of-Session distillation
   turn, or offer it ("process this meeting?")? Auto matches "the agent records what
   it learns"; offered respects the user's attention and the one-beat budget
   (ADR-0011 §4). Likely: auto-distil quietly, surface only a one-line summary +
   undo.
3. **Live grounding budget (§7).** How large a rolling transcript window to push per
   live turn before it crowds the Self/Working-Set slots (ADR-0011 §2, ADR-0015 §3)?
   Needs a cap like `SELF_SUMMARY_BUDGET`, felt-tested.
4. **Voiceprint drift & multi-enrolment (§6).** One voiceprint per person, or a
   centroid over several enrolments (voices vary by room/mic/health)? Start with a
   running centroid; revisit if false-matches appear. Cross-talk (two people at once)
   degrades both diarization and identification — accept for V1.
5. **Default local model (§2).** `whisper.cpp` (mature, Metal-fast, batch-leaning)
   vs a `sherpa-onnx` streaming model (lower-latency partials, reuses `ort`). Lean
   `sherpa-onnx`/Parakeet for true streaming on the runtime we already ship; bench
   on real hardware before locking.
