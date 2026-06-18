/**
 * DEV-ONLY browser mock for the Tauri command layer.
 *
 * Active ONLY when running the Vite dev server in a plain browser (no Tauri
 * runtime). In the real desktop app `__TAURI_INTERNALS__` is present, so this
 * never engages; in production builds `import.meta.env.DEV` is false, so it is
 * dead code and gets stripped. Its sole purpose is to let the redesigned UI be
 * previewed/screenshotted in a browser with representative data.
 */
export const isBrowserMock =
  import.meta.env.DEV && typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);

const NOW = Math.floor(Date.now() / 1000);

const NOTES = [
  { relative_path: "Daily Notes/2026-06-15.md", modified_secs: NOW - 30 },
  { relative_path: "Tasks.md", modified_secs: NOW - 7200 },
  { relative_path: "People/Keaton Vale.md", modified_secs: NOW - 20 },
  { relative_path: "People/Mara Lindqvist.md", modified_secs: NOW - 90_000 },
  { relative_path: "People/Theo Brandt.md", modified_secs: NOW - 200_000 },
  { relative_path: "Projects/Sediment.md", modified_secs: NOW - 4000 },
  { relative_path: "Projects/Lease decision.md", modified_secs: NOW - 1200 },
  { relative_path: "Organizations/Stripe.md", modified_secs: NOW - 60_000 },
];

const KEATON = `---
type: person
at: Stripe
city: Oakland
---

# Keaton Vale

## Work
- Joined [[Stripe]] as a **staff product designer** — June 2026 *(superseded: was at Figma)*
- Leads the design-systems guild; owns the tokens migration
- Wants to talk through [[Sediment]]'s working-set model

## Threads
- Owes me a PM intro for the [[Lease decision]] referral
- Said she'd send the staff-design rubric — not yet received
`;

const DAILY = `---
daily note: 2026-06-15
week: 2026-W24
---

# Sunday, June 15

## Checklist
- [x] Morning pages
- [x] Read 20 pages
- [ ] Workout
- [ ] Inbox to zero

## Did
- Lunch with [[Keaton Vale]] — she joined [[Stripe]] as staff designer
- Reviewed [[Lease decision]] options again — still blocked on the PM intro
- Completed task **Send Q2 numbers**

## Notes
- Kept circling the lease without deciding. The blocker is the intro, not the options.
`;

const LEASE = `---
type: project
status: deciding
due: Fri Jun 20
---

# Lease decision

## Options
- Renew current space — $4,200/mo, 24-month term
- Relocate near [[Stripe]] — pending [[Keaton Vale]]'s PM intro

## Open questions
- Need the PM intro before committing *(blocking)*

## Decision
- Land by Friday — nudge set for Thursday
`;

function noteBody(path: string): string {
  if (path.includes("Keaton")) return KEATON;
  if (path.includes("Daily Notes")) return DAILY;
  if (path.includes("Lease")) return LEASE;
  if (path === "Tasks.md")
    return "# Tasks\n\n- [ ] Decide on lease renewal\n- [ ] Send rubric to Keaton\n";
  return `# ${path.replace(/^.*\//, "").replace(/\.md$/, "")}\n\n## Notes\n- Placeholder note.\n`;
}

const WORKING_SET = {
  activeEntities: [
    { name: "Keaton Vale", entityType: "person", notePath: "People/Keaton Vale.md" },
    { name: "Stripe", entityType: "organization", notePath: "Organizations/Stripe.md" },
    { name: "Lease decision", entityType: "project", notePath: "Projects/Lease decision.md" },
  ],
  recentNotes: ["People/Keaton Vale.md", "Daily Notes/2026-06-15.md"],
  openTasks: [{ title: "Decide on lease renewal", due: "2026-06-20" }],
  openLoops: [
    {
      id: "open_loop:1",
      title: "You said you'd pick a conversation engine",
      context: "Claude Code vs Copilot — still unset in Settings",
    },
  ],
};

// Due dates are derived from the wall clock so the grouped view (Overdue /
// Today / Upcoming / Someday) always has a representative spread when previewed.
const DAY = 86_400_000;
function isoOffset(days: number, hour = 17): string {
  const d = new Date(Date.now() + days * DAY);
  d.setHours(hour, 0, 0, 0);
  return d.toISOString();
}

