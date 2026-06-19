//! Meeting **Session** commands (ADR-0017 §3–§5, plan M1/M2).
//!
//! Start/stop are explicit and user-initiated — a Session is bounded, never a
//! daemon. While open, segments and notes land in the Meeting note through the
//! single `core::session::record_*` path and stream `SessionEvent`s back over the
//! `Channel` the start call registered (mirroring `chat_turn`).
//!
//! Two segment sources share that path:
//!   - **manual** (`session_push_segment`) — the M1 fake source, always available;
//!   - **capture pipeline** (M2, `audio` feature) — real mic capture →
//!     transcription, spawned on `session_start`, torn down when the Session's
//!     `CaptureController` drops on stop.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::meeting_note;
use crate::core::memory::MemoryHandle;
use crate::core::session::{
    record_note, record_segment, MeetingSession, SessionEvent, SessionLifecycle, SessionRegistry,
};
use crate::error::{AppError, AppResult};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::Manager;
use tauri::State;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResult {
    pub session_id: String,
    /// Formation-relative path of the Meeting note (for the note viewer).
    pub note_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStopResult {
    pub note_path: String,
    pub segment_count: usize,
    pub attendees: Vec<String>,
}

/// Open a Session: create its Meeting note, reserve the `meeting` Entity, register
/// the open Session with its streaming channel, (under the `audio` feature) spawn
/// the capture pipeline, and emit a `Status{started}`.
#[tauri::command]
pub async fn session_start(
    title: String,
    on_event: Channel<SessionEvent>,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<SessionStartResult> {
    let formation_root = formation.require()?;
    let started = chrono::Local::now();
    let title = meeting_note::sanitize_title(&title);
    let note_path = meeting_note::meeting_note_relative_path(started, &title);

    meeting_note::ensure_meeting_note(&formation_root, &note_path, &title, started)?;

    // Reserve the `meeting` Entity so the note has a graph node from the start
    // (ADR-0017 §5). Best-effort: a Session must still open if the store can't
    // init. note_path↔entity linking is the indexer's job, as for every Note.
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    match memory.get_or_init(&memory_dir).await {
        Ok(store) => {
            if let Err(e) = store.upsert_entity(&title, "meeting", vec![]).await {
                tracing::warn!("session_start: reserve meeting entity failed: {e}");
            }
        }
        Err(e) => tracing::warn!("session_start: memory init failed (continuing): {e}"),
    }

    let session_id = format!("session:{}", uuid::Uuid::new_v4());
    // `mut` is only needed when the `audio` feature wires up the capture pipeline.
    #[cfg_attr(not(feature = "audio"), allow(unused_mut))]
    let mut session = MeetingSession::new(
        session_id.clone(),
        title,
        note_path.clone(),
        on_event.clone(),
    );

    // Spawn the real capture→transcription→diarization pipeline. It feeds segments
    // through the same `record_segment` path as the manual source. Gated on `audio`
    // so a `--no-default-features` build pulls no audio backend.
    #[cfg(feature = "audio")]
    {
        // Seed the diarizer with the formation's enrolled Voiceprints so a known
        // voice is named on its first segment (ADR-0017 §6).
        #[cfg(feature = "local-asr")]
        let known_voiceprints: Vec<(String, Vec<f32>)> = match memory.get_or_init(&memory_dir).await
        {
            Ok(store) => store.all_voiceprints().await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        #[cfg(not(feature = "local-asr"))]
        let known_voiceprints: Vec<(String, Vec<f32>)> = Vec::new();

        match spawn_capture(
            &formation_root,
            &note_path,
            on_event.clone(),
            known_voiceprints,
            session.centroids.clone(),
            session.relabels.clone(),
            session.audio.clone(),
            session.clips.clone(),
        ) {
            Ok(controller) => session.capture = Some(controller),
            Err(e) => {
                // The Session still opens (the note + manual source work); capture
                // just isn't running — e.g. the ASR model isn't installed yet.
                tracing::error!("session_start: capture failed to start: {e}");
            }
        }
    }

    let _ = on_event.send(SessionEvent::Status {
        session_id: session_id.clone(),
        note_path: note_path.clone(),
        state: SessionLifecycle::Started,
    });
    sessions.insert(session);

    tracing::info!(session_id = %session_id, note = %note_path, "session started");
    Ok(SessionStartResult {
        session_id,
        note_path,
    })
}

/// Wire the live capture pipeline whose segments record into the Meeting note:
/// microphone → on-device streaming ASR → per-segment diarization → the shared
/// `record_segment` path. The transcriber is the real [`LocalTranscriber`] under
/// `local-asr` (else the mock cadence source); the speaker resolver is the real
/// [`Diarizer`] when the speaker model is installed (else a single placeholder).
/// Returns an error when the ASR model isn't installed, so the Session can open
/// without faking a transcript.
#[cfg(feature = "audio")]
#[allow(clippy::too_many_arguments)] // pure wiring: each arg is a distinct pipeline input
fn spawn_capture(
    formation_root: &std::path::Path,
    note_rel: &str,
    events: Channel<SessionEvent>,
    known_voiceprints: Vec<(String, Vec<f32>)>,
    centroids: crate::core::session::SharedCentroids,
    relabels: crate::core::session::SharedRelabels,
    audio: crate::core::session::SharedAudio,
    clips: crate::core::session::SharedClips,
) -> AppResult<crate::core::capture_pipeline::CaptureController> {
    use crate::core::capture::MicSource;
    use crate::core::transcription::Transcriber;

    // Resolves a finalized segment's audio to a speaker label (the diarization seam).
    type SpeakerResolver = Box<dyn FnMut(&[f32]) -> Option<String> + Send>;

    let root = formation_root.to_path_buf();
    let note = note_rel.to_string();
    // Each finalized segment lands in the Meeting note; we also scan its text for a
    // self-introduction ("I'm Sarah") and, when the speaker is still unnamed, offer
    // that name as a one-tap rename (ADR-0017 §6, suggest-not-assert).
    let events_seg = events.clone();
    let on_segment = move |offset_ms: i64, speaker: &str, text: &str| {
        if let Err(e) = record_segment(&root, &note, &events_seg, offset_ms, speaker, text) {
            tracing::warn!("capture pipeline: record_segment failed: {e}");
        }
        if crate::core::session::is_unknown_speaker(speaker) {
            if let Some(name) = crate::core::name_detect::detect_self_introduction(text) {
                let _ = events_seg.send(SessionEvent::SpeakerNameSuggested {
                    speaker: speaker.to_string(),
                    name,
                });
            }
        }
    };

    // Transcriber: real streaming ASR when compiled, else the mock cadence source.
    #[cfg(feature = "local-asr")]
    let transcriber: Box<dyn Transcriber> = {
        let paths = crate::core::asr_model::asr_paths()?;
        Box::new(crate::core::transcription::LocalTranscriber::new(&paths)?)
    };
    #[cfg(not(feature = "local-asr"))]
    let transcriber: Box<dyn Transcriber> =
        Box::new(crate::core::transcription::MockTranscriber::default());

    // Speaker resolver: real diarization + identification when the speaker model is
    // present; otherwise every segment falls back to the placeholder speaker.
    #[cfg(feature = "local-asr")]
    let resolver: SpeakerResolver = match crate::core::asr_model::speaker_model_path() {
        Ok(model) => {
            match crate::core::diarization::Diarizer::new(
                &model,
                known_voiceprints,
                centroids,
                relabels,
            ) {
                Ok(mut diarizer) => Box::new(move |audio| Some(diarizer.assign(audio))),
                Err(e) => {
                    tracing::warn!("diarizer init failed ({e}); single speaker");
                    Box::new(|_audio| None)
                }
            }
        }
        Err(e) => {
            tracing::warn!("speaker model unavailable ({e}); single speaker");
            Box::new(|_audio| None)
        }
    };
    #[cfg(not(feature = "local-asr"))]
    let resolver: SpeakerResolver = {
        let _ = (&known_voiceprints, &centroids, &relabels);
        Box::new(|_audio| None)
    };

    // Capture source: mic mixed with system-output loopback when available, so the
    // meeting's far side is transcribed too (ADR-0017 §1); mic-only otherwise.
    #[cfg(all(feature = "loopback", target_os = "macos"))]
    let source = crate::core::capture::MixedSource {
        mic: Box::new(MicSource),
        loopback: Box::new(crate::core::capture::ScreenCaptureSource),
    };
    #[cfg(all(feature = "loopback", target_os = "windows"))]
    let source = crate::core::capture::MixedSource {
        mic: Box::new(MicSource),
        loopback: Box::new(crate::core::capture::WasapiLoopbackSource),
    };
    #[cfg(all(
        feature = "loopback",
        not(any(target_os = "macos", target_os = "windows"))
    ))]
    let source = MicSource; // loopback feature on, but no backend for this OS
    #[cfg(not(feature = "loopback"))]
    let source = MicSource;

    Ok(crate::core::capture_pipeline::spawn(
        source,
        transcriber,
        "Unknown speaker 1".to_string(),
        audio,
        clips,
        resolver,
        on_segment,
    ))
}

