//! The agent's graph tool surface — ADR-0009 §5, plan M1.
//!
//! Nine tools over the knowledge graph and the embedding index — the part of
//! the formation that is *not* files. Note read/write is the conversational
//! agent's own native file tools (ADR-0009 Option B); these tools cover only
//! what a file cannot: semantic search, entity/relationship lookup, the
//! bi-temporal contradiction check, and the structured `record_*` writes.
//!
//! Each tool is a plain async function over a [`ToolContext`], taking a JSON
//! arguments object and returning a JSON result. [`dispatch`] routes a call by
//! name; [`tool_schemas`] advertises the set. This module is
//! transport-agnostic — M2 (`core/formation_mcp.rs`) wraps it in a stdio MCP
//! server, and it is unit-tested here without one.

use crate::core::formation_state::atomic_write;
use crate::core::memory::{record_id_to_string, slugify, FactRow, FactWriteInput, MemoryStore};
use crate::core::ollama_sidecar::{OllamaSidecar, DEFAULT_EMBED_MODEL};
use crate::core::task_note::{parse_tasks_section, render_tasks_note, ChecklistLine};
use crate::core::tasks::{due_at, put_task, task_key, Task, TaskStatus, TASKS_NOTE_PATH};
use crate::error::{AppError, AppResult};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Entity types the graph schema accepts (mirrors the `ASSERT` on
/// `entity.entity_type` in `core::memory`). `record_fact` validates against
/// this set so a bad type fails with a clear message, not a raw SurrealDB error.
const ENTITY_TYPES: &[&str] = &[
    "person",
    "organization",
    "meeting",
    "project",
    "task",
    "topic",
    "location",
    "date",
    "event",
];

/// Default number of note chunks `search_notes` returns.
const DEFAULT_SEARCH_K: usize = 5;

/// Everything a tool call needs: the graph store, the formation root (for
/// `record_task`, which touches `Tasks.md`), an embedder (for `search_notes`),
/// and the current turn's provenance pointer.
pub struct ToolContext {
    pub store: MemoryStore,
    pub formation_root: PathBuf,
    pub sidecar: OllamaSidecar,
    /// The current turn's user `chat_message` id. Stamped as `source_chat_id`
    /// on every Fact recorded this turn (provenance — tech-spec principle #4).
    pub source_chat_id: String,
}

/// One tool's advertised contract: a name, a one-line description, and a
/// JSON-Schema object for its parameters. M2 hands these to the MCP client so
/// the agent knows what it can call.
pub struct ToolSchema {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// The full tool set the agent sees. Order is stable for readable transcripts.
pub fn tool_schemas() -> Vec<ToolSchema> {
    let str_prop = |desc: &str| json!({ "type": "string", "description": desc });
    vec![
        ToolSchema {
            name: "search_notes",
            description: "Semantic search over the formation's notes. Returns the most \
                          relevant note excerpts for a query.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": str_prop("What to search for, in natural language."),
                    "k": { "type": "integer", "description": "How many excerpts to return (default 5).", "minimum": 1, "maximum": 50 }
                },
                "required": ["query"]
            }),
        },
        ToolSchema {
            name: "find_entity",
            description: "Look up an entity (person, organization, project, …) by name and \
                          return its current relationship Facts.",
            parameters: json!({
                "type": "object",
                "properties": { "name": str_prop("The entity's name.") },
                "required": ["name"]
            }),
        },
        ToolSchema {
            name: "related_facts",
            description: "Return the current relationship Facts recorded for an entity — what \
                          it is connected to in the knowledge graph.",
            parameters: json!({
                "type": "object",
                "properties": { "entity": str_prop("The entity's name.") },
                "required": ["entity"]
            }),
        },
        ToolSchema {
            name: "find_contradiction",
            description: "Before recording a relationship, check whether it contradicts an \
                          existing current Fact (same subject and predicate, different object). \
                          Call this when a new statement might conflict with what is known.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "subject": str_prop("The entity the relationship is about."),
                    "predicate": str_prop("The relationship type, e.g. works_at, reports_to."),
                    "object": str_prop("The entity on the other end of the relationship.")
                },
                "required": ["subject", "predicate", "object"]
            }),
        },
        ToolSchema {
            name: "record_fact",
            description: "Record a relationship between two entities as a bi-temporal graph \
                          Fact. Use only for genuine entity→entity relationships; ordinary \
                          note details belong in the note text, not here.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "subject": str_prop("The entity the relationship is about."),
                    "subject_type": str_prop("One of: person, organization, meeting, project, task, topic, location, date, event."),
                    "predicate": str_prop("The relationship type, e.g. works_at, reports_to, lives_in."),
                    "object": str_prop("The entity on the other end."),
                    "object_type": str_prop("One of: person, organization, meeting, project, task, topic, location, date, event."),
                    "valid_from": str_prop("When the relationship began (YYYY-MM-DD or RFC3339). Defaults to now."),
                    "valid_to": str_prop("When the relationship ended, if it is historical (YYYY-MM-DD or RFC3339).")
                },
                "required": ["subject", "subject_type", "predicate", "object", "object_type"]
            }),
        },
        ToolSchema {
            name: "retract_fact",
            description: "Delete a Fact that was recorded in error — it was never true. To \
                          record that a relationship ended (was true, then changed), use \
                          record_fact with valid_to instead.",
            parameters: json!({
                "type": "object",
                "properties": { "fact_id": str_prop("The id of the Fact to retract, e.g. fact:abc123.") },
                "required": ["fact_id"]
            }),
        },
        ToolSchema {
            name: "record_task",
            description: "Add a reminder to the formation's task list (Tasks.md).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": str_prop("What the reminder is for."),
                    "due": str_prop("The due date (YYYY-MM-DD), if there is one.")
                },
                "required": ["title"]
            }),
        },
        ToolSchema {
            name: "record_open_loop",
            description: "Note an unresolved question or a stated-but-unfulfilled intention to \
                          follow up on later (\"decide on the vendor\", \"they were going to send \
                          the contract\"). Softer than a task: no due date, surfaced gently in \
                          conversation. Use when the user leaves something open.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "title": str_prop("The open loop, as a short phrase."),
                    "context": str_prop("Optional detail or why it matters.")
                },
                "required": ["title"]
            }),
        },
        ToolSchema {
            name: "close_open_loop",
            description: "Resolve an open loop once it has been settled. Pass the loop id shown \
                          in the working set (e.g. open_loop:vendor_a1b2c3).",
            parameters: json!({
                "type": "object",
                "properties": { "loop_id": str_prop("The id of the open loop to close.") },
                "required": ["loop_id"]
            }),
        },
    ]
}

