# Sediment — Plan: interrupt, steer & redirect a turn in flight

**Status:** Proposed (2026-06-18; revised the same day after design feedback).
**Predecessor:** current HEAD `a64d5a7` (non-blocking composer + turn queue —
the user can type and send while the agent is thinking; sent messages are
captured as turns and drained one at a time by `ChatPane`'s `pump` loop).
**Builds on:** [ADR-0009](../adr/0009-conversational-agent.md) (the turn model:
one message → snapshot → engine → diff → revertable audit entry; Sediment owns
the transcript in `chat_message`, scoped by `session_id`),
[ADR-0011](../adr/0011-working-set-and-push-grounding.md) (pre-pass grounding),
[ADR-0012](../adr/0012-github-copilot-engine.md) (the warm Copilot ACP engine).

The queue we shipped lets a thought reach the page immediately and run as a
*follow-up* turn. This plan adds three controls on top of it:

1. **Steer** — interrupt the running turn, **keep** what it did (and the whole
   conversation), and run a queued message next. A nudge.
2. **Redirect** — interrupt the running turn, **revert** what it did, and run a
   queued message instead. A change of direction.
3. **New conversation** — clear the chat session and start a fresh topic, while
   the formation (long-term memory) is untouched.

Plus the queue mechanics these imply: with several messages waiting, the user
can promote, reorder, and drop them.

It deliberately does **not** attempt true mid-token injection (Level 3); both
Steer and Redirect stop the turn at the next safe point and run the new message
as the next turn. That preserves the one-message↔one-snapshot↔one-audit-entry
invariant — every committed turn still has exactly one snapshot and one audit
entry; a redirected turn, like a failed one today, has none.

---

## Context for a fresh session

A turn is atomic in `commands/chat.rs::chat_turn` (`chat.rs:80`): persist the
user message → `source_chat_id` (`chat.rs:96`); snapshot the formation to
`…/snapshots/<turn_id>` (`chat.rs:129`); run the engine (`chat.rs:192`); diff the
snapshot + collect Facts stamped with `source_chat_id` (`chat.rs:222`); write one
revertable audit entry (`chat.rs:228`); persist the reply (`chat.rs:243`).

On engine **failure** today the snapshot is removed and the error propagates with
no audit entry (`chat.rs:205`). **Redirect reuses this path**, plus a revert step.
**Steer reuses the success path** — it commits whatever landed.

Both engines stop cleanly; we only need to trigger it on demand:

- **Claude Code** (`core/claude_code.rs`): one-shot `claude -p … stream-json`,
  stdin closed at EOF (`claude_code.rs:741`). `run_turn` wraps `drive_turn` in a
  timeout and already does `child.kill().await` on expiry (`claude_code.rs:721`).
- **Copilot** (`core/copilot.rs`): a resident process with a single-owner stdin
  writer task (`copilot.rs:497`); `CopilotSession::run_turn` drives a
  `tokio::select!` loop (`copilot.rs:626`). ACP *defines* a `session/cancel`
  notification — **but we don't know the installed CLI honours it** (see M4).

Revert exists: `audit::undo_turn` (`audit.rs:406`) restores changed notes from a
snapshot (deleting created notes), deletes recorded Facts, drops the snapshot.
We factor its note+fact body out so the live cancel path can reuse it.

History is `session_id`-scoped: `recent_messages` filters on `session_id`
(`memory.rs:378`), so a **new `session_id` is a clean conversation** with no new
storage and nothing deleted — the launch-fresh model ADR-0009 already assumes.
There is no `delete_chat_message` yet; Redirect needs one (M2).

---

## UX

### Two interrupt buttons on a queued message

```
┌─────────────────────────────────────────────────────────────┐
│  … "set up the Q3 planning doc"                        (you) │  running turn
│  ✦ searching your notes                                      │  "Thinking…"
│  ✦ editing Projects/Q3.md                                    │
│  Thinking…                                                   │
│                                                              │
│  … "actually, lead with the budget"                   (you) │  queued — NEXT
│  Queued · next        [ Steer ]  [ Redirect ]   [ ✕ ]        │
│                                                              │
│  … "and cc Dana"                                      (you) │  queued · 2nd
│  Queued · 2nd         [ Steer ]  [ Redirect ]   [ ↑ ] [ ✕ ]  │
└─────────────────────────────────────────────────────────────┘
```

Each **queued** message shows its place in line (`next`, `2nd`, …) and, *only
while a turn is actually running*, the interrupt buttons. Doing nothing is the
"let it finish" choice — the message runs when its turn comes, as today.

- **Steer** — stop the running turn, **keep** its partial work and reply (it is
  committed as a normal, revertable turn, badged *Steered*), keep the whole
  conversation, and run **this** message next.
- **Redirect** — stop the running turn, **revert** its partial work, collapse it
  to a thin *Redirected* tombstone (with **Resume**), and run **this** message
  next.

In both cases, **turns before the interrupted one are never touched** — Redirect
only rolls back the single in-flight turn the user chose to abandon.

### What each does — decision table

| | **Steer** (keep) | **Redirect** (revert) |
|---|---|---|
| Stop the running engine | yes | yes |
| Partial note edits / Facts | **kept**, committed | **reverted** from snapshot |
| Audit entry for the turn | written (revertable later) | none |
| Interrupted user message in history | **kept** | **removed** (`delete_chat_message`) |
| Partial assistant reply | kept (may be mid-sentence) | discarded |
| Transcript | normal turn, *Steered* badge | thin *Redirected* tombstone + Resume |
| Earlier turns | untouched | untouched |
| The promoted message | runs next | runs next |

Steer keeps a possibly-partial reply; because note writes are atomic per edit
(`atomic_write`), files are never left half-written — at worst the turn made
*fewer* edits than it would have. That's an acceptable nudge artifact.

> **Why two buttons, not a setting:** these are the two honest answers to "what
> happens to the work already done?" — sometimes the half-built doc is worth
> keeping and you just want to add direction (Steer); sometimes you realise it's
> the wrong doc entirely (Redirect). The user picks per interruption.

### Queue with several messages — promote, reorder, drop

The processing queue is explicit and editable while turns drain:

- **Steer / Redirect** on any queued message promotes it to the **front**
  (runs next); the others keep their relative order behind it.
- **↑ / ↓** reorder a queued message without interrupting.
- **✕** drops a queued message: removes it from the queue *and* the transcript
  (it was never run). This is "bypass — I don't need this one after all."
- New sends always append to the back, as today.

Place-in-line badges (`next`, `2nd`, …) are derived from queue position so the
processing order is always legible even though the transcript stays chronological.

While a cancel is in flight (the click landed, the engine hasn't stopped yet) the
interrupt buttons on **all** queued messages are disabled, so a double-tap can't
race two interrupts against one running turn. They re-enable when the turn stops.

### New conversation

A **New conversation** control (in a slim header strip above the transcript, and
mirrored in the command palette) starts a fresh topic:

1. If a turn is running, interrupt it **with revert** (Redirect semantics) — you
   are leaving the topic, so its in-flight work is rolled back.
2. Clear the queue and the transcript.
3. Mint a new `session_id` (the chat store owns it).
4. For the warm Copilot engine, recycle the resident ACP session so no
   server-side context bleeds across topics (see Backend §7).

The **formation, graph, and `Self.md` are untouched** — durable memory persists;
only the conversation resets, exactly as a fresh app launch behaves. Old
`chat_message` rows stay on disk but are never queried again (a future "clear
history" could prune them; out of scope here).

---

## Architecture

```
ChatPane.pump ──run──► chat_turn(clientTurnId, conversationId, …)
   ▲                       │ registers clientTurnId → CancelHandle{token, mode}
   │                       ▼
[Steer]/[Redirect]     engine.run_turn(TurnRequest{ …, cancel })
   │ 1. promote msg to front     │
   │ 2. cancelTurn(runId, mode) ─┼─► handle.mode = mode; token.cancel()
   │                             ▼
   │              ┌ Claude Code: select!{ … , _ = cancel.cancelled() => child.kill() }
   │              └ Copilot:     select!{ … , _ = cancel.cancelled() => session/cancel
   │                                                         (fallback: kill+recycle) }
   │                             │  TurnStop::Interrupted
   │                             ▼
   │        chat_turn reads handle.mode:
   │          Steer    → commit (diff, facts, audit entry, persist partial reply)   → stop:"steered"
   │          Redirect → revert_to_snapshot + delete_chat_message + no audit entry  → stop:"redirected"
   │                             ▼
   └────────── ChatTurnResult{ stop } ──► mark turn Steered / Redirected; pump continues
```

---

## Backend

### 1. A stop reason on the outcome

`core/conversation.rs`:

```rust
pub enum TurnStop { Completed, Interrupted }

pub struct TurnOutcome {
    pub reply: String,    // partial when Interrupted
    pub stop: TurnStop,   // NEW — engines only know "completed" vs "interrupted"
}
```

The engine does **not** know Steer-vs-Redirect; it just stops. The keep/revert
decision is `chat_turn`'s, read from the cancel handle. A genuine error stays
`Err`.

### 2. Cancel signal on `TurnRequest`

`TurnRequest`'s doc says per-turn state lives in the request (`conversation.rs:123`):

```rust
/// Tripped when the user interrupts this turn; engines watch it and stop.
pub cancel: tokio_util::sync::CancellationToken,
/// Identifies the chat session this turn belongs to. The warm Copilot engine
/// recycles its ACP session when this changes (New conversation). Cold engines
/// ignore it — they render history from the transcript window each turn.
pub conversation_id: String,
```

(Add `tokio-util = { version = "0.7" }` to `src-tauri/Cargo.toml` if absent.)

### 3. `CancelRegistry` (Tauri state) keyed by the client turn id

The frontend's local turn id (`crypto.randomUUID`) is the only handle the UI has
before the turn completes, so `chat_turn` registers under it and `cancel_turn`
addresses it. The handle also carries the requested mode:

```rust
// core/cancel.rs (new)
#[derive(Clone, Copy)]
pub enum CancelMode { Steer, Redirect }

struct CancelHandle { token: CancellationToken, mode: Arc<Mutex<Option<CancelMode>>> }

#[derive(Default)]
pub struct CancelRegistry { inner: Mutex<HashMap<String, CancelHandle>> }

impl CancelRegistry {
    pub fn register(&self, client_turn_id: &str) -> CancellationToken;     // insert, return token clone
    pub fn cancel(&self, client_turn_id: &str, mode: CancelMode);          // set mode, trip token
    pub fn taken_mode(&self, client_turn_id: &str) -> Option<CancelMode>;  // read mode after stop
    pub fn finish(&self, client_turn_id: &str);                            // remove
}
```

- `.manage(core::cancel::CancelRegistry::default())` in `lib.rs` (~`:116`).
- New command `cancel_turn(client_turn_id, mode, registry)` →
  `registry.cancel(&id, mode)`; registered in `generate_handler!` (~`lib.rs:139`)
  and exposed in `src/lib/tauri.ts`.

### 4. `chat_turn` branching

```rust
pub async fn chat_turn(
    message: String, session_id: String, client_turn_id: String,   // client_turn_id NEW
    on_event: Channel<TurnEvent>, …, cancel: State<'_, CancelRegistry>,
) -> AppResult<ChatTurnResult> {
    let token = cancel.register(&client_turn_id);
    let _finish = FinishGuard(&cancel, &client_turn_id);  // deregister on every exit, incl. panic
    // … persist msg, history, snapshot (unchanged) …
    turn_request.cancel = token.clone();
    turn_request.conversation_id = session_id.clone();

    let outcome = run the engine as today;
    match outcome {
        Ok(TurnOutcome { stop: TurnStop::Interrupted, reply }) => {
            match cancel.taken_mode(&client_turn_id).unwrap_or(CancelMode::Redirect) {
                CancelMode::Steer => { /* fall through to the normal commit path with `reply` */ }
                CancelMode::Redirect => {
                    let changed = audit::diff_formation(&formation_root, &snapshot_dir)?;
                    let facts   = store.facts_by_source(&source_chat_id).await?;
                    audit::revert_to_snapshot(&formation_root, &snapshot_dir, &changed, &facts, store).await?;
                    std::fs::remove_dir_all(&snapshot_dir).ok();
                    store.delete_chat_message(&source_chat_id).await?;     // NEW (M2)
                    return Ok(ChatTurnResult{ stop: "redirected", turn_id: String::new(),
                        reply: String::new(), changed_notes: vec![], recorded_fact_count: 0, working_set });
                }
            }
        }
        Ok(_) => { /* completed — unchanged */ }
        Err(e) => { std::fs::remove_dir_all(&snapshot_dir).ok(); return Err(e); }
    }
    // commit path (completed OR steered): diff, facts, audit entry, persist reply (unchanged) …
    Ok(ChatTurnResult{ stop: if was_interrupted { "steered" } else { "completed" }, … })
}
```

`ChatTurnResult` gains `stop: "completed" | "steered" | "redirected"`. Steered
turns are real, audited, revertable turns — just flagged so the UI can badge them
and the partial reply is understood as such.

> **Deregistration is unconditional** via a drop-guard, so a leaked token can't
> let a later `cancel_turn` hit a stale turn (client ids are UUIDs, but the guard
> is cheap and correct).

### 5. Shared revert helper (factor out of `undo_turn`)

```rust
// audit.rs — steps 1–2 of today's undo_turn body
pub async fn revert_to_snapshot(
    formation_root: &Path, snapshot_dir: &Path,
    changed_notes: &[ChangedNote], fact_ids: &[String], store: &MemoryStore,
) -> AppResult<()>;
```

`undo_turn` calls it then removes the audit entry; the Redirect path calls it with
`diff_formation` + `facts_by_source`. One revert path, two callers — semantics
can't drift.

### 6. Claude Code cancel arm (`core/claude_code.rs`)

```rust
tokio::select! {
    res = (timeout-wrapped drive_turn, as today) => { … }
    _ = turn.cancel.cancelled() => {
        let _ = child.kill().await;                       // the kill the timeout path already uses
        return Ok(TurnOutcome{ reply: streamed_so_far, stop: TurnStop::Interrupted });
    }
}
```

`drive_turn` should accumulate the streamed reply so a Steer can keep it; today
the reply is rebuilt from the `result` line, so capture deltas as they stream (or
return the partial accumulator on cancel).

### 7. Copilot cancel arm + the honour-unknown fallback (`core/copilot.rs`)

ACP cancel is a notification (no response); send it on the existing `writer_tx`:

```rust
pub fn session_cancel_msg(session_id: &str) -> Value {           // mirrors session_prompt_msg (:306)
    json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session_id}})
}
```

**Because we don't know the installed CLI honours `session/cancel`, the cancel arm
is defensive:**

```rust
_ = turn.cancel.cancelled() => {
    let _ = self.writer_tx.send(ndjson_line(&session_cancel_msg(&self.session_id)));
    // Give it a short grace window to wind down and emit stopReason "cancelled".
    // If the prompt response doesn't arrive within COPILOT_CANCEL_GRACE, force it:
    //   return a sentinel so CopilotEngineHandle kills + recycles the session.
    return Ok(String::new());   // handle maps to TurnStop::Interrupted
}
```

- `CopilotEngineHandle::run_turn` holds the `inner` lock for the whole turn
  (`copilot.rs:713`) — fine: the cancel travels through the token, not the handle;
  the session's own loop sends `session/cancel` on its own `writer_tx`. No lock
  contention with a concurrent `cancel_turn`.
- **Recycle policy:** if cancel returns cleanly within the grace window, keep the
  warm session (it stays usable). If the grace window elapses, **kill + recycle**
  (the existing error path, `copilot.rs:752`) — always correct, just colder. M4
  probes which branch the installed CLI takes and records it.
- **New conversation recycle:** add `conversation_id` to `ResidentEngine` and the
  `need_new` check (`copilot.rs:715`): recycle when it changes, so a New
  conversation transparently starts a fresh `session/new`. Cold Claude Code needs
  nothing — it renders history from the (now-empty) transcript window.

---

## Frontend

### 8. Store (`src/lib/store.ts`)

`ChatTurn` gains discriminated end-states (mirroring `failure`):

```ts
/** Set when Steer kept a partial, interrupted turn — a committed, revertable turn. */
steered?: boolean;
/** Set when Redirect reverted the turn; carries the text so Resume can re-run it. */
redirected?: { body: string };
```

Actions: `steerTurn(id)`, `redirectTurn(id, body)`, and `resetTurn` extended to
clear both. `newConversation()` → `set({ sessionId: crypto.randomUUID(), turns: [] })`.

The local turn id from `startTurn` is the `clientTurnId` passed to `chat_turn`.

### 9. `tauri.ts`

```ts
chatTurn: (message, sessionId, clientTurnId, onEvent) =>
  invoke<ChatTurnResult>("chat_turn", { message, sessionId, clientTurnId, onEvent: channel }),
cancelTurn: (clientTurnId: string, mode: "steer" | "redirect") =>
  invoke<void>("cancel_turn", { clientTurnId, mode }),
```

`ChatTurnResult.stop: "completed" | "steered" | "redirected"`.

### 10. `ChatPane` — pump, interrupt, reorder, new conversation

The `pump` loop is unchanged in shape (drain `queueRef` one at a time); the queue
just becomes editable and the result is branched.

```ts
// queue ops over queueRef.current ({ id, message }[])
function promote(id)   { const q=queueRef.current; const i=q.findIndex(m=>m.id===id);
                         if (i>0) q.unshift(...q.splice(i,1)); }
function move(id, d)   { /* swap with neighbour */ }
function removeQueued(id) { queueRef.current = queueRef.current.filter(m=>m.id!==id);
                            removeTurn(id); }                    // drop from transcript too

const [cancelling, setCancelling] = useState(false);
function interrupt(queuedId, mode) {
  const running = turns.find(t => t.pending && !t.queued);
  if (!running || cancelling) return;
  promote(queuedId);
  setCancelling(true);
  void tauri.cancelTurn(running.id, mode);   // runTurn resolves with stop; pump continues
}

// in runTurn, branch on result.stop:
//   "steered"     → steerTurn(id);  refresh panels (work was committed)
//   "redirected"  → redirectTurn(id, message);  no panel refresh (reverted)
//   "completed"   → completeTurn(...) as today
// clear `cancelling` whenever a turn settles.

async function newConversation() {
  const running = turns.find(t => t.pending && !t.queued);
  if (running) await tauri.cancelTurn(running.id, "redirect");
  queueRef.current = [];
  useChatStore.getState().newConversation();   // new sessionId + clear turns
}
```

Resume on a redirected turn = `resetTurn(id)` → enqueue `{ id, message: redirected.body }` → `pump()`.

### 11. `TurnView`

- **Queued**: `Queued · {place}` + `Steer` / `Redirect` (shown only when a turn is
  running; disabled while `cancelling`) + `↑`/`↓` + `✕`. `place` and the
  running/cancelling flags come from `ChatPane`.
- **Steered**: a normal completed turn (receipt + undo), with a small *Steered*
  badge so the partial reply reads as intentional.
- **Redirected**: a thin neutral tombstone — *"Redirected"* — with **Resume**.
- **Running**: unchanged (`Thinking…`).

### 12. New conversation control

A slim header above the transcript (or a button by the composer toolbar) calling
`newConversation()`; also wired into `CommandPalette` as "New conversation".

---

## Build order

- **M1 — cancel spine.** `TurnStop`, `TurnRequest.cancel` + `conversation_id`,
  `CancelRegistry` (with mode), `cancel_turn`, `chat_turn` register/deregister
  guard. Engines ignore the token (compiles, no behaviour change). Unit-test the
  registry incl. mode round-trip.
- **M2 — revert + branch.** Extract `audit::revert_to_snapshot`; add
  `MemoryStore::delete_chat_message`; add the Steer (commit) and Redirect
  (revert + delete chat row + no audit entry) branches to `chat_turn`. Tests:
  redirected turn → formation byte-identical to snapshot, facts gone, chat row
  gone, no audit entry; steered turn → audit entry present, partial reply kept.
- **M3 — Claude Code cancel.** select! cancel arm + `child.kill()`; accumulate the
  partial reply for Steer. Cancel mid-stream over the deterministic fixtures
  (`claude_code.rs:887`+) → `Interrupted` with the streamed-so-far reply.
- **M4 — Copilot cancel + probe.** `session_cancel_msg`, the defensive arm, the
  grace window, the kill+recycle fallback, and `conversation_id` recycle.
  **Live probe** (gated like existing live tests): does the installed CLI honour
  `session/cancel` and stay usable, or must we recycle? Record the answer here.
- **M5 — frontend.** Store states + `newConversation`, `tauri.ts`, pump branching,
  `interrupt`/`promote`/`move`/`removeQueued`/Resume, `TurnView` controls,
  New-conversation control + command-palette entry.
- **M6 — polish.** Cancel-in-flight race (finish-before-cancel resolves to the
  real stop), `cancelling` lockout, place-in-line badges, copy, biome/tsc/clippy.

Each milestone compiles and runs; M1–M2 are invisible, M3–M4 make each engine
interruptible, M5 wires the buttons.

---

## Testing

- **Registry:** register/cancel(mode)/taken_mode/finish; cancel of an unknown id
  is a no-op; double-finish safe; mode round-trips.
- **Redirect (M2):** formation byte-identical to the pre-turn snapshot;
  `facts_by_source` empty; chat row deleted; no audit entry.
- **Steer (M2):** audit entry written; changed notes + facts retained; partial
  reply persisted; turn is revertable via the normal `undo_turn`.
- **Claude Code (M3):** cancel during the captured `stream-json` transcript →
  `Interrupted`, partial reply = streamed-so-far, child reaped.
- **Copilot (M4):** live probe — `session/cancel` stops the prompt and the
  session survives; otherwise the recycle fallback fires and the next turn works.
- **New conversation (M4/M5):** new `session_id` ⇒ `recent_messages` empty for it;
  in-flight turn reverted; Copilot session recycled (fresh `session/new`).
- **Frontend:** finish-before-cancel resolves to the actual stop (no false
  "redirected"); promote/reorder/remove keep order; `cancelling` disables
  double-interrupt; Resume re-runs a redirected turn fresh.

---

## Open questions

1. **Copilot `session/cancel` honoured?** Unknown — M4's probe decides whether we
   ride the clean-cancel path or always kill+recycle. Plan works either way.
2. **Soft Steer variant?** This plan's Steer interrupts promptly and keeps the
   partial reply. An alternative "soft steer" would *not* interrupt — it lets the
   running turn finish and only jumps the message to the front of the queue (no
   partial reply, no kill). If the partial-reply artifact proves jarring in
   practice we can switch Steer to soft, or offer both. Recommend: ship prompt
   Steer; revisit if the partial replies read badly.
3. **Redirected tombstone vs. vanish.** Plan keeps a thin tombstone + Resume so
   the user remembers they redirected. Alternative: remove the turn entirely.
   Recommend: tombstone.
4. **Drag-reorder vs. ↑/↓.** Plan uses ↑/↓ + promote (simple, keyboard-friendly).
   Full drag-and-drop is a later polish if queues routinely get long.