/// Push one transcript segment (M1 manual source). Appends to `## Transcript`,
/// adds the speaker to `## Attendees` if new, and streams the events.
#[tauri::command]
pub async fn session_push_segment(
    session_id: String,
    speaker: String,
    text: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, offset_ms, events) = sessions
        .with_session(&session_id, |s| {
            (s.note_path.clone(), s.offset_ms(), s.events.clone())
        })
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    record_segment(
        &formation_root,
        &note_rel,
        &events,
        offset_ms,
        &speaker,
        &text,
    )
}

/// Push a time-anchored note/chat line into `## Notes` (the user typing alongside
/// the meeting, ADR-0017 §8).
#[tauri::command]
pub async fn session_push_note(
    session_id: String,
    text: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, offset_ms, events) = sessions
        .with_session(&session_id, |s| {
            (s.note_path.clone(), s.offset_ms(), s.events.clone())
        })
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    record_note(&formation_root, &note_rel, &events, offset_ms, &text)
}

/// Write `samples` as `name`'s voice clip WAV under the formation (`People/.voices`),
/// returning its formation-relative path, or `None` on failure. Best-effort — a clip
/// is a nicety, never blocks naming.
#[cfg(feature = "local-asr")]
fn write_voice_clip(
    formation_root: &std::path::Path,
    name: &str,
    samples: &[f32],
) -> Option<String> {
    if samples.is_empty() {
        return None;
    }
    let rel = crate::core::people_note::voice_clip_relative_path(name);
    let abs = formation_root.join(&rel);
    if let Some(parent) = abs.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("voice clip: create dir failed: {e}");
            return None;
        }
    }
    match crate::core::audio::write_wav_i16(&abs, samples, crate::core::audio::TARGET_RATE) {
        Ok(()) => Some(rel),
        Err(e) => {
            tracing::warn!("voice clip: write failed: {e}");
            None
        }
    }
}

