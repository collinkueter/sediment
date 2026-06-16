# Redesign implementation — "Strata" (conversation-as-hero)

Translate the approved mockup `docs/design/sediment-redesign-hero.html` into the real
React + Tauri app, preserving every store/`tauri` call while replacing the cold zinc
chrome with the warm, editorial **Paper / Strata** design.

## Design system (the contract every component builds against)

**Fonts (bundled via @fontsource, offline-first):**
- `font-serif` → Fraunces Variable — agent voice, note titles, reading prose, wordmark
- `font-sans` → Hanken Grotesk Variable — all UI chrome (default body)
- `font-mono` → IBM Plex Mono — timestamps, the fact-trail, keycaps

**Themeable color tokens** (defined on `:root[data-theme="paper"|"strata"]`, exposed as
Tailwind utilities via `@theme inline`): `bg`, `bg-sunk`, `surface`, `raised`, `ink`,
`ink-soft`, `muted`, `faint`, `line`, `line-strong`, `accent`, `accent-ink`,
`accent-tint`, `gold`, `gold-tint`, `sage`, `sage-tint`, `user-bg`, `user-ink`.
→ usage: `bg-surface text-ink border-line text-accent` etc.

**Dark variant:** `@custom-variant dark (&:where([data-theme="strata"], [data-theme="strata"] *))`
so existing `dark:` utilities follow the manual theme.

**Theme store** (`src/lib/theme.ts`): `theme: 'paper'|'strata'`, init from
`prefers-color-scheme`, persist to localStorage, write `data-theme` on `<html>`.
Toggle lives in the title bar (segmented Paper/Strata) and Settings.

**Icons** (`src/components/icons.tsx`): one feather-style SVG set, replacing every emoji
(🔔 ⚙ ↳ × ✅ 💬 …). Stroke = `currentColor`.

## Layout (App.tsx)

```
TitleBar  ── traffic-light safe area · ⛰ wordmark · breadcrumb · theme toggle · search · tasks · settings
─────────────────────────────────────────────────────────────────────────────
Formation nav (resizable)  │  Conversation HERO (flex)         │  Note pane (resizable, collapsible)
  formation switch         │   In-focus bar (entities/tasks)   │   header · Read/Source
  ⌘K search                │   centered transcript             │   prose · frontmatter tags
  grouped tree             │   trace + inline receipt          │   daily-note checklist
                           │   composer                        │   backlinks slot (deferred data)
```
- Resizable dividers (pointer-drag, widths persisted to localStorage).
- **Focus mode** collapses the note pane (button in note header + ⌘\\).
- Overlays: CommandPalette (⌘K), RemindersPopover (bell), SettingsModal (gear). Esc closes.
- `FormationPicker` still shown full-screen when no formation is open.

## Undo model
- **Chat-turn** changes → **inline receipt** rendered in the completed turn
  (`✓ Recorded N facts · updated <note> · Undo`), calling `auditStore.undoTurn(turnId)`.
- **Task-completion** changes (from `daily-note-appended`) → keep `UndoToast`, restyled.

## Workstreams

**Foundation (main agent, sequential — the stable contract):**
1. Install `@fontsource-variable/fraunces`, `@fontsource-variable/hanken-grotesk`,
   `@fontsource/ibm-plex-mono`.
2. `globals.css`: font imports, token layer, `@theme inline`, dark variant, base styles,
   full `.note-prose` re-skin to warm tokens, daily-note checklist styling, frontmatter-tag
   + backlinks classes.
3. `src/lib/theme.ts` theme store; init in `main.tsx`.
4. `src/components/icons.tsx` icon set.
5. `App.tsx` shell: TitleBar, 3-col resizable layout, focus mode, overlay wiring.
   Create stub `InFocusBar` / `CommandPalette` so the app compiles before agents fill them.

**Parallel sub-agents (distinct files, build against the contract above):**
- A. `FileTree.tsx` — formation nav: grouped sections, type icons, Pinned (Today + Tasks),
  counts, active state with accent bar.
- B. `ChatPane.tsx` — conversation hero: day marks, user bubble, agent serif reply, fact
  trace, **inline receipt**, generous composer, inline retry.
- C. `InFocusBar.tsx` (replaces WorkingSetPanel) — horizontal entity chips + task/loop pills
  + "N more", driven by `useWorkingSetStore`.
- D. `NoteViewer.tsx` + `NotePreview.tsx` — note header, Read/Source segmented control,
  client-side frontmatter tag chips, entity-type avatar, daily-note checklist, backlinks slot.
- E. `SettingsModal.tsx` — engine-picker cards (Claude Code / Copilot with detection status),
  appearance theme toggle, formation + models rows.
- F. `CommandPalette.tsx` (new, ⌘K) + `RemindersPopover.tsx` + `UndoToast.tsx`
  + `ReminderToast.tsx` + `IndexProgress.tsx` — overlays & toasts.
- G. Secondary full-screens — `Onboarding.tsx`, `ModelSetup.tsx`, `FormationPicker.tsx`,
  `AuditLog.tsx` — re-skin to the warm palette for consistency.

**Integration (main agent):** `npm run typecheck` + `npm run lint` + `npm run build` green;
launch dev server; screenshot Paper & Strata; compare against the mockup; polish.

## Out of scope / deferred
- Real **backlinks** data (needs a `get_backlinks` Tauri command) — render the slot only
  when data exists; no fake content.
- CodeMirror Source-mode gets a light warm theme; deep token highlighting deferred.
