# Sediment — Plan: interrupt & steer a turn in flight

**Status:** Proposed (2026-06-18).
**Predecessor:** current HEAD `139dbb7` (non-blocking composer + turn queue —
the user can type and send while the agent is thinking; sent messages are
captured as turns and drained one at a time by `ChatPane`'s `pump` loop).
**Builds on:** [ADR-0009](../adr/0009-conversational-agent.md) (the turn model:
one message → snapshot → engine → diff → revertable audit entry),
[ADR-0011](../adr/0011-working-set-and-push-grounding.md) (pre-pass grounding),
[ADR-0012](../adr/0012-github-copilot-engine.md) (the warm Copilot ACP engine).

This is the "Level 2 — interrupt-and-restart" steering path. The queue we just
shipped lets a thought reach the page immediately and run as a *follow-up* turn.
This plan adds the other half: a queued message can **interrupt** the turn that's
currently running so it takes over now, instead of waiting in line. The choice is
the user's, surfaced as a button **on the queued message** — interrupt to jump
ahead, or do nothing and let the running turn finish.

It deliberately does **not** attempt true mid-token injection (Level 3); that
would break the one-message↔one-snapshot↔one-audit-entry invariant and needs its
own ADR. Interrupt-and-restart preserves that invariant: an interrupted turn
leaves **no** audit entry, exactly like a failed turn does today.

---

## Context for a fresh session

A turn is an atomic unit in `commands/chat.rs::chat_turn` (`chat.rs:80`):

1. persist the user message → `source_chat_id` (`chat.rs:96`)
2. snapshot the whole formation to `…/snapshots/<turn_id>` (`chat.rs:129`)
3. run the engine (`chat.rs:192`) — Claude Code (cold, one-shot) or the warm
   Copilot ACP session
4. diff the snapshot for changed notes; collect Facts stamped with
   `source_chat_id` (`chat.rs:222`)
5. write **one** revertable audit entry (`chat.rs:228`)
6. persist the assistant reply (`chat.rs:243`)

On engine **failure** today, the snapshot is removed and the error propagates
with no audit entry written (`chat.rs:205`). **Interrupt reuses exactly this
path**, plus a revert step: an interrupted turn is a failed turn whose partial
side-effects (note edits already on disk, Facts already recorded) are rolled
back from the snapshot before the snapshot is dropped.

Both engines already kill/stop cleanly — we only need to *trigger* it on demand:

- **Claude Code** (`core/claude_code.rs`): one-shot `claude -p … stream-json`,
  stdin closed at EOF (`claude_code.rs:741`). `drive_turn` runs under a
  `tokio::time::timeout`; on expiry it already does `child.kill().await`
  (`claude_code.rs:714`,`:721`). We add a second wake-up reason: a cancel signal.
- **Copilot** (`core/copilot.rs`): a resident process with a persistent
  single-owner stdin writer task (`copilot.rs:497`) serving many
  `session/prompt`s. `CopilotSession::run_turn` drives a `tokio::select!` loop
  (`copilot.rs:626`). ACP defines a `session/cancel` notification; we add a
  branch that sends it over `writer_tx` and stops the loop.

Revert already exists: `audit::undo_turn` (`audit.rs:406`) restores changed
notes from the snapshot (deleting created notes), deletes recorded Facts, and
removes the snapshot. The cancel path needs the same three steps but driven from
the *live* snapshot dir + `facts_by_source`, since an interrupted turn has no
audit entry to read. We factor the shared body out of `undo_turn`.

---

## UX

```
┌───────────────────────────────────────────────┐
│  … "set up the Q3 planning doc"          (you) │   ← running turn
│  ✦ searching your notes                        │     shows "Thinking…"
│  ✦ editing Projects/Q3.md                      │
│  Thinking…                                      │
│                                                 │
│  … "actually, focus on the budget"      (you)  │   ← queued turn
│  Queued…                  [ Interrupt & run ]   │     button offered only
└───────────────────────────────────────────────┘     while a turn is running
```

- A **queued** message shows `Queued…` and, *only while another turn is actually
  running*, an **`Interrupt & run`** button. Doing nothing is the "continue"
  choice — the message runs when the current turn finishes, as today.
- Clicking **`Interrupt & run`** on a queued message:
  1. moves that message to the front of the processing queue (it may not have
     been next),
  2. cancels the running turn,
  3. the running turn is rolled back and marked **Interrupted** in the
     transcript — not failed — with a **`Resume`** button that re-enqueues it,
  4. the clicked message starts immediately.
- Multiple queued messages each get their own button; interrupting promotes the
  one you clicked, the rest keep their order behind it.
- **Race:** if the running turn finishes on its own between the click and the
  cancel landing, the cancel is a no-op (its token is already deregistered); the
  turn is recorded as **completed**, not interrupted, and the promoted message
  simply runs next. The "interrupted" state is driven by the backend result, not
  by the click.

An interrupted turn's partial reply is discarded (it was abandoned); its receipt
shows nothing recorded because the partial writes were reverted. `Resume`
re-runs it from the original message text as a fresh turn.

> Out of scope but a natural sibling: a plain **`Stop`** on the *running* turn
> (interrupt with nothing queued). Same backend; trivial to add later. This plan
> focuses on the queued-message button the user asked for.

---

## Architecture

```
ChatPane.pump  ──run──►  chat_turn(clientTurnId, …)
     ▲                        │  registers clientTurnId → CancellationToken
     │                        │  in CancelRegistry (Tauri state)
     │                        ▼
 [Interrupt & run]        engine.run_turn(TurnRequest{ …, cancel })
     │  1. move msg to front      │
     │  2. cancel_turn(runningId) ─┼─► token.cancel()
     │                             ▼
     │                 ┌─ Claude Code: select! { … , _ = cancel.cancelled() => child.kill() }
     │                 └─ Copilot:     select! { … , _ = cancel.cancelled() => send session/cancel }
     │                             │
     │                        TurnStop::Interrupted
     │                             ▼
     │             chat_turn: revert_to_snapshot(snapshot, changed, facts_by_source)
     │                        remove snapshot, NO audit entry, delete user chat row
     │                             ▼
     └───────────── ChatTurnResult{ stop: "interrupted", … }  ──► mark turn Interrupted
```

---

## Backend

### 1. Distinguish "cancelled" from "completed" and "failed"

`ConversationEngine::run_turn` returns `AppResult<TurnOutcome>`. Add a stop
reason rather than overloading `Err`:

```rust
// core/conversation.rs
pub enum TurnStop { Completed, Interrupted }

pub struct TurnOutcome {
    pub reply: String,    // partial when Interrupted; chat_turn discards it
    pub stop: TurnStop,   // NEW
}
```

A genuine error stays `Err(AppError)`. `Interrupted` is an `Ok` outcome with a
stop reason — it is an expected, user-driven stop, not a failure.

### 2. Thread a cancel signal through `TurnRequest`

`TurnRequest`'s doc already says "per-turn state lives entirely in the
`TurnRequest`" (`conversation.rs:123`). Keep the trait signature stable by adding
the token there:

```rust
// core/conversation.rs — add to TurnRequest
/// Tripped when the user interrupts this turn. Engines watch it in their
/// stream loop and stop promptly (kill the child / send session/cancel).
pub cancel: tokio_util::sync::CancellationToken,
```

`tokio_util`'s `CancellationToken` is the right primitive: cloneable, awaitable
(`cancelled()`), and idempotent. (Add `tokio-util = { version = "0.7", features
= ["rt"] }` to `src-tauri/Cargo.toml` if not already present.)

### 3. `CancelRegistry` Tauri state + addressing

The frontend's local turn id (`crypto.randomUUID`) is the only stable handle the
UI has *before* the turn completes (the backend `turn_id` is generated inside
`chat_turn` and only returned at the end). So `chat_turn` takes a
`client_turn_id` and registers under it:

```rust
// core/cancel.rs (new)
#[derive(Default)]
pub struct CancelRegistry { inner: Mutex<HashMap<String, CancellationToken>> }
impl CancelRegistry {
    pub fn register(&self, client_turn_id: &str) -> CancellationToken { … }   // insert + return clone
    pub fn cancel(&self, client_turn_id: &str) { /* token.cancel() if present */ }
    pub fn finish(&self, client_turn_id: &str) { /* remove */ }
}
```

- `.manage(core::cancel::CancelRegistry::default())` in `lib.rs:116`-ish.
- New command `cancel_turn(client_turn_id, registry)` → `registry.cancel(&id)`;
  registered in `generate_handler!` (`lib.rs:139` neighbourhood) and exposed in
  `src/lib/tauri.ts` as `cancelTurn(clientTurnId)`.

### 4. `chat_turn` wiring

```rust
pub async fn chat_turn(
    message: String,
    session_id: String,
    client_turn_id: String,                 // NEW
    on_event: Channel<TurnEvent>,
    …,
    cancel: State<'_, CancelRegistry>,       // NEW
) -> AppResult<ChatTurnResult> {
    let token = cancel.register(&client_turn_id);
    // … steps 1–3 unchanged (persist msg, history, snapshot) …
    turn_request.cancel = token.clone();
    let outcome = run the engine as today;
    // ensure we always deregister, even on early return:
    //   let _guard = scopeguard-style finish(client_turn_id) on drop, or finish() on each path.

    match outcome {
        Ok(TurnOutcome { stop: TurnStop::Interrupted, .. }) => {
            // Roll back partial side-effects, then behave like a failed turn:
            let changed = audit::diff_formation(&formation_root, &snapshot_dir)?;
            let fact_ids = store.facts_by_source(&source_chat_id).await?;
            audit::revert_to_snapshot(&formation_root, &snapshot_dir, &changed, &fact_ids, store).await?;
            std::fs::remove_dir_all(&snapshot_dir).ok();
            store.delete_chat_message(&source_chat_id).await?;   // keep history clean
            return Ok(ChatTurnResult { stop: "interrupted", turn_id: String::new(),
                                       reply: String::new(), changed_notes: vec![],
                                       recorded_fact_count: 0, working_set });
        }
        Ok(TurnOutcome { reply, .. }) => { /* steps 5–7 exactly as today */ }
        Err(e) => { std::fs::remove_dir_all(&snapshot_dir).ok(); return Err(e); }
    }
}
```

`ChatTurnResult` gains `stop: "completed" | "interrupted"` (camelCase via serde).
Completed turns set `"completed"` and are unchanged in every other respect.

> **Deregistration must be unconditional.** Use a drop-guard so `finish()` runs
> on success, error, *and* panic — a leaked token would make a later
> `cancel_turn` for a reused id hit a stale turn. (Client ids are UUIDs so reuse
> is unlikely, but the guard is cheap and correct.)

### 5. Revert helper (factor out of `undo_turn`)

`audit::undo_turn` (`audit.rs:406`) already does notes-from-snapshot +
delete-facts + drop-snapshot. Extract the first two steps:

```rust
// audit.rs
pub async fn revert_to_snapshot(
    formation_root: &Path, snapshot_dir: &Path,
    changed_notes: &[ChangedNote], fact_ids: &[String], store: &MemoryStore,
) -> AppResult<()> { /* steps 1–2 of today's undo_turn body */ }
```

`undo_turn` calls it (then removes the audit entry); the cancel path in
`chat_turn` calls it with `diff_formation` output + `facts_by_source`. One code
path, two callers — the revert semantics can't drift.

### 6. Claude Code engine (`core/claude_code.rs`)

`drive_turn` (`claude_code.rs:736`) already loops over `stream-json` lines; the
outer `run_turn` wraps it in a timeout and kills the child on expiry
(`claude_code.rs:714`,`:721`). Add the cancel token as a second stop reason:

```rust
tokio::select! {
    res = drive_turn(&mut child, &prompt, on_event) => { /* timeout-wrapped as today */ }
    _ = turn.cancel.cancelled() => {
        let _ = child.kill().await;                 // same kill the timeout uses
        return Ok(TurnOutcome { reply: String::new(), stop: TurnStop::Interrupted });
    }
}
```

`kill_on_drop` is a belt-and-braces backstop, but explicit `kill().await` (as the
timeout path already does) is what we rely on. The temp MCP config is cleaned up
on every exit path, same as today.

### 7. Copilot engine (`core/copilot.rs`)

`CopilotSession::run_turn`'s `select!` (`copilot.rs:626`) gets a cancel arm. ACP
cancellation is a notification (no response expected); send it on the existing
`writer_tx` and stop:

```rust
// new message builder, mirrors session_prompt_msg (copilot.rs:306)
pub fn session_cancel_msg(session_id: &str) -> Value {
    json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session_id}})
}

// inside the select! loop:
_ = turn.cancel.cancelled() => {
    let _ = self.writer_tx.send(ndjson_line(&session_cancel_msg(&self.session_id)));
    *self.active.lock().await = None;
    return Ok(String::new());   // CopilotEngineHandle maps to TurnStop::Interrupted
}
```

Notes:

- `CopilotEngineHandle::run_turn` holds the `inner` lock for the whole turn
  (`copilot.rs:713`) — that's *fine*, because the cancel travels through the
  `CancellationToken`, not through the handle. The session's own loop observes it
  and writes to its own `writer_tx`. No lock contention, no second `cancel_turn`
  acquiring the busy lock.
- **Do not recycle** the session on cancel. A clean ACP `session/cancel` leaves
  the session usable and warm; recycling would throw away server-side history and
  re-send the persona. Cancel is an expected stop, distinct from the error path
  that *does* recycle (`copilot.rs:752`).
- *Open question:* confirm the installed Copilot CLI honours `session/cancel`
  mid-prompt and reports `stopReason: "cancelled"` (or just stops emitting). If a
  given build ignores it, fall back to killing+recycling the session — uglier but
  always correct. Gate behind a quick live probe in M-test.

---

## Frontend

### 8. Turn state — add an `interrupted` state

`src/lib/store.ts`, `ChatTurn`:

```ts
/** Set when the turn was interrupted by the user; carries the message to resume. */
interrupted?: { body: string };
```

New actions:

- `interruptTurn(id, body)` → `{ pending: false, queued: false, interrupted: { body } }`
  (mirrors `failTurn`).
- `resetTurn` already clears `interrupted` alongside `failure` (extend it) so a
  resume re-queues cleanly.

`startTurn` already accepts `queued`; no change. The local turn id created by
`startTurn` is the `client_turn_id` we now pass to `chat_turn`.

### 9. `tauri.ts`

```ts
chatTurn: (message, sessionId, clientTurnId, onEvent) =>
  invoke<ChatTurnResult>("chat_turn", { message, sessionId, clientTurnId, onEvent: channel }),
cancelTurn: (clientTurnId: string) => invoke<void>("cancel_turn", { clientTurnId }),
```

`ChatTurnResult` gains `stop: "completed" | "interrupted"`.

### 10. `ChatPane` — pump + interrupt

- `runTurn(id, message)` passes `id` as `clientTurnId`. On result, branch on
  `result.stop`:
  - `"interrupted"` → `interruptTurn(id, message)` (don't complete, don't refresh
    panels — nothing was recorded).
  - `"completed"` → unchanged (complete + refresh panels).
- The `pump` loop is unchanged — it still drains `queueRef` one at a time. Reorder
  happens before cancel, so the next `shift()` returns the promoted message.
- New `handleInterrupt(queuedTurnId)`:

```ts
function handleInterrupt(queuedTurnId: string) {
  // find the running turn (pending && !queued); nothing to interrupt? no-op
  const running = turns.find((t) => t.pending && !t.queued);
  if (!running) return;
  // promote the clicked message to the front of the processing queue
  const q = queueRef.current;
  const i = q.findIndex((m) => m.id === queuedTurnId);
  if (i > 0) q.unshift(...q.splice(i, 1));
  // cancel the running turn — its runTurn promise resolves with stop:"interrupted",
  // the pump loop continues and picks up the promoted message next.
  void tauri.cancelTurn(running.id);
}
```

- `Resume` on an interrupted turn = `handleRetry`-style: `resetTurn(id)` →
  enqueue `{ id, message: turn.interrupted.body }` → `pump()`.

### 11. `TurnView` rendering

- **Queued** turn: keep `Queued…`; add an `Interrupt & run` button rendered only
  when a sibling turn is running. Pass a `runningCount > 0` flag (already computed
  in `ChatPane`) down, or compute `canInterrupt` per turn.
- **Interrupted** turn: a muted line — *"Interrupted"* — with a `Resume` button,
  styled like the existing failed-turn affordance but neutral (not `danger`).
- **Running** turn: unchanged (`Thinking…`).

---

## Build order

- **M1 — backend cancel spine.** `TurnStop`, `TurnRequest.cancel`,
  `CancelRegistry`, `cancel_turn` command, `chat_turn` registration +
  deregistration drop-guard. Engines ignore the token for now (compiles, no
  behaviour change). Unit-test the registry.
- **M2 — revert helper.** Extract `audit::revert_to_snapshot`; re-point
  `undo_turn` at it; add the interrupted branch to `chat_turn` (revert + no audit
  entry + delete chat row). Test: simulate an interrupted turn that wrote a note
  and recorded a fact → assert the note matches the snapshot and the fact is gone.
- **M3 — Claude Code cancel.** Add the `select!` cancel arm + `child.kill()`.
  Live test `live_run_turn_*`-style: start a turn, cancel mid-stream, assert
  `TurnStop::Interrupted` and a reaped child.
- **M4 — Copilot cancel.** `session_cancel_msg`, the cancel arm, the
  no-recycle decision. Live probe that the CLI honours `session/cancel`; fall
  back to kill+recycle if not.
- **M5 — frontend.** `interrupted` state + actions, `tauri.ts` changes,
  `handleInterrupt`/`Resume`, `TurnView` buttons. The queue/pump from `139dbb7`
  is already in place.
- **M6 — polish.** Race handling (finish-before-cancel), the optional `Stop` on
  the running turn, copy review, biome/tsc/clippy.

Each milestone compiles and runs; M1–M2 are invisible to the UI, M3–M4 make
cancel real per engine, M5 wires the button.

---

## Testing

- **Registry:** register/cancel/finish; cancel of an unknown id is a no-op;
  double-finish is safe.
- **Revert (M2):** the property that matters — after an interrupted turn, the
  formation is byte-identical to the pre-turn snapshot and `facts_by_source` is
  empty. Reuse the snapshot fixtures around `undo_turn`.
- **Claude Code (M3):** cancel during the captured `stream-json` transcript
  (the deterministic fixtures at `claude_code.rs:887`+) → `Interrupted`, partial
  reply discarded.
- **Copilot (M4):** live `--acp` probe (gated like the existing live tests) that
  `session/cancel` stops the prompt and the session survives for the next turn.
- **Frontend:** the finish-before-cancel race resolves to `completed`; a promoted
  message runs next; `Resume` re-runs an interrupted turn as a fresh turn.

---

## Open questions

1. **Copilot `session/cancel` semantics** — does the installed CLI stop the
   in-flight prompt cleanly and stay usable, or must we kill+recycle? Decides M4's
   fallback. (Probe early.)
2. **Resume = same turn or new turn?** This plan re-runs from the captured text as
   a *fresh* turn (new snapshot, new id). Simpler and matches `handleRetry`. The
   alternative — resuming the engine's partial work — is Level 3 and out of scope.
3. **Keep or drop the interrupted user message from `chat_message` history?** Plan
   drops it (`delete_chat_message`) so the abandoned, half-answered message
   doesn't pollute the next turn's continuity window. If we'd rather preserve the
   literal transcript, keep it — but then the next turn sees a user message with
   no assistant reply, which the engine may find confusing. Recommend: drop.
4. **Auto-revert vs. keep partial edits.** Plan reverts all partial side-effects
   on interrupt (clean "as if it never ran"). If users would rather *keep* a
   half-finished note edit when they steer, that's a different contract — but it
   reintroduces the audit-entry question for a turn that never produced a reply.
   Recommend: revert (matches "interrupt = abandon").
