use crate::core::formation_state::{atomic_write, AppConfig, FormationNote, FormationState};
use crate::core::indexer::Indexer;
use crate::core::watcher::FormationWatcher;
use crate::error::{AppError, AppResult};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use walkdir::WalkDir;

/// Subdirectory inside the formation root that holds app-specific state. Skipped during
/// note traversal so the user never sees these files in their note list.
pub const APP_DIR: &str = ".chat-notes";

/// Show the native folder picker; resolve to the chosen path or None if
/// cancelled.
async fn pick_folder(app: &tauri::AppHandle) -> AppResult<Option<PathBuf>> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<PathBuf>>();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path.and_then(|p| p.into_path().ok()));
    });
    rx.await
        .map_err(|e| AppError::other(format!("dialog cancelled: {e}")))
}

/// Pick a formation folder (onboarding / formation switch).
#[tauri::command]
pub async fn pick_formation_dir(app: tauri::AppHandle) -> AppResult<Option<PathBuf>> {
    pick_folder(&app).await
}

/// Pick an arbitrary directory — used by Settings to choose the shared models
/// location.
#[tauri::command]
pub async fn pick_directory(app: tauri::AppHandle) -> AppResult<Option<PathBuf>> {
    pick_folder(&app).await
}

/// Initialize `.chat-notes/` skeleton inside the formation, set the active formation, persist to config.
#[tauri::command]
pub fn open_formation(
    path: PathBuf,
    state: State<'_, FormationState>,
    watcher: State<'_, FormationWatcher>,
    app: tauri::AppHandle,
) -> AppResult<FormationSummary> {
    if !path.is_dir() {
        return Err(AppError::other(format!(
            "not a directory: {}",
            path.display()
        )));
    }
    init_chat_notes_skeleton(&path)?;
    state.set(path.clone());

    let mut config = AppConfig::load(&app);
    config.last_formation_path = Some(path.clone());
    config.save(&app)?;

    // Start watching for external edits; ignore failures (logged) so the
    // formation still opens even on platforms where notify struggles.
    if let Err(e) = watcher.start(path.clone(), app.clone()) {
        tracing::warn!("watcher start failed: {e}");
    }

    let notes = walk_notes(&path)?;
    Ok(FormationSummary {
        path: path.clone(),
        note_count: notes.len(),
    })
}

/// Restore the previously-opened formation on launch, if any. Returns None for first-run.
#[tauri::command]
pub fn restore_last_formation(
    state: State<'_, FormationState>,
    watcher: State<'_, FormationWatcher>,
    app: tauri::AppHandle,
) -> AppResult<Option<FormationSummary>> {
    let mut config = AppConfig::load(&app);
    let Some(path) = config.last_formation_path.clone() else {
        return Ok(None);
    };
    if !path.is_dir() {
        // Stale path — clear it so next launch isn't broken.
        config.last_formation_path = None;
        config.save(&app).ok();
        return Ok(None);
    }
    init_chat_notes_skeleton(&path)?;
    state.set(path.clone());
    if let Err(e) = watcher.start(path.clone(), app.clone()) {
        tracing::warn!("watcher start failed: {e}");
    }
    let notes = walk_notes(&path)?;
    Ok(Some(FormationSummary {
        path,
        note_count: notes.len(),
    }))
}

/// List every `.md` file in the open formation, excluding `.chat-notes/`. Paths are formation-relative.
#[tauri::command]
pub fn list_notes(state: State<'_, FormationState>) -> AppResult<Vec<FormationNote>> {
    let formation = state.require()?;
    walk_notes(&formation)
}

/// Read a note by its formation-relative path.
#[tauri::command]
pub fn read_note(relative_path: String, state: State<'_, FormationState>) -> AppResult<String> {
    let formation = state.require()?;
    let abs = resolve_in_formation(&formation, &relative_path)?;
    Ok(std::fs::read_to_string(abs)?)
}

/// A note that links to the current one — `path` (formation-relative) + display
/// `title` (basename without `.md`).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Backlink {
    pub path: String,
    pub title: String,
}

