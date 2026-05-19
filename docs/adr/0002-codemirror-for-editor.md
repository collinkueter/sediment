# ADR-0002: CodeMirror 6 as the note editor

**Status:** Accepted (2026-05-19)

## Context

The spec's signature interaction (§9) is *inline diff staging*: when the AI proposes edits, the affected note in the left pane shows green/red gutters and the user keeps or discards changes line by line. That puts hard requirements on the editor:

1. Render markdown reasonably (syntax highlighting, optional live preview)
2. Support inline diff decorations programmatically
3. Be embeddable in a React + TS app
4. Stay performant on multi-megabyte notes

We considered:

- **Monaco** — comes with VS Code's editor power but a large bundle, heavier than we need.
- **TipTap / Lexical** — rich-text-first, would fight us when round-tripping plain Markdown.
- **Plain `<textarea>`** — fine for M2 but blocks the diff UX entirely.
- **Custom canvas** — too much work for the scope.

## Decision

Use **CodeMirror 6** via [`@uiw/react-codemirror`](https://github.com/uiwjs/react-codemirror), with `@codemirror/lang-markdown` and `@codemirror/theme-one-dark`. CM6's modular extension system covers diff decorations (Phase 3) cleanly.

## Consequences

- **Positive**
  - First-class diff/decoration support means M3's editor and Phase 3's staging diffs share one host component.
  - Smaller bundle than Monaco; the Vite production build with CodeMirror sits around 800 KB gzipped — acceptable.
  - The React wrapper is thin, so we can drop down to bare CM6 when needed.

- **Negative**
  - The default `useState` + onChange pattern in `@uiw/react-codemirror` re-renders on every keystroke. If profiling shows it's a bottleneck on long notes, switch to a CM6 `ViewPlugin` and skip React's state on hot paths.
  - Light/dark theme tracking is manual (`prefers-color-scheme` listener) because CM6's theme is an extension, not a prop. `NoteViewer` owns the subscription.
