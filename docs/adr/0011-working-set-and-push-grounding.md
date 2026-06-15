# ADR-0011: The Working Set, push-grounded turns, and in-reply proactivity

**Status:** Proposed (2026-06-15) — designed through a structured grilling session;
domain terms captured in [CONTEXT.md](../../CONTEXT.md) (**Working Set**, **Open
Loop**); the speed decision is gated by the auth spike recorded below.
**Amended by:** [ADR-0012](0012-github-copilot-engine.md) — §6: the GitHub Copilot
engine supplies the warm + subscription path the spike found missing for Claude.
**Amends:** [ADR-0009](0009-conversational-agent.md) — reverses framing decision #4
*for retrieval and identity only* (§2); replaces §5's pull-based grounding with a
deterministic push (§2, §3); investigates promoting §5's "persistent-per-session
later optimisation" to V1 and finds it **blocked** (§6 + auth spike); narrows the
deferred "autonomous organise/connect pass" into bounded in-reply proactivity
(§4, §5).
**Relates to:** [ADR-0007](0007-tasks-and-reminders.md) (an **Open Loop** is
distinct from a **Task**), [ADR-0008](0008-claude-code-answer-engine.md) (the
warm-session question), [ADR-0010](0010-daily-logs-and-recurring.md).
**Keeps:** ADR-0009's single conversation (no Write/Ask modes), the formation and
graph stores, the snapshot/audit/undo machinery.

## Context

ADR-0009 made chat a single conversational agent loop. In real use it falls short
of the product's stated goal — *a personal assistant you talk to that organizes
what you tell it, catches inconsistencies, brings up connections, and reminds you
of related things.* Four gaps, all reported from daily use:

- **Inert.** The agent only reacts to the current message; it never surfaces a
  connection, reminder, or related note on its own. Three of the four desired
  behaviours (connect, remind, catch-across-time) are *proactive*, and the
  architecture is purely reactive (ADR-0009/0010 defer all proactivity).
- **Unreliable grounding.** Grounding is the agent's job to *pull* (ADR-0009 §5;
  `conversation.rs`). A fresh `claude` process optimising to finish the turn skips
  it — so it duplicates/fragments notes (`Josh.md` vs `People/Josh.md`) and misses
  contradictions.
- **Slow capture.** Every input pays a full agentic turn (~6s cold spawn,
  ADR-0008, plus round-trips).
- **No durable model of the user.** Each turn re-derives the user from a cold
  start; the only long-term memory is the formation + graph, queried on demand.

**Root cause:** the assistant has no persistent working model and does all of its
thinking *synchronously, inside the user's message*, leaving grounding to a coding
agent's mid-task discretion. The four symptoms are one disease.

The interaction shape is **kept** — one conversation, no capture/chat/reflect
split, no new surfaces (a second review surface would re-grow the staging tray
ADR-0009 deleted). What changes is everything *around* the agent.

## Decision

### 1. Keep the single conversation; fix the turn, not the shape

No modes, no inbox, no background daemon. ADR-0009 §2 stands. The work is to make
one turn reliable, contextual, and fast.

### 2. Push, don't pull — a deterministic pre-pass grounds every turn

Before the agent runs, the `chat_turn` orchestrator (Rust) runs grounding tools
itself and **injects the results** into the agent's context, rather than leaving
the agent to fetch them:

| Push | Backed by | Fixes |
|---|---|---|
| **Resolved entity identity** — names in the message → canonical note paths | `find_entity` + embeddings | duplicate/fragmented notes |
| **Pre-fetched related notes** — top-k semantic hits for the message + recent window | `search_notes` | missed connections |
| **The Working Set** (§3) | derived (see §3) | the amnesia |
| **Contradiction candidates** — when a relationship-fact is in play | `find_contradiction` | missed inconsistencies |

This **amends ADR-0009 framing decision #4** (*"rely on AI; retire deterministic
components"*): deterministic scaffolding returns for **retrieval and identity
resolution**. It does *not* return for extraction — GLiNER stays retired; the
agent keeps all generation and judgment. The agent may still pull more with its
own tools; the pre-pass only guarantees a reliable floor.