/// Notes that link to `note_path` via an Obsidian `[[wiki-link]]` to its title
/// (basename without `.md`). Case-insensitive; matches `[[Title]]`, `[[Title|alias]]`,
/// and `[[Title#heading]]`. Scans the formation's `.md` files (the `.chat-notes/`
/// app dir is excluded by `walk_notes`).
#[tauri::command]
pub fn note_backlinks(
    note_path: String,
    state: State<'_, FormationState>,
) -> AppResult<Vec<Backlink>> {
    let formation = state.require()?;
    let title = base_title(&note_path);
    if title.is_empty() {
        return Ok(Vec::new());
    }
    let title_lower = title.to_lowercase();
    let mut out = Vec::new();
    for note in walk_notes(&formation)? {
        if note.relative_path == note_path {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(formation.join(&note.relative_path)) else {
            continue;
        };
        if links_to(&content, &title_lower) {
            out.push(Backlink {
                title: base_title(&note.relative_path).to_string(),
                path: note.relative_path,
            });
        }
    }
    out.sort_by_key(|b| b.title.to_lowercase());
    Ok(out)
}

/// Basename of a formation-relative path without the `.md` extension.
fn base_title(rel: &str) -> &str {
    rel.rsplit('/')
        .next()
        .unwrap_or(rel)
        .trim_end_matches(".md")
}

/// Whether `content` contains a wiki-link whose target (alias/heading stripped)
/// equals `title_lower` (already lowercased).
fn links_to(content: &str, title_lower: &str) -> bool {
    let lower = content.to_lowercase();
    let mut i = 0;
    while let Some(pos) = lower[i..].find("[[") {
        let start = i + pos + 2;
        let Some(end_rel) = lower[start..].find("]]") else {
            break;
        };
        let inner = &lower[start..start + end_rel];
        let target = inner.split(['|', '#']).next().unwrap_or(inner).trim();
        if target == title_lower {
            return true;
        }
        i = start + end_rel + 2;
    }
    false
}

/// Write a note (atomic). Creates parent dirs if needed. Path stays formation-relative.
/// After a successful write, queues the note for background re-indexing
/// (debounced — rapid saves coalesce into one embed pass).
#[tauri::command]
pub fn write_note(
    relative_path: String,
    content: String,
    state: State<'_, FormationState>,
    indexer: State<'_, Indexer>,
) -> AppResult<()> {
    let formation = state.require()?;
    let abs = resolve_in_formation(&formation, &relative_path)?;
    atomic_write(&abs, content.as_bytes())?;
    if relative_path.ends_with(".md") {
        indexer.request(relative_path);
    }
    Ok(())
}

#[derive(Debug, serde::Serialize)]
pub struct FormationSummary {
    pub path: PathBuf,
    pub note_count: usize,
}

fn init_chat_notes_skeleton(formation: &std::path::Path) -> AppResult<()> {
    let app_dir = formation.join(APP_DIR);
    for sub in ["snapshots", "staging", "chat-history"] {
        std::fs::create_dir_all(app_dir.join(sub))?;
    }
    let cfg_path = app_dir.join("config.json");
    if !cfg_path.exists() {
        atomic_write(&cfg_path, b"{}\n")?;
    }
    Ok(())
}

pub(crate) fn walk_notes(root: &std::path::Path) -> AppResult<Vec<FormationNote>> {
    let mut out = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            // Skip the app dir entirely.
            !(e.file_type().is_dir() && e.file_name() == APP_DIR)
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|_| AppError::other("walked outside formation"))?
            .to_string_lossy()
            .replace('\\', "/");

        let modified_secs = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        out.push(FormationNote {
            relative_path: rel,
            modified_secs,
        });
    }
    // Most-recent first by default; UI can re-sort.
    out.sort_by_key(|n| std::cmp::Reverse(n.modified_secs));
    Ok(out)
}

/// Defence-in-depth: refuse paths that escape the formation via `..` or absolute components.
fn resolve_in_formation(formation: &std::path::Path, rel: &str) -> AppResult<PathBuf> {
    let candidate = formation.join(rel);
    let canonical = candidate.canonicalize().or_else(|_| {
        // Note doesn't exist yet (write_note creating new file). Resolve parent and rejoin.
        let parent = candidate
            .parent()
            .ok_or_else(|| AppError::other("invalid path"))?;
        std::fs::create_dir_all(parent).ok();
        let parent_canonical = parent
            .canonicalize()
            .map_err(|e| AppError::other(format!("resolve parent: {e}")))?;
        let file_name = candidate
            .file_name()
            .ok_or_else(|| AppError::other("invalid file name"))?;
        Ok::<PathBuf, AppError>(parent_canonical.join(file_name))
    })?;
    let formation_canonical = formation
        .canonicalize()
        .map_err(|e| AppError::other(format!("resolve formation: {e}")))?;
    if !canonical.starts_with(&formation_canonical) {
        return Err(AppError::other("path escapes formation"));
    }
    Ok(canonical)
}
