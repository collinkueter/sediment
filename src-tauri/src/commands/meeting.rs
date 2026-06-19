//! Post-meeting commands (ADR-0017 §6) — assigning speakers to people *after* the
//! Session has ended.
//!
//! Live, `session_rename_speaker` relabels an open Session's transcript and enrolls
//! the speaker's Voiceprint from the diarizer's running centroid. But most naming
//! happens when reviewing the Meeting note afterwards ("who was Unknown speaker 2?").
//! These commands operate on the *finalized note file* — no open Session required:
//! relabel its `## Transcript` + `## Attendees`, and ensure the named person has an
//! Entity and a `People/<Name>.md` file the `[[…]]` attendee link resolves to.

use crate::commands::formation::APP_DIR;
use crate::core::formation_state::FormationState;
use crate::core::memory::MemoryHandle;
use crate::core::session::SessionRegistry;
use crate::core::{meeting_note, people_note};
use crate::error::{AppError, AppResult};
use serde::Serialize;
use tauri::State;

/// The distinct speakers in a finalized Meeting note — its `## Attendees`, which
/// `record_segment` keeps in sync with everyone who spoke. Drives the post-meeting
/// "assign speakers" panel.
#[tauri::command]
pub async fn meeting_speakers(
    note_path: String,
    formation: State<'_, FormationState>,
) -> AppResult<Vec<String>> {
    let root = formation.require()?;
    meeting_note::list_attendees(&root.join(&note_path))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignSpeakerResult {
    /// The refreshed attendee list after the rename.
    pub attendees: Vec<String>,
    /// Formation-relative path of the person's People note (created if needed).
    pub person_note_path: String,
    /// How many transcript segments were relabelled.
    pub relabeled: usize,
}

/// Assign a Meeting-note speaker to a person after the meeting (ADR-0017 §6).
/// Relabels the `## Transcript` and `## Attendees`, ensures the person has an
/// Entity, and creates `People/<Name>.md` if it doesn't exist yet — so the person
/// has their own file and the `[[Name]]` link resolves. Idempotent on the note and
/// the file; a no-op rename (same name) still ensures the person + file.
#[tauri::command]
pub async fn assign_meeting_speaker(
    note_path: String,
    from: String,
    to: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
    sessions: State<'_, SessionRegistry>,
) -> AppResult<AssignSpeakerResult> {
    let root = formation.require()?;
    let to = to.trim();
    if to.is_empty() {
        return Err(AppError::other("assign_meeting_speaker: empty name"));
    }
    // This edits the note file directly; while the meeting is still recording the
    // live pipeline is also writing it. Refuse, so we don't race the diarizer —
    // name speakers live from the recording bar instead (which the UI routes to).
    if sessions.is_recording(&note_path) {
        return Err(AppError::other(
            "This meeting is still recording — name speakers from the recording bar.",
        ));
    }
    let note_abs = root.join(&note_path);
    let relabeled = meeting_note::rename_speaker(&note_abs, &from, to)?;

    // Give the person an Entity (best-effort — the graph is not the source of truth
    // for the rename) and a People note file the attendee link can resolve to.
    let memory_dir = root.join(APP_DIR).join("memory");
    if let Ok(store) = memory.get_or_init(&memory_dir).await {
        if let Err(e) = store.upsert_entity(to, "person", vec![]).await {
            tracing::warn!("assign_meeting_speaker: upsert entity failed: {e}");
        }
    }
    let person_note_path = people_note::ensure_person_note(&root, to)?;

    let attendees = meeting_note::list_attendees(&note_abs).unwrap_or_default();
    Ok(AssignSpeakerResult {
        attendees,
        person_note_path,
        relabeled,
    })
}

/// Which of a Meeting note's attendees actually have a playable **voice clip** on
/// disk (ADR-0017 §6) — so the speaker panel only shows a ▶ where there's a voice to
/// hear, never a play control that does nothing (a speaker named after the meeting,
/// when the audio is gone, has no clip). Returns the subset of attendee names.
#[tauri::command]
pub async fn meeting_voice_clips(
    note_path: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<Vec<String>> {
    let root = formation.require()?;
    let attendees = meeting_note::list_attendees(&root.join(&note_path)).unwrap_or_default();
    let memory_dir = root.join(APP_DIR).join("memory");
    let Ok(store) = memory.get_or_init(&memory_dir).await else {
        return Ok(Vec::new());
    };
    let mut with_clips = Vec::new();
    for name in attendees {
        if let Ok(Some(rel)) = store.voice_clip_path(&name).await {
            if root.join(&rel).is_file() {
                with_clips.push(name);
            }
        }
    }
    Ok(with_clips)
}

/// The bytes of a person's **voice clip** WAV (ADR-0017 §6), or `None` when they have
/// none. Lets the speaker panel play a short sample so you can recognise someone by
/// ear. Reads from the formation by the path recorded on the person Entity — no audio
/// model needed, so it's available in every build.
#[tauri::command]
pub async fn read_voice_clip(
    name: String,
    formation: State<'_, FormationState>,
    memory: State<'_, MemoryHandle>,
) -> AppResult<Option<Vec<u8>>> {
    let root = formation.require()?;
    let memory_dir = root.join(APP_DIR).join("memory");
    let rel = match memory.get_or_init(&memory_dir).await {
        Ok(store) => store.voice_clip_path(&name).await.unwrap_or(None),
        Err(_) => None,
    };
    let Some(rel) = rel else {
        return Ok(None);
    };
    Ok(std::fs::read(root.join(&rel)).ok())
}