/// The second pass's output: refined `(offset_ms, speaker, text)` segments and the
/// longest audio clip captured per *named* speaker (for voice-clip persistence).
#[cfg(feature = "local-asr")]
type SecondPassResult = (
    Vec<(i64, String, String)>,
    std::collections::HashMap<String, Vec<f32>>,
);

/// The CPU-bound half of the end-of-Session **second pass** (ADR-0017 §2 two-pass),
/// run on a blocking thread: re-transcribe the whole meeting with the offline
/// high-accuracy engine and re-diarize each segment, seeding the diarizer with `seed`
/// (the formation's enrolled Voiceprints + this meeting's *named* live speakers) so
/// known/named voices keep their names. Returns the refined `(offset_ms, speaker,
/// text)` segments and the longest audio clip seen per *named* speaker (for voice-clip
/// persistence). An empty result (no offline model, or nothing decoded) leaves the
/// live transcript in place.
#[cfg(feature = "local-asr")]
fn run_second_pass(samples: &[f32], seed: Vec<(String, Vec<f32>)>) -> AppResult<SecondPassResult> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    let paths = crate::core::asr_model::offline_paths()?;
    let transcriber = crate::core::transcription::OfflineTranscriber::new(&paths)?;
    let speaker_model = crate::core::asr_model::speaker_model_path()?;
    let mut diarizer = crate::core::diarization::Diarizer::new(
        &speaker_model,
        seed,
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(Mutex::new(Vec::new())),
    )?;

    // ≈6 s cap per clip — enough to recognise a voice without storing the meeting.
    const MAX_CLIP: usize = crate::core::audio::TARGET_RATE as usize * 6;
    let rate = crate::core::audio::TARGET_RATE as i64;

    let mut segments: Vec<(i64, String, String)> = Vec::new();
    let mut clips: HashMap<String, Vec<f32>> = HashMap::new();
    let refined: Vec<crate::core::transcription::OfflineSegment> = transcriber.transcribe(samples);
    for seg in refined {
        // Slice the audio this segment covers and attribute it to a speaker.
        let start = ((seg.start_ms * rate) / 1000).max(0) as usize;
        let end = (((seg.end_ms * rate) / 1000) as usize).min(samples.len());
        let slice = if start < end { &samples[start..end] } else { &[] };
        let speaker = diarizer.assign(slice);

        // Keep the longest clip per named speaker (unknowns get no persisted clip).
        if !crate::core::session::is_unknown_speaker(&speaker) && !slice.is_empty() {
            let better = clips.get(&speaker).map(|c| slice.len() > c.len()).unwrap_or(true);
            if better {
                clips.insert(speaker.clone(), slice[..slice.len().min(MAX_CLIP)].to_vec());
            }
        }
        segments.push((seg.start_ms, speaker, seg.text));
    }
    Ok((segments, clips))
}