The tools already exist in `core/formation_tools.rs`. The change is *who pulls the
trigger* — a deterministic pre-pass, not agent discretion.

### 3. The Working Set — a derived view of what is in play

The **Working Set** is the durable model of the user that cures the amnesia. It is
**deterministically derived** each turn — never authored by the agent — from
signals already in the system:

- recently-touched notes (file `mtime`),
- recently-mentioned entities (`chat_message` + `facts_by_source`),
- open tasks (the `task` table, ADR-0007),
- recently-changed Facts,
- open **Open Loops** (§5).

It is recomputed each turn (cheap *because* it is a view, not stored state),
pushed into the agent, and rendered in a UI panel ("what's in play"). The agent
**reads** it; it never writes it. It is not a **Note** and not authoritative — a
peer view, the way the knowledge graph is a peer store.

**Honest limit:** a derived set knows what the user *touched*, not what *matters*.
"Recently active: Sarah, Q2 Planning, 3 open tasks" is mechanical and free. "You
said you'd pick a vendor and never logged the decision" is semantic and invisible
to recency — which is why §5 exists.

### 4. Proactivity lives inside the reply

Each turn **may** end with **at most one** proactive surfacing — a connection from
the pre-fetched related notes, or an **Open Loop** — disciplined exactly like the
existing one-clarifying-question-per-turn rule (`prompts/conversation-agent.md`),
with a per-item cooldown so the same item cannot nag twice in a row. This is
deliberately *narrower* than ADR-0009's deferred "autonomous background
organise/connect pass": it needs no daemon and no second surface, and it only
fires on turns the user is already having.

### 5. Open Loops are captured as records, surfaced deterministically

An **Open Loop** — an unresolved question or a stated-but-unfulfilled intention —
is recorded the moment it arises (a `record_open_loop` graph tool), when context
is richest, so it can be surfaced later without re-deriving it by semantic scan.
This is the same capture-once-surface-deterministically move as the rest of the
ADR.

An Open Loop closes **three ways**, because agent-detection alone is not
trustworthy: (a) the agent closes it when it clearly hears the resolution; (b) the
in-reply rider always offers a one-tap dismiss — the user is the authority; (c)
time-decay demotes an untouched loop after a window so the list self-cleans.

Distinct from a **Task** (ADR-0007): a Task is a scheduled action with a due time
and a notification; an Open Loop is a soft, agent-noticed thread nudged only in
conversation, never alarmed. The agent picks by whether the thing is a *scheduled
action* (Task) or a *pending resolution* (Open Loop).

### 6. Speed — cold-spawn now, warm later (gated by the auth spike)

The intended decision was "warm the agent, keep the subscription." The auth spike
(below) found **no supported path** that is both warm and subscription-authed
today — *for the Claude Code engine.* (**Amended by ADR-0012:** warm + subscription
*is* reachable, just not with Claude — the GitHub Copilot engine's resident
ACP/SDK mode is the warm path. The cold-spawn + mask decision below stands for the
Claude Code engine specifically.) Therefore:

- **V1 keeps cold-spawn-subscription.** It works today and preserves "no second
  bill." The ~6s start is **masked** with optimistic UI (the captured text and the
  tool-activity trail appear instantly; the reply catches up) and **structurally
  reduced** by §2 (push-retrieval means fewer agent-initiated round-trips).
- **Warm-subscription is a prototype, not a commitment.** The one unproven
  candidate — a *resident* `claude -p --input-format stream-json --output-format
  stream-json` process fed successive turns over stdin — is tracked as a spike
  (Open question 1), not built.
- **The API engine is the documented escape hatch.** If warming proves impossible
  and latency is unacceptable, an HTTP tool-use loop (ADR-0009's deferred API
  engine) trades the subscription for speed + control. Offered in settings, not
  forced.

## Auth spike (2026-06-15)

Run against the local install — `claude` **2.1.162**, `claude auth status --json`
→ `authMethod: "claude.ai"`, `subscriptionType: "max"`, `apiProvider:
"firstParty"`. Findings, with confidence stated:

