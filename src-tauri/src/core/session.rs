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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::Channel;

/// Per-session map of speaker label → running speaker-embedding centroid, written
/// by the live diarizer (`core::diarization`) and read by `session_rename_speaker`
/// to persist a named speaker's **Voiceprint** (progressive enrolment, ADR-0017
/// §6). Defined here (not in the `local-asr`-gated diarization module) so the
/// `MeetingSession` field type exists in every build.
pub type SharedCentroids = Arc<Mutex<HashMap<String, Vec<f32>>>>;

/// Pending live speaker relabels (`from` → `to`) the capture worker's diarizer
/// applies to its own clusters, so naming a speaker mid-meeting sticks to *new*
/// segments too (ADR-0017 §6). Drained by the `Diarizer` on each `assign`.
pub type SharedRelabels = Arc<Mutex<Vec<(String, String)>>>;

/// The whole Session's captured audio (16 kHz mono f32), accumulated by the capture
/// worker and read once at stop for the high-accuracy **second pass** (offline
/// re-transcription) and per-person voice-clip extraction, then **cleared** — audio
/// is held only long enough to use, never persisted (ADR-0017 §9). Defined here (not
/// in the `local-asr`-gated modules) so the `MeetingSession` field exists in every
/// build; only written/read under the `audio`/`local-asr` paths.
pub type SharedAudio = Arc<Mutex<Vec<f32>>>;

/// Per-speaker-label representative audio clip (16 kHz mono f32), the longest clean
/// segment seen for each live label. Read by `session_rename_speaker` to persist a
/// named person's **voice clip** beside their Voiceprint (ADR-0017 §6), so they can
/// be identified by ear later. Empty until diarization assigns the first segment.
pub type SharedClips = Arc<Mutex<HashMap<String, Vec<f32>>>>;

/// Serialises formation-mutating turns — a `chat_turn` and the background meeting
/// distillation — so their whole-formation snapshot→diff→audit windows never
/// overlap. Without it, one turn's diff can attribute a *concurrent* turn's note
/// edits to itself, so undoing one turn silently reverts another's work
/// (ADR-0009 §6 assumes serialized turns). Managed as Tauri app state.
#[derive(Default)]
pub struct FormationLock(pub tokio::sync::Mutex<()>);

/// Whether `speaker` is an unnamed diarization label (`Unknown speaker N`) rather
/// than a real person. Used to gate the live self-introduction suggestion (only
/// offer a name for a speaker we haven't named yet) — the backend twin of the
/// frontend's `isUnknown` (`src/lib/speakers.ts`).
// Used on the capture/second-pass paths (`audio`/`local-asr`); unused in a headless build.
#[allow(dead_code)]
pub fn is_unknown_speaker(speaker: &str) -> bool {
    speaker.trim_start().starts_with("Unknown speaker")
}

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
    /// The end-of-Session distillation turn finished (ADR-0017 §7): a one-line
    /// receipt and the audit `turn_id` for undo. Arrives after `Status{stopped}`,
    /// once the background distillation completes. `suggested_title` is a title
    /// the distillation derived from the meeting's content, offered as an optional
    /// rename when it differs from the one the user typed at Start (else `None`).
    Distilled {
        summary: String,
        turn_id: String,
        suggested_title: Option<String>,
    },
    /// A speaker said their own name (ADR-0017 §6, suggest-not-assert): the capture
    /// worker detected a self-introduction ("I'm Sarah") in `speaker`'s segment.
    /// The recording bar offers `name` as a one-tap rename — never auto-applied, so
    /// a false positive costs nothing. `speaker` is the current label (often an
    /// `Unknown speaker N`).
    // Sent only on the capture path (`audio`); unused in a headless build.
    #[allow(dead_code)]
    SpeakerNameSuggested { speaker: String, name: String },
    /// The end-of-Session **second pass** finished: the `## Transcript` was rewritten
    /// by the offline high-accuracy engine (ADR-0017 §2 two-pass). Tells an open note
    /// view to reload its now-improved transcript. Arrives after `Status{stopped}`,
    /// before `Distilled`.
    // Sent only on the `local-asr` second pass; unused in a headless build.
    #[allow(dead_code)]
    TranscriptRefined { segment_count: usize },
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
    /// `audio` feature, ADR-0017 §1). An RAII guard — never read directly; its
    /// `Drop` on `session_stop` tears capture down deterministically (§3). `None`
    /// in builds without the `audio` feature, where segments come from the manual
    /// `session_push_segment` source.
    #[allow(dead_code)]
    pub capture: Option<CaptureController>,
    /// Live diarizer's per-speaker centroids (ADR-0017 §6). Shared with the capture
    /// worker; `session_rename_speaker` reads it to enroll a named speaker's
    /// Voiceprint. Empty until diarization writes the first segment's centroid.
    /// Only read under `local-asr` (the diarizer/enrolment path).
    #[allow(dead_code)]
    pub centroids: SharedCentroids,
    /// Pending live speaker relabels the capture worker's diarizer applies to its
    /// own clusters, so a mid-meeting rename sticks to ongoing speech too (ADR-0017
    /// §6). Pushed by `session_rename_speaker`. Only read under `local-asr`.
    #[allow(dead_code)]
    pub relabels: SharedRelabels,
    /// The Session's captured audio, accumulated by the capture worker and consumed
    /// once at stop for the offline second pass + clip extraction, then cleared
    /// (ADR-0017 §2/§9). Empty in builds without the `audio` feature.
    #[allow(dead_code)]
    pub audio: SharedAudio,
    /// The longest clip seen per live speaker label, so naming a speaker persists a
    /// voice clip beside their Voiceprint (ADR-0017 §6). Only written under
    /// `local-asr` (the diarizer path).
    #[allow(dead_code)]
    pub clips: SharedClips,
}