/// Name a speaker — the "that was Sarah" move (ADR-0017 §6). Rewrites the
/// `## Transcript` labels and `## Attendees` for an open Session's Meeting note,
/// streams the refreshed attendee list, and (under `local-asr`) **enrolls that
/// speaker's Voiceprint** from the live diarizer's centroid so future meetings
/// auto-recognise them — the durable half of progressive enrolment.
#[tauri::command]
pub async fn session_rename_speaker(
    session_id: String,
    from: String,
    to: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
    #[cfg_attr(not(feature = "local-asr"), allow(unused_variables))] memory: State<
        '_,
        MemoryHandle,
    >,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, events) = sessions
        .with_session(&session_id, |s| (s.note_path.clone(), s.events.clone()))
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    let note_abs = formation_root.join(&note_rel);
    meeting_note::rename_speaker(&note_abs, &from, &to)?;
    let attendees = meeting_note::list_attendees(&note_abs)?;
    let _ = events.send(SessionEvent::AttendeeChanged { attendees });

    // Tell the live diarizer about the rename so *ongoing* speech from this speaker
    // is labeled with the new name too (otherwise its cluster keeps the old label
    // and new segments revert to "Unknown speaker N"). ADR-0017 §6.
    sessions.with_session(&session_id, |s| {
        if let Ok(mut q) = s.relabels.lock() {
            q.push((from.clone(), to.clone()));
        }
    });

    // Persist the named speaker's Voiceprint from the diarizer's running centroid and
    // a short **voice clip** from their longest captured segment, then re-key both
    // under the new name so further segments keep matching (ADR-0017 §6 progressive
    // enrolment — the durable half of "that was Sarah").
    #[cfg(feature = "local-asr")]
    {
        let (centroid, clip) = sessions
            .with_session(&session_id, |s| {
                let centroid = s.centroids.lock().ok().and_then(|m| m.get(&from).cloned());
                let clip = s.clips.lock().ok().and_then(|m| m.get(&from).cloned());
                (centroid, clip)
            })
            .unwrap_or((None, None));

        let memory_dir = formation_root.join(APP_DIR).join("memory");
        let store = memory.get_or_init(&memory_dir).await;
        if let Err(e) = &store {
            tracing::warn!("rename: memory init failed: {e}");
        }

        if let Some(centroid) = centroid {
            if let Ok(store) = &store {
                if let Err(e) = store.enroll_voiceprint_named(&to, &centroid).await {
                    tracing::warn!("rename: enroll voiceprint failed: {e}");
                }
            }
            sessions.with_session(&session_id, |s| {
                if let Ok(mut m) = s.centroids.lock() {
                    if let Some(c) = m.remove(&from) {
                        m.insert(to.clone(), c);
                    }
                }
            });
        }

        if let (Ok(store), Some(clip)) = (&store, clip) {
            if let Some(rel) = write_voice_clip(&formation_root, &to, &clip) {
                if let Err(e) = store.set_voice_clip_named(&to, &rel).await {
                    tracing::warn!("rename: set voice clip failed: {e}");
                }
            }
            sessions.with_session(&session_id, |s| {
                if let Ok(mut m) = s.clips.lock() {
                    if let Some(c) = m.remove(&from) {
                        m.insert(to.clone(), c);
                    }
                }
            });
        }
    }
    Ok(())
}

