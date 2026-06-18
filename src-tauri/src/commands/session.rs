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
use crate::core::memory::MemoryHandle;
use crate::core::meeting_note;
use crate::core::session::{record_note, record_segment, MeetingSession, SessionEvent, SessionLifecycle, SessionRegistry};
use crate::error::{AppError, AppResult};
use serde::Serialize;
use tauri::ipc::Channel;
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

    // M2: spawn the real capture→transcription pipeline. It feeds segments through
    // the same `record_segment` path as the manual source. Feature-gated so the
    // default build pulls no audio backend and keeps M1's manual-source behaviour.
    #[cfg(feature = "audio")]
    {
        session.capture = Some(spawn_capture(&formation_root, &note_path, on_event.clone()));
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

/// Wire a [`MicSource`] → [`MockTranscriber`] pipeline whose segments record into
/// the Meeting note. M3 swaps the mock for the on-device ASR engine; M4 adds
/// diarization so the placeholder speaker becomes a real attribution.
#[cfg(feature = "audio")]
fn spawn_capture(
    formation_root: &std::path::Path,
    note_rel: &str,
    events: Channel<SessionEvent>,
) -> crate::core::capture_pipeline::CaptureController {
    use crate::core::capture::MicSource;
    use crate::core::transcription::MockTranscriber;

    let root = formation_root.to_path_buf();
    let note = note_rel.to_string();
    crate::core::capture_pipeline::spawn(
        MicSource,
        Box::new(MockTranscriber::default()),
        "Unknown speaker 1".to_string(),
        move |offset_ms, speaker, text| {
            if let Err(e) = record_segment(&root, &note, &events, offset_ms, speaker, text) {
                tracing::warn!("capture pipeline: record_segment failed: {e}");
            }
        },
    )
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

    record_segment(&formation_root, &note_rel, &events, offset_ms, &speaker, &text)
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
/// then streams the refreshed attendee list. The Voiceprint enrolment that would
/// auto-recognise the speaker next time is the ONNX-gated half (M4 runtime).
#[tauri::command]
pub async fn session_rename_speaker(
    session_id: String,
    from: String,
    to: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, events) = sessions
        .with_session(&session_id, |s| (s.note_path.clone(), s.events.clone()))
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    let note_abs = formation_root.join(&note_rel);
    meeting_note::rename_speaker(&note_abs, &from, &to)?;
    let attendees = meeting_note::list_attendees(&note_abs)?;
    let _ = events.send(SessionEvent::AttendeeChanged { attendees });
    Ok(())
}

/// Close a Session: deregister it (dropping its `CaptureController` tears down
/// capture), emit `Status{stopped}`, and return a summary derived from the note.
/// The end-of-Session distillation turn (ADR-0017 §7) is M6 — M1/M2 just close.
#[tauri::command]
pub async fn session_stop(
    session_id: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
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

    // Derive the summary from the note — the single source of truth.
    let note_abs = formation.require()?.join(&session.note_path);
    let segment_count = meeting_note::count_transcript_segments(&note_abs).unwrap_or(0);
    let attendees = meeting_note::list_attendees(&note_abs).unwrap_or_default();

    tracing::info!(session_id = %session_id, segments = segment_count, "session stopped");
    Ok(SessionStopResult {
        note_path: session.note_path,
        segment_count,
        attendees,
    })
}
