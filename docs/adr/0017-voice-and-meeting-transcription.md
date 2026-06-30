# ADR-0017: Voice and meeting transcription — the formation listens

**Status:** Accepted (2026-06-18) — designed before any code (ADR-first, ADR-0008/0009)
and refined through a structured grilling session the same day: Q1, Q2, Q4, Q5, Q6
resolved, Gaps A and B surfaced and resolved as Q7/Q8, Q3 deferred to a felt-test
loop (see Open questions). Domain terms (**Session**, **Meeting note**,
**Transcript segment**, **Voiceprint**) captured in [CONTEXT.md](../../CONTEXT.md).
Build order in [docs/plans/voice-and-meeting-transcription.md](../plans/voice-and-meeting-transcription.md).
**Renumbered 0016 → 0017 (2026-06-18):** originally authored as ADR-0016, renumbered
on merge with `main`, which had concurrently landed [ADR-0016: local-only model
provisioning](0016-local-only-model-provisioning.md). Unrelated ADR; the number was
the only collision.
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
**Amended (2026-06-19):** two-pass transcription, live speaker naming + verbal-name
suggestion, and per-person voice clips — see [Amendment](#amendment-2026-06-19--two-pass-accuracy-live-naming-voice-clips) below.

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

**Labels and Facts treat confidence asymmetrically** (Resolved Q4 + Gap B,
2026-06-18). A transcript **label** is liberal — guess the speaker even when
unsure, because the error is *visible* in the live transcript and one click to fix.
A **Fact** attributed to that speaker is conservative — gated behind a higher
identification-confidence threshold (§7), because that error is *silent* and
propagates into a real person's Note. Cheap-to-correct guesses run free; expensive,
invisible ones do not. Voiceprints start as a running centroid per person and
accept cross-talk degradation for V1.

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
  same timeline. **Live chat requires the warm engine** (Copilot ACP, ADR-0012):
  cold-spawning Claude Code per turn (~6 s) is unusable mid-meeting, so in-meeting
  chat is gated to the resident engine (~100 ms); cold Claude Code is used for the
  post-meeting distillation turn only, where 6 s is irrelevant. (Resolved Gap A,
  2026-06-18.)
- **On Session end, a distillation turn.** The Agent reads the Meeting note and
  does what it already does: records Facts about attendees (`record_fact` —
  "Sarah now leads the migration" supersedes the old fact, ADR-0004), updates each
  attendee's People Note with its file tools, opens loops/Tasks for action items
  (ADR-0007), and surfaces connections (`related_facts`, `search_notes`). It runs
  **automatically and quietly**, surfacing a **one-line summary + undo** rather than
  asking first or writing silently (Resolved Q2, 2026-06-18) — "the agent records
  what it learns," but the user always sees that it did and can revert per Fact.
  **A Fact is attributed to a *named* speaker only above an identification-confidence
  threshold**; below it, the Fact is recorded *unattributed* (or to "a participant"),
  never pinned to the wrong person — because a mis-attributed Fact is *silently*
  wrong and propagates into a real person's Note, unlike a visibly-wrong transcript
  line (Resolved Gap B, 2026-06-18). Because a full transcript can blow the context
  budget, distillation runs **segment-windowed** (chunk the `## Transcript` by
  speaker-turn/topic, ground per-window) — the same budgeting discipline ADR-0011
  applies to injected context, applied to a long source. Output is **audited and
  undoable** per Fact, like any turn.

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

Most were resolved in a grilling session (2026-06-18); the strikethroughs record
the decisions and their rationale, ADR-0015 style.

1. ~~**Transcription engine seam (§2).**~~ **Resolved (2026-06-18) — stay concrete.**
   Ship `LocalTranscriber` as a plain struct; extract the `TranscriptionEngine` trait
   only when the cloud engine is *actually built*. A trait designed against one real
   implementation and one stub is shaped by a guess — and local (raw frame-driven
   partials) vs cloud (a vendor's websocket endpointing/partial/reconnect semantics)
   are not symmetric. This is exactly how `ConversationEngine` earned its trait
   (ADR-0008/0012): *two real engines*, never one and a promise.
2. ~~**Distillation trigger (§7).**~~ **Resolved (2026-06-18) — auto-run, surface a
   one-line summary + undo.** Matches "the agent records what it learns" while keeping
   the write *visible*: never silent, never a gating prompt. Per-Fact undo (the audit
   log) is the safety net; the summary is the receipt. See §7.
3. **Live grounding budget (§7).** *Deferred to a felt-test loop* — not a desk
   decision. Start at ≈2000 chars / most-recent ~90 s of transcript, ranked **below**
   the Self (ADR-0015 §3) and Working Set (ADR-0011 §2); tune in the running app like
   `SELF_SUMMARY_BUDGET`. No design change pending.
4. ~~**Voiceprint drift & multi-enrolment (§6).**~~ **Resolved (2026-06-18) — running
   centroid; asymmetric confidence.** One running-centroid Voiceprint per person.
   Labels are liberal (visible, one-click fix), Fact attribution is gated (silent,
   propagates) — see §6/§7. Cross-talk degradation accepted for V1.
5. ~~**Default local model (§2).**~~ **Resolved (2026-06-18) — sherpa-onnx/Parakeet.**
   Non-English meetings are *not* a V1 requirement, so the default is
   **sherpa-onnx/Parakeet**: true streaming partials (which §4's live transcript
   needs) on the `ort` ONNX runtime Sediment **already ships** — no second native
   runtime. Multilingual is served later by the BYOK cloud opt-in (§2), not by
   swapping in a heavier local runtime. **M0 still benches it on real hardware**
   before locking the exact model size; the *direction* (streaming ONNX, not
   whisper.cpp) is now fixed. **M0-scaffolding caveat (2026-06-18):** in sherpa-onnx,
   NVIDIA **Parakeet-TDT is an *offline* model run under VAD-simulated streaming**,
   not natively online — only streaming-*zipformer* gives continuous sub-second
   partials. So §2's "true streaming" is delivered by streaming-zipformer; if the
   bench shows we need Parakeet's accuracy, partials become *segment-granular* and
   §2/this answer must be reworded. Settled by the M0 numbers (see
   `docs/plans/m0-benchmark-results.md`).
6. ~~**Quick-capture scope (Appendix A).**~~ **Resolved (2026-06-18) — typed in V1,
   voice as fast-follow.** The typed path (global hotkey → `chat_turn`/Daily-note
   append) is nearly free and directly answers "with typing, not just voice," so it
   ships with the core. The voice path waits for the meeting stack (M2–M6) to land.
   See plan M6.5.
7. ~~**Live-chat engine constraint (Gap A, §7).**~~ **Resolved (2026-06-18) — live
   chat requires the warm engine.** In-meeting chat is gated to the resident Copilot
   ACP engine (~100 ms); cold Claude Code (~6 s spawn) is used for post-meeting
   distillation only. See §7.
8. ~~**Low-confidence Fact attribution (Gap B, §7).**~~ **Resolved (2026-06-18) —
   threshold-gate named attribution.** Below the identification-confidence threshold,
   a Fact is recorded unattributed rather than pinned to a guessed person. See §6/§7.

---

## Amendment (2026-06-19) — two-pass accuracy, live naming, voice clips

Dogfooding the V1 stack surfaced four gaps; resolved as follows, refining (not
reversing) the decisions above.

### A1. Transcription is **two-pass** (refines §2)

The streaming-zipformer (§5/Q5) is tuned for sub-second partials, so its live
transcript is rough. We keep it for the *live* surface (§4 needs partials) and add a
**second pass**: when the **Session** stops, a non-streaming, high-accuracy **offline**
recognizer (NeMo **Parakeet-TDT** by default, on the same `sherpa-onnx`/ONNX runtime —
no new runtime, no cloud) re-transcribes the whole meeting and **replaces** the
`## Transcript`. Distillation (§7) reads this refined transcript. The live engine also
moves from greedy to `modified_beam_search` for a small no-cost lift. The offline model
is provisioned exactly like the streaming one (download or folder import, validated
before promote) and is part of the readiness gate. This keeps §2's "on-device by
default" intact — accuracy now comes from a *second local model*, not the cloud
opt-in.

### A2. Re-diarization seeds from named live speakers (refines §6)

The second pass re-diarizes the meeting and **seeds** identification with the
formation's enrolled **Voiceprints** *plus this meeting's named live speakers'
centroids* — so anyone named during the call (by hand or by A4) keeps their name in
the refined transcript; everyone else is freshly labelled `Unknown speaker N`.

**Update (2026-06-29) — the `OfflineSpeakerDiarization` pipeline is now the default
second-pass diarizer (`core::speaker_diarization`).** The originally-deferred pyannote
**segmentation → embedding → global clustering** pipeline shipped: it sees the whole
meeting at once, so it finds true speaker turns (not ASR endpoints) and clusters each
voice into one speaker across the meeting — fixing the live per-segment greedy
clustering's two structural failures (turn-changes mid-segment, and a poisoned
first-come centroid). The high-accuracy offline ASR still transcribes natural
silence-split utterances; each is attributed to the diarization turn it overlaps most,
and each anonymous cluster is **named** by matching its representative audio against the
seeds (the §6 identification step, reused via `SpeakerNamer`). This needs a third model
(the ~6 MB pyannote `model.onnx`), now part of standard ASR setup; when it is absent the
pass **falls back** to the original greedy `Diarizer`-per-segment behaviour, so
diarization degrades in quality but never in function. The **live** transcript still
uses the streaming `Diarizer`, now sharpened with silence-trimmed embeddings,
L2-normalized centroids, and turn-stickiness.

### A3. **Voice clips** — recognise a person by ear (extends §6, narrow §9 exception)

A **Voiceprint** is a vector you can't listen to. We now also keep a short **voice
clip** (a few seconds of the person's clearest speech) at `People/.voices/<Name>.wav`,
pointed to from the `person` Entity, so you can *hear* who "Unknown speaker 2" was.
Clips are written only for **named** speakers — when you name one live, or for each
named speaker in the second pass. This is a **bounded, named exception** to §9's
transcribe-and-delete: the *full* meeting audio is still buffered only in memory and
discarded right after the second pass; only these short per-person clips persist.
(Limitation: a speaker named *after* stop, when the audio is already gone, gets no
clip.)

### A4. Names heard aloud are **suggested** (extends §6)

When a speaker says their own name ("I'm Sarah", "this is John"), a dependency-free
on-device heuristic offers it as a one-tap rename for the current unnamed speaker —
**suggested, never asserted**, consistent with §6 (a false positive is one dismissal).
Accepting runs the same rename path (relabel + enrol Voiceprint + voice clip). The
recording bar also tracks the **current speaker** so the *active* speaker can be named
the moment they change, not only from the static attendee avatars.

### A5. Discoverability — make the work *visible* (design psychology)

A feature the user can't see or trust isn't shipped. Three rules applied:

- **Narrate the background pass (visibility of system status; peak-end rule).** The
  second pass runs after Stop, so without feedback the user's last impression is the
  *rough* live transcript — and they'd conclude "still inaccurate." A bottom-right
  receipt now narrates "Wrapping up — sharpening the transcript…" → "Transcript
  sharpened ✓", then hands off to the distillation summary. The accuracy win is the
  last thing seen.
- **No false affordances (Norman's signifiers).** The ▶ "hear voice" control renders
  only for speakers who *have* a clip on disk (`meeting_voice_clips`), never as a
  button that errors — a speaker named after the audio is gone simply has no ▶.
- **One alive element; explain auto-guesses.** The bar keeps a single motion cue (the
  record pulse); the current-speaker chip is static. The heard-name suggestion states
  *why* it appeared ("Heard a name — call them Sarah?") rather than asserting, and it
  supersedes the generic "Name speaker" chip so the two never stack.

---

## Appendix A: Pocket-style capture — feature mapping

The user pointed at **Pocket** (heypocket.com) — a card-sized always-on wearable
that captures speech, then summarises, extracts action items, draws "mind maps"
that connect ideas, auto-detects speaker names (Pro), and answers questions over
your captures via **"Ask Pocket"**. Tellingly, Pocket's headline power-user feature
is an **MCP server that lets Claude Code query every capture** — and it offers
**Claude Opus** as a summarisation model. Pocket is **cloud-based** (button →
record → *cloud upload* → transcribe → summarise); its privacy story is "enterprise
encryption," not on-device.

The fit is striking: **Sediment is already the local-first, Claude-driven, graph-backed
version of what Pocket bolts onto a cloud.** Pocket exposes an MCP server *so that
Claude Code can reach its notes*; Sediment **is** a Claude-Code/Copilot agent with a
graph MCP server over a persistent Entity/Fact store. Most of Pocket's value is
already core Sediment; the rest is small.

| Pocket feature | Sediment mapping | Status |
|---|---|---|
| Always-on / instant capture (hardware button) | **Quick-capture** — a global hotkey / menu-bar voice memo that opens a *lightweight Session* (a Session with no meeting framing). Honours §3: bounded, user-initiated, **not** an always-on wearable mic. | **New** (Appendix A §1) |
| *…and with typing, not just voice* (user's ask) | Quick-capture has a **typed** twin: a global-hotkey quick-note that drops a line into the conversation / Daily note `## Did` (ADR-0010) and runs as a `chat_turn`. Same friction-free capture, text path. | **New** (Appendix A §1) |
| Auto summaries, multiple **styles** | The end-of-Session distillation turn (§7) already summarises; add a small **summary-style** setting (brief / decisions-only / narrative) as prompt presets. | **Extends §7** |
| Action items pulled out | `record_task` / open loops (ADR-0007) in distillation (§7). | **Covered** |
| **Mind maps** that "connect ideas" | Sediment's **knowledge graph** (Entities + bi-temporal Facts) is a *persistent, cross-meeting* superset of Pocket's per-recording mind map. A read-only **graph view** of an entity's neighbourhood would surface it visually. | **Covered** (graph view = new UI, not new model) |
| Speaker-name auto-detection | **Voiceprints** (§6). | **Covered** |
| **"Ask Pocket"** (chat over captures) | The conversational **Agent** (ADR-0009) over the whole formation — Sediment's primary surface, not a bolt-on. | **Covered** (core) |
| Projects, tags, pinning | `project` entity type already exists; tags via Obsidian frontmatter; "pin" = a Working-Set / favourites affordance. | **Mostly covered** |
| MCP server for Claude Code to query captures | Inverted: Sediment's Agent already runs *on* Claude Code with a graph MCP server. (Optional later: *consume* Pocket's MCP as an **ingestion source** for users who own the device — captures flow in as Meeting notes.) | **Core** (consume = optional) |
| Model-agnostic, incl. Claude Opus | Runs under the user's own Claude Code / Copilot subscription (ADR-0008/0012) — their model, their bill. | **Covered** |
| 120+ languages | Bounded by the chosen STT model (open Q5); Whisper/Parakeet are strongly multilingual. | **Model-dependent** |
| Cloud upload of all audio | **Rejected as default** — the core divergence. Sediment stays on-device + transcribe-and-delete (§2, §9); cloud STT is an explicit opt-in only (§2). This is the *differentiator*, not a gap. | **Deliberately differ** |

**The takeaway:** Pocket validates the demand and the workflow (capture → summarise →
action items → connect → ask), but its architecture is the cloud inverse of Sediment's.
Adopting its *workflow* costs almost nothing here because the graph, the Agent, the
distillation turn, and Voiceprints already cover it. The only genuinely new surface
Pocket inspires is **§1 quick-capture** — and it directly answers the user's "with
typing, not just voice," since quick-capture ships a voice path *and* a typed path
into the same conversation.

### A§1. Quick-capture — friction-free voice *or* typed capture into the conversation

A **global hotkey** (and menu-bar item) opens a tiny capture popover from anywhere,
without raising the full app — Pocket's "capture the instant it happens," for a
desktop:

- **Voice mode** starts a *lightweight Session* (capture + transcribe + diarise, §1–§6)
  with no meeting framing; on close it lands as a short note (a `Captures/<date>.md`
  entry or appended to the Daily note `## Notes`, ADR-0010) and runs a distillation
  turn (§7) at memo scale.
- **Typed mode** is a one-field quick-note that posts a `chat_turn` (ADR-0009) or
  appends to the Daily note — the same friction-free capture, text path. This is the
  explicit answer to "also with typing, not just voice": one capture surface, two
  input modes, **one conversation** behind both.

Quick-capture **reuses** the Session lifecycle (§3), the transcription/diarization
stack (§2–§6), the distillation turn (§7), and `chat_turn` — it is **UI + a hotkey**,
not a new subsystem, and it stays inside the no-daemon / no-second-surface line
because it is momentary, user-invoked, and collapses into the conversation + a Note.