- **(High) `--continue` / `--resume` do not reduce cold-start.** They restore
  conversational context but still spawn a fresh Node process each invocation.
  Irrelevant to warming (Sediment already owns the transcript, ADR-0009 §5).
- **(High) The Agent SDK / API path is API-key only.** The `--bare` help is
  explicit: *"Anthropic auth is strictly ANTHROPIC_API_KEY or apiKeyHelper … OAuth
  and keychain are never read."* The Claude Agent SDK authenticates with
  `ANTHROPIC_API_KEY` (per-token billing), not the claude.ai subscription. A warm
  *in-process* SDK loop therefore **loses** the subscription.
- **(Medium) A resident CLI process via `--input-format stream-json` is the only
  warm-and-subscription candidate.** The flag exists ("realtime streaming input,"
  print-mode only) and is the mechanism for feeding multiple messages to one
  `claude` process. Whether a single process genuinely stays resident across turns
  under subscription auth — and how Sediment injects a freshly-derived Working Set
  per message when `--system-prompt` is fixed at spawn — is **unproven**. Needs a
  hands-on prototype (Open question 1).
- **(Low — verify against primary sources) Possible policy restriction.** Research
  surfaced reports (Feb 2026) that Anthropic restricts claude.ai *subscription*
  OAuth in third-party apps. It is **unclear** whether this applies to Sediment's
  model — the user installs and authenticates *their own* `claude` CLI and
  Sediment shells out to that local binary (ADR-0008), which is different from an
  app embedding claude.ai login. This must be checked against Anthropic's actual
  commercial terms / usage policy, because if it applies it threatens the entire
  subscription-engine premise of ADR-0008/0009, not merely warming (Open
  question 2).

**Net:** warm × subscription has no off-the-shelf answer. V1 ships cold-spawn +
masking; warming and the API fallback are tracked, not built.

## Consequences

- **Positive** — all four gaps are addressed inside the single conversation, with
  no new surface. Reuses the existing graph tools and stores. The agent does *less*
  per turn (context is pushed), so the loop is structurally shorter.
- **Positive** — reliability stops depending on a coding agent's mid-task whim;
  identity and contradiction checks become deterministic floors.
- **Negative** — partially reverses ADR-0009 #4: deterministic scaffolding returns
  (retrieval + identity). Accepted: the pivot over-corrected by leaving grounding
  entirely to the agent.
- **Negative** — per-turn token/quota cost grows; the pushes inflate the prompt on
  a subscription that is quota-metered. Needs a per-turn **context budget** with
  ranked truncation (Open question 3).
- **Negative** — the Working Set's "what matters" is only as good as the recency
  heuristics plus Open-Loop capture precision; both are tunable, neither is
  guaranteed.
- **Negative** — the speed win is partial: warming is blocked, so V1 *masks*
  latency rather than eliminating it.
- **Neutral** — *which note* a fact lands in is now deterministic (§2), but a
  note's *internal section structure* stays prompt-driven; "automatically
  categorizes" is reliable at the file level, tunable-not-guaranteed at the
  section level.
- **Out of scope** — the background daemon, the new-day/app-open catch-up turn,
  and the on-demand "what am I forgetting" command (all considered and declined in
  favour of the in-reply rider); the warm-session implementation (spike-gated);
  the API engine implementation.

## Open questions

1. **Warm-subscription prototype.** Does a resident `claude -p --input-format
   stream-json --output-format stream-json` process stay alive across turns under
   subscription auth, and can Sediment inject a fresh Working Set per message
   (since `--system-prompt` is fixed at spawn — likely via a per-message context
   block rather than the system prompt)? Prototype before committing any warming
   work.
2. **Subscription policy.** Verify against Anthropic's primary commercial
   terms / usage policy whether the "user runs their own installed CLI, Sediment
   shells out to it" model is permitted for a distributed app. Gates ADR-0008/0009,
   not just §6.
3. **Per-turn context budget.** Ranking + truncation policy for the §2 pushes
   (Working Set first, then top-k related notes, drop the rest) so the prompt stays
   bounded.
4. **Open-loop capture precision.** Tune capture in the behaviour prompt; measure
   the false-positive nag rate and the decay window empirically.