impl MeetingSession {
    pub fn new(
        id: String,
        title: String,
        note_path: String,
        events: Channel<SessionEvent>,
    ) -> Self {
        Self {
            id,
            title,
            note_path,
            started: Instant::now(),
            events,
            capture: None,
            centroids: Arc::new(Mutex::new(HashMap::new())),
            relabels: Arc::new(Mutex::new(Vec::new())),
            audio: Arc::new(Mutex::new(Vec::new())),
            clips: Arc::new(Mutex::new(HashMap::new())),
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
    /// The most-recently-ended meeting, kept briefly after stop so chat turns just
    /// after a meeting are still grounded on it (ADR-0017 §7) — "recognise we're
    /// talking about the meeting" extends past the live session by
    /// [`RECENT_MEETING_WINDOW`]. `None` until a meeting ends.
    recent: Mutex<Option<RecentMeeting>>,
}

/// A just-ended meeting and when it stopped — the "after" half of in-meeting
/// grounding.
struct RecentMeeting {
    note_path: String,
    ended: Instant,
}

impl SessionRegistry {
    /// Lock the inner map, recovering from a poisoned mutex rather than panicking —
    /// a single panic while holding the lock must not brick every later session
    /// command (the `recent` slot below is already written defensively).
    fn inner(&self) -> std::sync::MutexGuard<'_, HashMap<String, MeetingSession>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn insert(&self, session: MeetingSession) {
        self.inner().insert(session.id.clone(), session);
    }

    pub fn remove(&self, id: &str) -> Option<MeetingSession> {
        self.inner().remove(id)
    }

    /// Run `f` against the open Session `id`, if present. Returns `None` when no
    /// such Session is open (e.g. a push after stop, or an unknown id).
    pub fn with_session<R>(&self, id: &str, f: impl FnOnce(&mut MeetingSession) -> R) -> Option<R> {
        self.inner().get_mut(id).map(f)
    }

    /// Whether a Session is open — used by M5 to gate live in-meeting chat
    /// grounding (push the rolling transcript only while recording).
    #[allow(dead_code)]
    pub fn is_open(&self, id: &str) -> bool {
        self.inner().contains_key(id)
    }

    /// Whether a Session is currently recording into `note_path`. Lets the
    /// post-meeting `assign_meeting_speaker` refuse to file-edit a note the live
    /// pipeline is still writing (it would race the diarizer). ADR-0017 §6.
    pub fn is_recording(&self, note_path: &str) -> bool {
        self.inner().values().any(|s| s.note_path == note_path)
    }

    /// Re-point the "recent meeting" grounding slot when a meeting note is renamed
    /// (its file moved), so post-meeting chat grounding keeps resolving (ADR-0017
    /// §7) instead of reading the old, now-missing path.
    pub fn note_path_renamed(&self, old: &str, new: &str) {
        if let Ok(mut recent) = self.recent.lock() {
            if let Some(r) = recent.as_mut() {
                if r.note_path == old {
                    r.note_path = new.to_string();
                }
            }
        }
    }

    /// Note that a meeting just ended, so its transcript keeps grounding chat turns
    /// for [`RECENT_MEETING_WINDOW`] after stop (ADR-0017 §7). Called from
    /// `session_stop`.
    pub fn mark_meeting_ended(&self, note_path: String) {
        if let Ok(mut recent) = self.recent.lock() {
            *recent = Some(RecentMeeting {
                note_path,
                ended: Instant::now(),
            });
        }
    }

    /// Grounding block for the meeting's most-recent transcript, so chat turns are
    /// "about the meeting" both *during* it and for a short while *after* (ADR-0017
    /// §7). Prefers an open Session (live); failing that, falls back to the most
    /// recently ended one within [`RECENT_MEETING_WINDOW`]. `None` when neither
    /// applies or there is no transcript yet. Capped at [`LIVE_TRANSCRIPT_BUDGET`]
    /// so it never crowds the Self (ADR-0015 §3) or Working Set (ADR-0011 §2) above
    /// it in the turn's grounding (ADR-0017 Q3).
    pub fn live_transcript_grounding(&self, formation_root: &Path) -> Option<String> {
        // Live: the most-recently-started open Session (the UI runs one at a time).
        let open = {
            let guard = self.inner.lock().ok()?;
            guard
                .values()
                .max_by_key(|s| s.started)
                .map(|s| s.note_path.clone())
        };
        // After: a meeting that ended within the window still counts as "in play".
        let note_rel = match open {
            Some(p) => p,
            None => {
                let recent = self.recent.lock().ok()?;
                match recent.as_ref() {
                    Some(r) if r.ended.elapsed() < RECENT_MEETING_WINDOW => r.note_path.clone(),
                    _ => return None,
                }
            }
        };
        let note_abs = formation_root.join(note_rel);
        meeting_note::recent_transcript_grounding(&note_abs, LIVE_TRANSCRIPT_BUDGET)
            .ok()
            .flatten()
    }
}

/// Cap on the live-transcript grounding slot (ADR-0017 Q3 — a felt-test starting
/// point of ~2 KB / roughly the last minute or two of speech).
const LIVE_TRANSCRIPT_BUDGET: usize = 2000;

/// How long after a meeting ends its transcript still grounds chat turns — long
/// enough that "what did Sarah say about Q3?" right after the call still resolves,
/// short enough that it stops bleeding into unrelated later conversation.
const RECENT_MEETING_WINDOW: Duration = Duration::from_secs(30 * 60);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::meeting_note;
    use chrono::TimeZone;

    // A meeting that ended within the window still grounds chat turns (ADR-0017 §7
    // "during *and* after"), even with no open Session.
    #[test]
    fn recent_meeting_grounds_chat_after_stop() {
        let root = std::env::temp_dir()
            .join("sediment-test-recent-grounding")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let started = chrono::Local
            .with_ymd_and_hms(2026, 6, 18, 15, 30, 0)
            .unwrap();
        let rel = meeting_note::meeting_note_relative_path(started, "Q3 Planning");
        let abs = meeting_note::ensure_meeting_note(&root, &rel, "Q3 Planning", started).unwrap();
        meeting_note::append_transcript_segment(&abs, 5_000, "Sarah Chen", "Let's start with Q3.")
            .unwrap();

        let registry = SessionRegistry::default();
        // No open Session and nothing ended yet → nothing to ground on.
        assert!(registry.live_transcript_grounding(&root).is_none());

        // After the meeting ends, its transcript grounds the next chat turns.
        registry.mark_meeting_ended(rel.clone());
        let grounding = registry
            .live_transcript_grounding(&root)
            .expect("recent meeting grounds chat");
        assert!(grounding.contains("Let's start with Q3."));

        std::fs::remove_dir_all(root).ok();
    }
}