/// Route a tool call by name. `args` is the JSON arguments object; the returned
/// `Value` is the tool's JSON result. An unknown name or invalid arguments is
/// an `Err` — M2 surfaces it to the agent as a tool error, not a crash.
pub async fn dispatch(ctx: &ToolContext, name: &str, args: Value) -> AppResult<Value> {
    match name {
        "search_notes" => search_notes(ctx, args).await,
        "find_entity" => find_entity(ctx, args).await,
        "related_facts" => related_facts(ctx, args).await,
        "find_contradiction" => find_contradiction(ctx, args).await,
        "record_fact" => record_fact(ctx, args).await,
        "retract_fact" => retract_fact(ctx, args).await,
        "record_task" => record_task(ctx, args).await,
        "record_open_loop" => record_open_loop(ctx, args).await,
        "close_open_loop" => close_open_loop(ctx, args).await,
        other => Err(AppError::other(format!("unknown tool: {other}"))),
    }
}

// ── Tools ───────────────────────────────────────────────────────────────────

/// Semantic search over note chunks. Embeds the query, then runs the HNSW
/// vector search in `MemoryStore`.
async fn search_notes(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let query = req_str(&args, "query")?;
    let k = args
        .get("k")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_SEARCH_K);

    let embedding = ctx.sidecar.embed(DEFAULT_EMBED_MODEL, &query).await?;
    let hits = ctx.store.search_chunks(embedding, k).await?;
    let results: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            json!({
                "note_path": h.note_path,
                "chunk_idx": h.chunk_idx,
                "text": h.text,
                "distance": h.distance,
            })
        })
        .collect();
    Ok(json!({ "results": results }))
}

/// Resolve an entity by name and return it with its current outgoing Facts.
async fn find_entity(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let name = req_str(&args, "name")?;
    let Some(entity) = ctx.store.lookup_entity(&name).await? else {
        return Ok(json!({ "found": false }));
    };
    let facts = ctx.store.current_facts(&entity.id).await?;
    Ok(json!({
        "found": true,
        "entity": {
            "id": entity.id,
            "name": entity.canonical_name,
            "type": entity.entity_type,
            "note_path": entity.note_path,
        },
        "current_facts": facts.iter().map(fact_to_json).collect::<Vec<_>>(),
    }))
}

