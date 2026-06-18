//! Meeting **Session** state (ADR-0016 §3, plan M1).
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

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tauri::ipc::Channel;

/// One speaker-attributed, timestamped span of transcribed speech (ADR-0016 §6,
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
    pub title: String,
    pub note_path: String,
    /// Wall clock anchor; `offset_ms()` is elapsed-since-start.
    started: Instant,
    pub attendees: Vec<String>,
    pub segment_count: usize,
    pub events: Channel<SessionEvent>,
}

impl MeetingSession {
    pub fn new(id: String, title: String, note_path: String, events: Channel<SessionEvent>) -> Self {
        Self {
            id,
            title,
            note_path,
            started: Instant::now(),
            attendees: Vec::new(),
            segment_count: 0,
            events,
        }
    }

    /// Milliseconds since the Session started — the audio offset (ADR-0016 §8).
    pub fn offset_ms(&self) -> i64 {
        self.started.elapsed().as_millis() as i64
    }
}

/// Registry of currently-open Sessions, keyed by session id. Managed by Tauri as
/// app state. Bounded by user action — entries appear on `session_start` and are
/// removed on `session_stop` (ADR-0016 §3).
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

    pub fn is_open(&self, id: &str) -> bool {
        self.inner
            .lock()
            .expect("session registry poisoned")
            .contains_key(id)
    }
}
