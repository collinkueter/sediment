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
fn spawn_capture(
    formation_root: &std::path::Path,
    note_rel: &str,
    events: Channel<SessionEvent>,
    known_voiceprints: Vec<(String, Vec<f32>)>,
    centroids: crate::core::session::SharedCentroids,
) -> AppResult<crate::core::capture_pipeline::CaptureController> {
    use crate::core::capture::MicSource;
    use crate::core::transcription::Transcriber;

    // Resolves a finalized segment's audio to a speaker label (the diarization seam).
    type SpeakerResolver = Box<dyn FnMut(&[f32]) -> Option<String> + Send>;

    let root = formation_root.to_path_buf();
    let note = note_rel.to_string();
    let on_segment = move |offset_ms: i64, speaker: &str, text: &str| {
        if let Err(e) = record_segment(&root, &note, &events, offset_ms, speaker, text) {
            tracing::warn!("capture pipeline: record_segment failed: {e}");
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
            match crate::core::diarization::Diarizer::new(&model, known_voiceprints, centroids) {
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
        let _ = (&known_voiceprints, &centroids);
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

    // Persist the named speaker's Voiceprint from the diarizer's running centroid
    // for the old label, then re-key the centroid under the new name so further
    // segments keep matching it (ADR-0017 §6 progressive enrolment).
    #[cfg(feature = "local-asr")]
    {
        let centroid = sessions
            .with_session(&session_id, |s| {
                s.centroids.lock().ok().and_then(|m| m.get(&from).cloned())
            })
            .flatten();
        if let Some(centroid) = centroid {
            let memory_dir = formation_root.join(APP_DIR).join("memory");
            match memory.get_or_init(&memory_dir).await {
                Ok(store) => {
                    if let Err(e) = store.enroll_voiceprint_named(&to, &centroid).await {
                        tracing::warn!("rename: enroll voiceprint failed: {e}");
                    }
                }
                Err(e) => tracing::warn!("rename: memory init failed: {e}"),
            }
            sessions.with_session(&session_id, |s| {
                if let Ok(mut m) = s.centroids.lock() {
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
    // `session` drops at end of scope → its CaptureController stops the pipeline.

    let _ = session.events.send(SessionEvent::Status {
        session_id: session_id.clone(),
        note_path: session.note_path.clone(),
        state: SessionLifecycle::Stopped,
    });

    // Keep this meeting "in play" briefly so chat turns right after it are still
    // grounded on it (ADR-0017 §7 — recognise we're talking about the meeting,
    // during *and* after).
    sessions.mark_meeting_ended(session.note_path.clone());

    // Derive the summary from the note — the single source of truth.
    let formation_root = formation.require()?;
    let note_abs = formation_root.join(&session.note_path);
    let segment_count = meeting_note::count_transcript_segments(&note_abs).unwrap_or(0);
    let attendees = meeting_note::list_attendees(&note_abs).unwrap_or_default();

    // Distillation (ADR-0017 §7) runs in the background so Stop returns at once. It
    // resolves its own state from the AppHandle, so nothing borrowed from this
    // command crosses into the task. Skipped when nothing was transcribed.
    if segment_count > 0 {
        let events = session.events.clone();
        let note_rel = session.note_path.clone();
        let title = session.title.clone();
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
            match crate::core::distillation::distill_meeting(
                &formation_root,
                &note_rel,
                &title,
                &attendees_for_distill,
                &conversation_id,
                store,
                &cfg,
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
                Err(e) => tracing::warn!("distillation turn failed: {e}"),
            }
        });
    }

    tracing::info!(session_id = %session_id, segments = segment_count, "session stopped");
    Ok(SessionStopResult {
        note_path: session.note_path,
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
) -> AppResult<RenameMeetingResult> {
    let formation_root = formation.require()?;
    let old_title = meeting_note::title_from_path(&note_path);
    let new_rel = meeting_note::rename_meeting_note(&formation_root, &note_path, &new_title)?;

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
