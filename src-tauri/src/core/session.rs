//! Meeting **Session** state (ADR-0017 §3, plan M1).
//!
//! A **Session** is one user-initiated, bounded stretch of live capture — the
//! opposite of a background daemon (it is started and stopped explicitly, exists
//! only while it runs, and collapses into the conversation + a Meeting note when
//! it ends). This module owns the in-memory state of an *open* Session and the
//! registry of currently-open ones; the durable artifact is the Meeting note
//! (`core::meeting_note`), not anything here.
//!
//! M1 has no audio: segments are pushed in by a fake source to validate the
//! spine (UI → registry → Meeting note → stream back). M2+ swaps the source for
//! real capture + transcription; the types below do not change.

use crate::core::capture_pipeline::CaptureController;
use crate::core::meeting_note;
use crate::error::AppResult;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;
use tauri::ipc::Channel;

/// One speaker-attributed, timestamped span of transcribed speech (ADR-0017 §6,
/// §8). `offset_ms` is measured from Session start — the spine that time-aligns
/// the transcript to the notes taken beside it.
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptSegment {
    pub offset_ms: i64,
    /// The attributed speaker name (e.g. a person's canonical name, "Self", or
    /// "Unknown speaker 2"). In M1 the fake source supplies this directly; in M4
    /// it comes from diarization + Voiceprint matching.
    pub speaker: String,
    pub text: String,
}

/// Streamed to the UI over a Tauri `Channel<SessionEvent>` while a Session is
/// open, mirroring the `chat_turn` streaming pattern. `kind` is the tag the
/// frontend switches on.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionEvent {
    /// The Session opened; carries the Meeting note's formation-relative path.
    Status {
        session_id: String,
        note_path: String,
        state: SessionLifecycle,
    },
    /// A transcript segment was appended to the Meeting note.
    Segment { segment: TranscriptSegment },
    /// A new attendee was added to `## Attendees`.
    AttendeeChanged { attendees: Vec<String> },
    /// A time-anchored note/chat line was appended to `## Notes`.
    Note { offset_ms: i64, text: String },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionLifecycle {
    Started,
    Stopped,
}

/// The in-memory state of one open Session. Holds the streaming channel so the
/// `session_push_*` commands can emit events without re-plumbing it each call.
///
/// Not `Debug`/`Clone`: it owns a `Channel` and a wall clock. The registry hands
/// out copies of the *data* (id, note path, attendees, offset) via accessors.
pub struct MeetingSession {
    pub id: String,
    /// Read by the M6 distillation turn (the meeting's title for its prompt);
    /// stored now so the Session carries it for its whole lifetime.
    #[allow(dead_code)]
    pub title: String,
    pub note_path: String,
    /// Wall clock anchor; `offset_ms()` is elapsed-since-start.
    started: Instant,
    pub events: Channel<SessionEvent>,
    /// The running capture→transcription pipeline, when capture is active (the
    /// `audio` feature, ADR-0017 §1). Dropping it on `session_stop` tears capture
    /// down deterministically (§3). `None` in M1 / default builds, where segments
    /// come from the manual `session_push_segment` source.
    pub capture: Option<CaptureController>,
}

impl MeetingSession {
    pub fn new(id: String, title: String, note_path: String, events: Channel<SessionEvent>) -> Self {
        Self {
            id,
            title,
            note_path,
            started: Instant::now(),
            events,
            capture: None,
        }
    }

    /// Milliseconds since the Session started — the audio offset (ADR-0017 §8).
    pub fn offset_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Shared recording — the single path both the manual command source
// (`session_push_segment`) and the capture pipeline use to land a segment in the
// Meeting note and stream the event. Attendee state is derived from the note, not
// tracked in memory, so the two sources stay consistent.
// ──────────────────────────────────────────────────────────────────────────

/// Append a transcript segment to the Meeting note and stream `Segment` (plus
/// `AttendeeChanged` when the speaker is new). `offset_ms` is from Session start.
pub fn record_segment(
    formation_root: &Path,
    note_rel: &str,
    events: &Channel<SessionEvent>,
    offset_ms: i64,
    speaker: &str,
    text: &str,
) -> AppResult<()> {
    let note_abs = formation_root.join(note_rel);
    let is_new_attendee = !meeting_note::attendee_present(&note_abs, speaker)?;
    if is_new_attendee {
        meeting_note::ensure_attendee(&note_abs, speaker)?;
    }
    meeting_note::append_transcript_segment(&note_abs, offset_ms, speaker, text)?;

    let _ = events.send(SessionEvent::Segment {
        segment: TranscriptSegment {
            offset_ms,
            speaker: speaker.to_string(),
            text: text.to_string(),
        },
    });
    if is_new_attendee {
        let attendees = meeting_note::list_attendees(&note_abs)?;
        let _ = events.send(SessionEvent::AttendeeChanged { attendees });
    }
    Ok(())
}

/// Append a time-anchored note/chat line to `## Notes` and stream `Note`.
pub fn record_note(
    formation_root: &Path,
    note_rel: &str,
    events: &Channel<SessionEvent>,
    offset_ms: i64,
    text: &str,
) -> AppResult<()> {
    let note_abs = formation_root.join(note_rel);
    meeting_note::append_note_line(&note_abs, offset_ms, text)?;
    let _ = events.send(SessionEvent::Note {
        offset_ms,
        text: text.to_string(),
    });
    Ok(())
}

/// Registry of currently-open Sessions, keyed by session id. Managed by Tauri as
/// app state. Bounded by user action — entries appear on `session_start` and are
/// removed on `session_stop` (ADR-0017 §3).
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<String, MeetingSession>>,
}

impl SessionRegistry {
    pub fn insert(&self, session: MeetingSession) {
        self.inner
            .lock()
            .expect("session registry poisoned")
            .insert(session.id.clone(), session);
    }

    pub fn remove(&self, id: &str) -> Option<MeetingSession> {
        self.inner
            .lock()
            .expect("session registry poisoned")
            .remove(id)
    }

    /// Run `f` against the open Session `id`, if present. Returns `None` when no
    /// such Session is open (e.g. a push after stop, or an unknown id).
    pub fn with_session<R>(
        &self,
        id: &str,
        f: impl FnOnce(&mut MeetingSession) -> R,
    ) -> Option<R> {
        let mut guard = self.inner.lock().expect("session registry poisoned");
        guard.get_mut(id).map(f)
    }

    /// Whether a Session is open — used by M5 to gate live in-meeting chat
    /// grounding (push the rolling transcript only while recording).
    #[allow(dead_code)]
    pub fn is_open(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("session registry poisoned")
            .contains_key(id)
    }
}