/// Close a Session: deregister it (dropping its `CaptureController` tears down
/// capture), emit `Status{stopped}`, return a summary derived from the note, and
/// kick off the **end-of-Session distillation turn** (ADR-0017 §7) in the
/// background — the capture surface collapses immediately; the distillation streams
/// a `Distilled` event with a one-line receipt + undo handle when it finishes.
#[tauri::command]
pub async fn session_stop(
    session_id: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
    app: tauri::AppHandle,
) -> AppResult<SessionStopResult> {
    let session = sessions
        .remove(&session_id)
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    // Pull what we need off the Session, then drop it to stop capture *now* (its
    // CaptureController joins the worker on drop) — so the audio buffer we read below
    // is complete and nothing writes to it afterwards.
    let events = session.events.clone();
    let note_path = session.note_path.clone();
    let title = session.title.clone();
    #[cfg(feature = "local-asr")]
    let audio_arc = session.audio.clone();
    #[cfg(feature = "local-asr")]
    let centroids_arc = session.centroids.clone();
    drop(session);

    // Take the captured audio (cleared here — the full audio is never persisted,
    // ADR-0017 §9) and the meeting's *named* live speakers, to seed re-diarization.
    #[cfg(feature = "local-asr")]
    let audio_samples: Vec<f32> = audio_arc
        .lock()
        .map(|mut b| std::mem::take(&mut *b))
        .unwrap_or_default();
    #[cfg(feature = "local-asr")]
    let live_named: Vec<(String, Vec<f32>)> = centroids_arc
        .lock()
        .map(|m| {
            m.iter()
                .filter(|(label, _)| !crate::core::session::is_unknown_speaker(label))
                .map(|(label, c)| (label.clone(), c.clone()))
                .collect()
        })
        .unwrap_or_default();

    let _ = events.send(SessionEvent::Status {
        session_id: session_id.clone(),
        note_path: note_path.clone(),
        state: SessionLifecycle::Stopped,
    });

    // Keep this meeting "in play" briefly so chat turns right after it are still
    // grounded on it (ADR-0017 §7 — during *and* after).
    sessions.mark_meeting_ended(note_path.clone());

    // Derive the summary from the note — the single source of truth.
    let formation_root = formation.require()?;
    let note_abs = formation_root.join(&note_path);
    let segment_count = meeting_note::count_transcript_segments(&note_abs).unwrap_or_else(|e| {
        tracing::warn!("session_stop: count segments failed: {e}");
        0
    });
    let attendees = meeting_note::list_attendees(&note_abs).unwrap_or_default();

    // Background finishing so Stop returns at once: the offline **second pass**
    // (ADR-0017 §2) rewrites the transcript with high-accuracy text + re-diarized
    // speakers, then the **distillation turn** (§7) reads that improved transcript.
    // Skipped when nothing was transcribed.
    if segment_count > 0 {
        let note_rel = note_path.clone();
        let conversation_id = session_id.clone();
        let attendees_for_distill = attendees.clone();
        let formation_root = formation_root.clone();
        tauri::async_runtime::spawn(async move {
            let memory = app.state::<MemoryHandle>();
            let memory_dir = formation_root.join(APP_DIR).join("memory");
            let store = match memory.get_or_init(&memory_dir).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("distillation: memory init failed: {e}");
                    return;
                }
            };
            let cfg = crate::core::formation_state::AppConfig::load(&app);
            let copilot = app.state::<crate::core::copilot::CopilotEngineHandle>();
            // Serialize the whole stop-time mutation (second-pass rewrite +
            // distillation) against concurrent chat turns so their snapshot→diff→audit
            // windows never overlap (ADR-0009 §6).
            let turn_lock = app.state::<crate::core::session::FormationLock>();
            let _guard = turn_lock.0.lock().await;

            // ── Second pass (ADR-0017 §2) — local-asr only ─────────────────────
            #[cfg(feature = "local-asr")]
            if crate::core::asr_model::offline_present() && !audio_samples.is_empty() {
                let mut seed = store.all_voiceprints().await.unwrap_or_default();
                seed.extend(live_named);
                match tokio::task::spawn_blocking(move || run_second_pass(&audio_samples, seed))
                    .await
                {
                    Ok(Ok((segments, clips))) if !segments.is_empty() => {
                        let note_abs = formation_root.join(&note_rel);
                        match meeting_note::replace_transcript(&note_abs, &segments) {
                            Ok(n) => {
                                // Persist a voice clip for each named speaker so they
                                // can be recognised by ear later (ADR-0017 §6).
                                for (name, clip) in clips {
                                    if let Some(rel) =
                                        write_voice_clip(&formation_root, &name, &clip)
                                    {
                                        if let Err(e) =
                                            store.set_voice_clip_named(&name, &rel).await
                                        {
                                            tracing::warn!("second pass: set voice clip: {e}");
                                        }
                                    }
                                }
                                let _ = events
                                    .send(SessionEvent::TranscriptRefined { segment_count: n });
                            }
                            Err(e) => tracing::warn!("second pass: replace transcript: {e}"),
                        }
                    }
                    Ok(Ok(_)) => tracing::info!("second pass: no segments decoded"),
                    Ok(Err(e)) => tracing::warn!("second pass failed: {e}"),
                    Err(e) => tracing::warn!("second pass join failed: {e}"),
                }
            }

            // ── Distillation (ADR-0017 §7) over the (refined) transcript ───────
            match crate::core::distillation::distill_meeting(
                &formation_root,
                &note_rel,
                &title,
                &attendees_for_distill,
                &conversation_id,
                store,
                &cfg,
                &copilot,
            )
            .await
            {
                Ok(Some(result)) => {
                    let _ = events.send(SessionEvent::Distilled {
                        summary: result.summary,
                        turn_id: result.turn_id,
                        suggested_title: result.suggested_title,
                    });
                }
                Ok(None) => tracing::info!("distillation: no transcript to distil"),
                Err(e) => {
                    tracing::warn!("distillation turn failed: {e}");
                    // Surface the failure instead of silently doing nothing — an
                    // empty turn_id tells the UI to show the note without an Undo.
                    let _ = events.send(SessionEvent::Distilled {
                        summary: "Couldn't summarize this meeting — the transcript is saved."
                            .to_string(),
                        turn_id: String::new(),
                        suggested_title: None,
                    });
                }
            }
        });
    }

    tracing::info!(session_id = %session_id, segments = segment_count, "session stopped");
    Ok(SessionStopResult {
        note_path,
        segment_count,
        attendees,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameMeetingResult {
    /// The note's new formation-relative path after the rename.
    pub note_path: String,
}

/// Rename a finished Meeting note from the end-of-session suggestion (ADR-0017 §7):
/// rewrite the note's H1 and move the file (keeping its timestamp prefix), then
/// best-effort rename the `meeting` graph entity so the node tracks the file. The
/// current title is read from the note's own filename — the single source of truth
/// — so the caller only passes the path and the new title. Returns the new path so
/// the UI can re-point its note link. Runs after Stop, when the Session is closed
/// and nothing holds the old path.
#[tauri::command]
pub async fn rename_meeting_note(
    note_path: String,
    new_title: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<RenameMeetingResult> {
    let formation_root = formation.require()?;
    let old_title = meeting_note::title_from_path(&note_path);
    let new_rel = meeting_note::rename_meeting_note(&formation_root, &note_path, &new_title)?;

    // If this meeting is still in the post-meeting grounding window, re-point the
    // recent slot at the new path so chat turns keep resolving its transcript.
    sessions.note_path_renamed(&note_path, &new_rel);

    // Keep the graph node's name in step with the file. Best-effort: the file
    // rename has already succeeded and must not be undone by a store hiccup.
    let new_clean = meeting_note::sanitize_title(&new_title);
    let memory_dir = formation_root.join(APP_DIR).join("memory");
    match memory.get_or_init(&memory_dir).await {
        Ok(store) => {
            if let Err(e) = store.rename_entity(&old_title, &new_clean, "meeting").await {
                tracing::warn!("rename_meeting_note: entity rename failed: {e}");
            }
        }
        Err(e) => tracing::warn!("rename_meeting_note: memory init failed: {e}"),
    }

    tracing::info!(from = %note_path, to = %new_rel, "meeting note renamed");
    Ok(RenameMeetingResult { note_path: new_rel })
}