/// The current outgoing Facts for a named entity. A name that resolves to no
/// entity yields an empty list rather than an error.
async fn related_facts(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let name = req_str(&args, "entity")?;
    let facts = match ctx.store.lookup_entity(&name).await? {
        Some(entity) => ctx.store.current_facts(&entity.id).await?,
        None => Vec::new(),
    };
    Ok(json!({ "facts": facts.iter().map(fact_to_json).collect::<Vec<_>>() }))
}

/// Check whether `(subject, predicate, object)` would contradict an existing
/// current Fact — same subject and predicate, a *different* object. A subject
/// not yet in the graph cannot contradict anything.
async fn find_contradiction(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let subject = req_str(&args, "subject")?;
    let predicate = req_str(&args, "predicate")?;
    let object = req_str(&args, "object")?;

    let Some(subject_entity) = ctx.store.lookup_entity(&subject).await? else {
        return Ok(json!({ "contradictions": [] }));
    };
    // The object may be brand-new; a synthetic slug id still lets `find_conflicts`
    // exclude the exact-same-object restatement case correctly.
    let object_id = match ctx.store.lookup_entity(&object).await? {
        Some(e) => e.id,
        None => format!("entity:{}", slugify(&object)),
    };

    let conflicts = ctx
        .store
        .find_conflicts(&subject_entity.id, &predicate, &object_id)
        .await?;
    let contradictions: Vec<Value> = conflicts
        .into_iter()
        .map(|c| {
            json!({
                "predicate": c.predicate,
                "existing_object": c.object_name,
                "since": c.valid_from.to_rfc3339(),
                "source_chat_id": c.source_chat_id,
            })
        })
        .collect();
    Ok(json!({ "contradictions": contradictions }))
}

/// Record a relationship as a bi-temporal `fact` edge, upserting both endpoint
/// entities. A closed interval (`valid_to` set) records a historical
/// relationship; an open one supersedes any contradicting current Fact.
async fn record_fact(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let subject = req_str(&args, "subject")?;
    let subject_type = req_str(&args, "subject_type")?;
    let predicate = req_str(&args, "predicate")?;
    let object = req_str(&args, "object")?;
    let object_type = req_str(&args, "object_type")?;
    check_entity_type(&subject_type)?;
    check_entity_type(&object_type)?;

    let valid_from = match opt_str(&args, "valid_from") {
        Some(s) => parse_dt(&s)?,
        None => Utc::now(),
    };
    let valid_to = match opt_str(&args, "valid_to") {
        Some(s) => Some(parse_dt(&s)?),
        None => None,
    };

    let subject_entity = ctx
        .store
        .upsert_entity(&subject, &subject_type, Vec::new())
        .await?;
    let object_entity = ctx
        .store
        .upsert_entity(&object, &object_type, Vec::new())
        .await?;

    let fact_id = ctx
        .store
        .relate_fact(FactWriteInput {
            subject_id: subject_entity.id.clone(),
            predicate,
            object_id: object_entity.id.clone(),
            valid_from,
            valid_to,
            source_chat_id: ctx.source_chat_id.clone(),
            confidence: 1.0,
        })
        .await?;

    Ok(json!({
        "fact_id": fact_id,
        "subject_id": subject_entity.id,
        "object_id": object_entity.id,
        "subject_was_new": subject_entity.was_new,
        "object_was_new": object_entity.was_new,
    }))
}

/// Delete a Fact edge — used only for a Fact recorded in error (ADR-0009 §6:
/// retract, not supersede).
async fn retract_fact(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let fact_id = req_str(&args, "fact_id")?;
    ctx.store.delete_fact(&fact_id).await?;
    Ok(json!({ "retracted": true, "fact_id": fact_id }))
}

