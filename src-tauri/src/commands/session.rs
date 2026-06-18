//! Meeting **Session** commands (ADR-0016 §3–§5, plan M1).
//!
//! Start/stop are explicit and user-initiated — a Session is bounded, never a
//! daemon. While open, `session_push_segment` / `session_push_note` splice into
//! the Meeting note and stream `SessionEvent`s back over the `Channel` the start
//! call registered, mirroring `chat_turn`'s streaming shape.
//!
//! M1 has no audio. `session_push_segment` is the **fake source** that proves the
//! spine (UI → registry → Meeting note → stream) end-to-end; M2 replaces it with
//! real capture + transcription, and the command surface here does not change.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::memory::MemoryHandle;
use crate::core::meeting_note;
use crate::core::session::{
    MeetingSession, SessionEvent, SessionLifecycle, SessionRegistry, TranscriptSegment,
};
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
/// the open Session with its streaming channel, and emit a `Status{started}`.
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
    // (ADR-0016 §5). Best-effort: a Session must still open if the store can't
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
    let session = MeetingSession::new(
        session_id.clone(),
        title,
        note_path.clone(),
        on_event.clone(),
    );
    // Emit the opening status on the freshly-registered channel.
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

/// Push one transcript segment (M1: the fake source). Appends to `## Transcript`,
/// adds the speaker to `## Attendees` if new, and streams `Segment` (+
/// `AttendeeChanged` on a new attendee).
#[tauri::command]
pub async fn session_push_segment(
    session_id: String,
    speaker: String,
    text: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, offset_ms) = sessions
        .with_session(&session_id, |s| (s.note_path.clone(), s.offset_ms()))
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    let note_abs = formation_root.join(&note_rel);

    // File IO outside the registry lock.
    let is_new_attendee = !meeting_note::attendee_present(&note_abs, &speaker)?;
    if is_new_attendee {
        meeting_note::ensure_attendee(&note_abs, &speaker)?;
    }
    meeting_note::append_transcript_segment(&note_abs, offset_ms, &speaker, &text)?;

    // Update counters and emit under the lock (the session may have been stopped
    // concurrently — then this is a no-op, which is correct).
    sessions.with_session(&session_id, |s| {
        s.segment_count += 1;
        if is_new_attendee && !s.attendees.iter().any(|a| a == &speaker) {
            s.attendees.push(speaker.clone());
        }
        let _ = s.events.send(SessionEvent::Segment {
            segment: TranscriptSegment {
                offset_ms,
                speaker: speaker.clone(),
                text: text.clone(),
            },
        });
        if is_new_attendee {
            let _ = s.events.send(SessionEvent::AttendeeChanged {
                attendees: s.attendees.clone(),
            });
        }
    });
    Ok(())
}

/// Push a time-anchored note/chat line into `## Notes` (the user typing alongside
/// the meeting, ADR-0016 §8). Streams a `Note` event.
#[tauri::command]
pub async fn session_push_note(
    session_id: String,
    text: String,
    formation: State<'_, FormationState>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<()> {
    let formation_root = formation.require()?;
    let (note_rel, offset_ms) = sessions
        .with_session(&session_id, |s| (s.note_path.clone(), s.offset_ms()))
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    let note_abs = formation_root.join(&note_rel);
    meeting_note::append_note_line(&note_abs, offset_ms, &text)?;

    sessions.with_session(&session_id, |s| {
        let _ = s.events.send(SessionEvent::Note {
            offset_ms,
            text: text.clone(),
        });
    });
    Ok(())
}

/// Close a Session: deregister it, emit `Status{stopped}`, and return a summary.
/// The end-of-Session distillation turn (ADR-0016 §7) is M6 — M1 just closes.
#[tauri::command]
pub async fn session_stop(
    session_id: String,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<SessionStopResult> {
    let session = sessions
        .remove(&session_id)
        .ok_or_else(|| AppError::other(format!("no open session {session_id}")))?;

    let _ = session.events.send(SessionEvent::Status {
        session_id: session_id.clone(),
        note_path: session.note_path.clone(),
        state: SessionLifecycle::Stopped,
    });

    tracing::info!(
        session_id = %session_id,
        segments = session.segment_count,
        "session stopped"
    );
    Ok(SessionStopResult {
        note_path: session.note_path,
        segment_count: session.segment_count,
        attendees: session.attendees,
    })
}