const TASKS = [
  {
    id: "task:overdue",
    title: "Reply to the landlord about the lease",
    status: "open",
    due: isoOffset(-2),
    remind_at: isoOffset(-2, 9),
    notified: true,
    created: "2026-06-08T12:00:00Z",
    completed_at: null,
    source_chat_id: null,
  },
  {
    id: "task:today",
    title: "Send staff-design rubric to Keaton",
    status: "open",
    due: isoOffset(0),
    remind_at: isoOffset(0, 9),
    notified: false,
    created: "2026-06-15T12:00:00Z",
    completed_at: null,
    source_chat_id: null,
  },
  {
    id: "task:soon",
    title: "Decide on lease renewal",
    status: "open",
    due: isoOffset(3),
    remind_at: isoOffset(2, 16),
    notified: false,
    created: "2026-06-10T12:00:00Z",
    completed_at: null,
    source_chat_id: null,
  },
  {
    id: "task:someday",
    title: "Read the working-set paper Keaton mentioned",
    status: "open",
    due: null,
    remind_at: null,
    notified: false,
    created: "2026-06-14T12:00:00Z",
    completed_at: null,
    source_chat_id: null,
  },
  {
    id: "task:done",
    title: "Send Q2 numbers to the team",
    status: "done",
    due: isoOffset(-1),
    remind_at: null,
    notified: true,
    created: "2026-06-12T12:00:00Z",
    completed_at: isoOffset(-1, 14),
    source_chat_id: null,
  },
];

const RESPONSES: Record<string, unknown> = {
  app_version: "0.1.0-dev",
  get_onboarding_state: { complete: true },
  restore_last_formation: { path: "/Users/you/Notes/Field Notes", note_count: NOTES.length },
  ollama_ensure_running: { installed: true, running: true, install_hint: null },
  ollama_status: { installed: true, running: true, install_hint: null },
  check_model_readiness: {
    ollama_installed: true,
    all_present: true,
    requirements: [
      { kind: "embed", id: "all-minilm", label: "Embeddings", size_hint: "45 MB", present: true },
    ],
  },
  list_notes: NOTES,
  get_working_set: WORKING_SET,
  get_self_summary: "- Prefers async over meetings\n- Shipping Sediment V1 by August",
  list_copilot_models: {
    available: [
      {
        modelId: "auto",
        name: "Auto",
        description: "Let Copilot pick the best model",
        usage: null,
        enabled: true,
      },
      { modelId: "gpt-5-mini", name: "GPT-5 mini", description: null, usage: "0x", enabled: true },
      {
        modelId: "claude-haiku-4.5",
        name: "Claude Haiku 4.5",
        description: null,
        usage: "0.33x",
        enabled: true,
      },
    ],
    currentModelId: "gpt-5-mini",
  },
  list_audit: [],
  list_tasks: TASKS,
  get_models_dir: "/Users/you/.sediment/models",
  get_embedding_provider: "ollama",
  set_embedding_provider: null,
  warmup_embedding_model: null,
  get_conversation_engine: {
    engine: "claude-code",
    claude_code_model: "sonnet",
    copilot_model: null,
  },
  detect_claude_code: {
    installed: true,
    binary_path: "/usr/local/bin/claude",
    logged_in: true,
    auth_method: "claude.ai",
    subscription_type: "max",
    email: "you@example.com",
  },
  detect_copilot: { installed: false, binary_path: null },
  index_formation: { total: NOTES.length, indexed: NOTES.length, skipped: 0, failed: 0 },
  write_note: null,
  dismiss_open_loop: null,
  complete_task: null,
  snooze_task: null,
  set_conversation_engine: null,
  set_models_dir: null,
  complete_onboarding: null,
};

export function browserMock<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (cmd === "read_note") {
    const path = (args?.relativePath as string) ?? "";
    return Promise.resolve(noteBody(path) as unknown as T);
  }
  if (cmd === "chat_turn") {
    return Promise.resolve({
      turnId: "turn-dev",
      reply: "Updated [[People/Keaton Vale.md]] and logged the lunch to today's note.",
      changedNotes: [{ path: "People/Keaton Vale.md", wasCreate: false }],
      recordedFactCount: 2,
      workingSet: WORKING_SET,
      stop: "completed",
    } as unknown as T);
  }
  if (cmd === "cancel_turn") {
    return Promise.resolve(undefined as unknown as T);
  }
  if (cmd in RESPONSES) {
    return Promise.resolve(RESPONSES[cmd] as T);
  }
  return Promise.resolve(null as unknown as T);
}