/// Append a reminder to the `## Tasks` section of `Tasks.md` and mirror it into
/// the `task` table (ADR-0007). Writes the note file directly — captured for
/// undo by the turn's pre-turn formation snapshot (ADR-0009 §6).
async fn record_task(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let title = req_str(&args, "title")?;
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(AppError::other("record_task: title is empty"));
    }
    let due: Option<NaiveDate> = match opt_str(&args, "due") {
        Some(s) => Some(
            NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").map_err(|_| {
                AppError::other(format!(
                    "record_task: bad due date {s:?} (expected YYYY-MM-DD)"
                ))
            })?,
        ),
        None => None,
    };

    let tasks_path = ctx.formation_root.join(TASKS_NOTE_PATH);
    let existing = std::fs::read_to_string(&tasks_path).ok();

    let id = Task::new_id(&title);
    let mut lines = parse_tasks_section(existing.as_deref().unwrap_or(""));
    lines.push(ChecklistLine {
        done: false,
        title: title.clone(),
        due,
        completed: None,
        id: Some(task_key(&id)),
    });
    let new_content = render_tasks_note(existing.as_deref(), &lines);
    atomic_write(&tasks_path, new_content.as_bytes())?;

    let now = Utc::now();
    let remind_at = due.map(due_at);
    put_task(
        &ctx.store,
        &Task {
            id: id.clone(),
            title: title.clone(),
            status: TaskStatus::Open,
            due: remind_at,
            remind_at,
            notified: false,
            created: now,
            completed_at: None,
            source_chat_id: Some(ctx.source_chat_id.clone()),
        },
    )
    .await?;

    Ok(json!({
        "task_id": id,
        "title": title,
        "due": due.map(|d| d.format("%Y-%m-%d").to_string()),
    }))
}

/// Record an Open Loop (ADR-0011 §5) — a soft, unresolved thread the agent will
/// surface later. Distinct from `record_task` (a scheduled reminder).
async fn record_open_loop(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let title = req_str(&args, "title")?;
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::other("record_open_loop: title is empty"));
    }
    let context = opt_str(&args, "context");
    let id = ctx
        .store
        .record_open_loop(title, context.as_deref(), &ctx.source_chat_id)
        .await?;
    Ok(json!({ "loop_id": id, "title": title }))
}

/// Close an Open Loop once resolved (ADR-0011 §5).
async fn close_open_loop(ctx: &ToolContext, args: Value) -> AppResult<Value> {
    let loop_id = req_str(&args, "loop_id")?;
    ctx.store.close_open_loop(&loop_id).await?;
    Ok(json!({ "closed": true, "loop_id": loop_id }))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Render a `FactRow` as a clean JSON object, stringifying SurrealDB record ids
/// (a raw `RecordId` serialises to an unfriendly nested shape).
fn fact_to_json(f: &FactRow) -> Value {
    json!({
        "fact_id": record_id_to_string(&f.id),
        "subject": record_id_to_string(&f.subject),
        "predicate": f.predicate,
        "object": record_id_to_string(&f.object),
        "valid_from": f.valid_from.to_rfc3339(),
        "valid_to": f.valid_to.map(|t| t.to_rfc3339()),
        "current": f.valid_to.is_none(),
    })
}

/// A required string argument, or a descriptive error.
fn req_str(args: &Value, key: &str) -> AppResult<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::other(format!("missing required string argument: {key}")))
}

/// An optional string argument (absent or non-string → `None`).
fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reject an entity type the graph schema would not accept, with a message
/// listing the valid set.
fn check_entity_type(t: &str) -> AppResult<()> {
    if ENTITY_TYPES.contains(&t) {
        Ok(())
    } else {
        Err(AppError::other(format!(
            "invalid entity type {t:?}; expected one of: {}",
            ENTITY_TYPES.join(", ")
        )))
    }
}

/// Parse an `YYYY-MM-DD` date or an RFC3339 datetime into a UTC datetime.
fn parse_dt(s: &str) -> AppResult<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).expect("midnight is valid")));
    }
    Err(AppError::other(format!(
        "could not parse {s:?} as a date (YYYY-MM-DD) or datetime (RFC3339)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tasks::list_tasks;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-formation-tools")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// A `ToolContext` over a fresh temp store + formation. The sidecar is
    /// unused by every test except the `#[ignore]`d `search_notes` one.
    async fn ctx() -> (ToolContext, PathBuf) {
        let root = tempdir();
        let store = MemoryStore::open(&root.join(".chat-notes").join("memory"))
            .await
            .expect("open store");
        let ctx = ToolContext {
            store,
            formation_root: root.clone(),
            sidecar: OllamaSidecar::default(),
            source_chat_id: "chat_message:test".to_string(),
        };
        (ctx, root)
    }

    /// The advertised schema set is the nine tools, each with an object schema.
    #[test]
    fn tool_schemas_lists_the_nine_tools() {
        let schemas = tool_schemas();
        assert_eq!(schemas.len(), 9);
        let names: Vec<&str> = schemas.iter().map(|s| s.name).collect();
        for expected in [
            "search_notes",
            "find_entity",
            "related_facts",
            "find_contradiction",
            "record_fact",
            "retract_fact",
            "record_task",
            "record_open_loop",
            "close_open_loop",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
        for s in &schemas {
            assert_eq!(s.parameters["type"], "object", "{} schema", s.name);
        }
    }

    /// An unknown tool name is an error, not a panic.
    #[tokio::test]
    async fn dispatch_rejects_an_unknown_tool() {
        let (ctx, root) = ctx().await;
        assert!(dispatch(&ctx, "no_such_tool", json!({})).await.is_err());
        std::fs::remove_dir_all(root).ok();
    }

    /// record_open_loop creates an active loop; close_open_loop archives it —
    /// both through dispatch (ADR-0011 §5).
    #[tokio::test]
    async fn open_loop_record_and_close_via_dispatch() {
        let (ctx, root) = ctx().await;
        let created = dispatch(
            &ctx,
            "record_open_loop",
            json!({ "title": "Decide on vendor" }),
        )
        .await
        .expect("record_open_loop");
        let loop_id = created["loop_id"].as_str().expect("loop_id").to_string();
        assert!(loop_id.starts_with("open_loop:"));
        let active = ctx.store.list_active_open_loops(10, 14).await.unwrap();
        assert!(active.iter().any(|l| l.id == loop_id), "loop is active");

        let closed = dispatch(
            &ctx,
            "close_open_loop",
            json!({ "loop_id": loop_id.clone() }),
        )
        .await
        .expect("close_open_loop");
        assert_eq!(closed["closed"], true);
        let active = ctx.store.list_active_open_loops(10, 14).await.unwrap();
        assert!(
            !active.iter().any(|l| l.id == loop_id),
            "closed loop is gone"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// record_fact upserts both entities and relates them; find_entity then
    /// resolves the subject and returns the new current Fact.
    #[tokio::test]
    async fn record_fact_then_find_entity_round_trip() {
        let (ctx, root) = ctx().await;

        let rec = dispatch(
            &ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "person",
                "predicate": "works_at",
                "object": "Cloudflare", "object_type": "organization"
            }),
        )
        .await
        .expect("record_fact");
        assert!(rec["fact_id"].as_str().unwrap().starts_with("fact:"));
        assert_eq!(rec["subject_was_new"], true);

        let found = dispatch(&ctx, "find_entity", json!({ "name": "Josh" }))
            .await
            .expect("find_entity");
        assert_eq!(found["found"], true);
        let facts = found["current_facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1, "the recorded fact is current");
        assert_eq!(facts[0]["predicate"], "works_at");
        assert_eq!(facts[0]["current"], true);

        // An unknown name resolves to found:false.
        let miss = dispatch(&ctx, "find_entity", json!({ "name": "Nobody" }))
            .await
            .expect("find_entity miss");
        assert_eq!(miss["found"], false);

        std::fs::remove_dir_all(root).ok();
    }

    /// record_fact rejects an entity type outside the schema's allowed set.
    #[tokio::test]
    async fn record_fact_rejects_an_invalid_entity_type() {
        let (ctx, root) = ctx().await;
        let err = dispatch(
            &ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "wizard",
                "predicate": "works_at",
                "object": "Cloudflare", "object_type": "organization"
            }),
        )
        .await;
        assert!(err.is_err(), "an invalid entity type must be rejected");
        std::fs::remove_dir_all(root).ok();
    }

    /// find_contradiction flags a conflicting current Fact and ignores a
    /// same-object restatement.
    #[tokio::test]
    async fn find_contradiction_flags_a_conflicting_employer() {
        let (ctx, root) = ctx().await;
        dispatch(
            &ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "person",
                "predicate": "works_at",
                "object": "Cloudflare", "object_type": "organization"
            }),
        )
        .await
        .expect("seed fact");

        let conflict = dispatch(
            &ctx,
            "find_contradiction",
            json!({ "subject": "Josh", "predicate": "works_at", "object": "Acme" }),
        )
        .await
        .expect("find_contradiction");
        let hits = conflict["contradictions"].as_array().unwrap();
        assert_eq!(hits.len(), 1, "a different employer is a contradiction");
        assert_eq!(hits[0]["existing_object"], "Cloudflare");

        // The same employer is a restatement, not a contradiction.
        let same = dispatch(
            &ctx,
            "find_contradiction",
            json!({ "subject": "Josh", "predicate": "works_at", "object": "Cloudflare" }),
        )
        .await
        .expect("find_contradiction same");
        assert!(same["contradictions"].as_array().unwrap().is_empty());

        // An unknown subject cannot contradict anything.
        let unknown = dispatch(
            &ctx,
            "find_contradiction",
            json!({ "subject": "Ghost", "predicate": "works_at", "object": "Acme" }),
        )
        .await
        .expect("find_contradiction unknown");
        assert!(unknown["contradictions"].as_array().unwrap().is_empty());

        std::fs::remove_dir_all(root).ok();
    }

    /// retract_fact deletes the edge — the entity then has no current Facts.
    #[tokio::test]
    async fn retract_fact_removes_the_edge() {
        let (ctx, root) = ctx().await;
        let rec = dispatch(
            &ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "person",
                "predicate": "works_at",
                "object": "Cloudflare", "object_type": "organization"
            }),
        )
        .await
        .expect("record_fact");
        let fact_id = rec["fact_id"].as_str().unwrap().to_string();

        dispatch(&ctx, "retract_fact", json!({ "fact_id": fact_id }))
            .await
            .expect("retract_fact");

        let found = dispatch(&ctx, "find_entity", json!({ "name": "Josh" }))
            .await
            .expect("find_entity");
        assert!(
            found["current_facts"].as_array().unwrap().is_empty(),
            "the retracted fact is gone"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// A closed interval supersedes the open one; related_facts then shows the
    /// historical Fact as not current.
    #[tokio::test]
    async fn record_fact_with_valid_to_is_historical() {
        let (ctx, root) = ctx().await;
        dispatch(
            &ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "person",
                "predicate": "worked_at",
                "object": "Cloudflare", "object_type": "organization",
                "valid_from": "2019-01-01", "valid_to": "2020-01-01"
            }),
        )
        .await
        .expect("record historical fact");

        let related = dispatch(&ctx, "related_facts", json!({ "entity": "Josh" }))
            .await
            .expect("related_facts");
        let facts = related["facts"].as_array().unwrap();
        // A closed edge is not "current", so it is absent from current_facts.
        assert!(
            facts.is_empty(),
            "a historical fact is not a current fact: {facts:?}"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// record_task writes the checklist line into Tasks.md and mirrors a row
    /// into the task table.
    #[tokio::test]
    async fn record_task_writes_the_note_and_the_row() {
        let (ctx, root) = ctx().await;
        let res = dispatch(
            &ctx,
            "record_task",
            json!({ "title": "Renew passport", "due": "2026-06-01" }),
        )
        .await
        .expect("record_task");
        assert!(res["task_id"].as_str().unwrap().starts_with("task:"));

        let note = std::fs::read_to_string(root.join("Tasks.md")).expect("Tasks.md written");
        assert!(note.contains("## Tasks"));
        assert!(note.contains("- [ ] Renew passport"));
        assert!(note.contains("📅 2026-06-01"));

        let tasks = list_tasks(&ctx.store).await.expect("list tasks");
        assert_eq!(tasks.len(), 1, "exactly one task row mirrored");
        assert_eq!(tasks[0].title, "Renew passport");
        assert_eq!(tasks[0].status, TaskStatus::Open);
        assert!(tasks[0].remind_at.is_some(), "a due date seeds remind_at");

        std::fs::remove_dir_all(root).ok();
    }

    /// `search_notes` needs a running Ollama for the query embedding — Layer 2
    /// of the test strategy (ADR-0006), excluded from CI.
    #[tokio::test]
    #[ignore]
    async fn search_notes_returns_results() {
        let (ctx, root) = ctx().await;
        let res = dispatch(&ctx, "search_notes", json!({ "query": "anything", "k": 3 }))
            .await
            .expect("search_notes (is Ollama running?)");
        assert!(res["results"].is_array());
        std::fs::remove_dir_all(root).ok();
    }
}
