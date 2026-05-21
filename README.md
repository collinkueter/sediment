# Sediment

Local-first AI note-taking with a temporal knowledge graph. You chat; Sediment extracts facts, files them into your Obsidian-compatible formation, and stages every change for review before it touches disk.

See [sediment-tech-spec.md](sediment-tech-spec.md) (v0.2) for the full design.

## Status

**Phase 1–5 complete.** The desktop shell launches, opens a formation, watches it for external edits, and auto-indexes notes into an embedded SurrealDB. Chat is intent-classified. Write-mode messages run through an **LLM-backed extraction** pipeline (ADR-0006) — the active tier's local model resolves the note-taker ("I" / "we"), coreference, and tense into a structured `Extraction`, with the deterministic gline-rs NER+RE extractor kept as a zero-dependency fallback. Extracted facts are routed to their subject's note, rendered as deterministic markdown diffs, and parked in a **staging tray** for review (spec principle #3, "AI proposes, human disposes"). Keep commits a change — snapshotting the note, writing the markdown, upserting entities, and writing bi-temporal facts — with a 10-second undo. Facts that contradict an existing one surface a side-by-side conflict banner. Ask-mode questions run hybrid retrieval (vector + graph) and stream a cited answer. A launch-time setup screen downloads any models the active hardware tier is missing. Phase 5 is a polish pass — extraction robustness (lenient JSON recovery, case-insensitive relation binding) and broader end-to-end test coverage.

See [docs/plans/](docs/plans/) for the per-phase milestone breakdowns and [docs/adr/](docs/adr/) for the architecture decisions.

## Prerequisites

- **macOS** (Apple Silicon recommended). Phase 1 is macOS-only; Linux/Windows come post-V1.
- **Rust** ≥ 1.88 — `rustup update stable`
- **Node** ≥ 22 + npm
- **Xcode Command Line Tools** — `xcode-select --install`
- **Ollama** — install from [ollama.com/download](https://ollama.com/download). The app will spawn `ollama serve` for you on first launch.

## Quickstart

```bash
git clone <repo> sediment && cd sediment
npm install
npm run tauri dev
```

The first build downloads + compiles a large Rust dep tree (Tauri, SurrealDB, ONNX Runtime, gline-rs) — expect ~5-10 min cold. Subsequent builds are incremental.

Pull the two models the app expects when you first launch:

```bash
ollama pull llama3.2:3b           # chat model (M6)
ollama pull nomic-embed-text      # embedding model (M7)
```

## Optional: GLiNER extraction models (Phase 2)

Deterministic entity + relation extraction uses [`gline-rs`](https://github.com/fbilhaut/gline-rs) loading the [multitask GLiNER ONNX model](https://huggingface.co/onnx-community/gliner-multitask-large-v0.5). Files are ~500MB and are not bundled. Open your formation, then:

```bash
cd <formation>/.chat-notes/models
mkdir -p gliner-multitask-large-v0.5/onnx
cd gliner-multitask-large-v0.5
curl -L -o tokenizer.json   https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/tokenizer.json
curl -L -o onnx/model.onnx  https://huggingface.co/onnx-community/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx
```

Or use the helper script: `docs/scripts/download-gliner.sh <formation-path>`.

Without these files, Write-mode chat returns a bootstrap hint and Ask-mode falls back to vector-only retrieval. Everything else still works.

## Phase 2 verification

Once a formation is open and both Ollama models are pulled:

1. **Background indexing.** Drop a few `.md` files into the formation folder. On the next launch (or open) the title bar shows an `indexing N/M` bar; when it clears, the notes are embedded into SurrealDB.
2. **Write a fact.** With the GLiNER model installed, type a declarative sentence in chat — e.g. `Sarah is the CTO at Acme.` The classifier shows `auto` → Write; on send the assistant bubble reports the entities and facts filed.
3. **Supersession.** Send `Sarah moved to Beta Corp.` The new `works_at` fact closes out the Acme one (history preserved — verify with a point-in-time query).
4. **Ask with citation.** Type a question — `Where does Sarah work?` The classifier flips to Ask; the answer streams in with `[[note path]]` citations rendered as clickable buttons.
5. **External edit.** Edit a formation file outside Sediment; the watcher re-indexes it within ~2s.

The storage half of the pipeline is covered by `cargo test` (46 tests, no models needed). The model-dependent extraction path has `#[ignore]`d tests — run them with the model present via `cargo test -- --ignored`.

## Phase 3 verification: the staging tray

Phase 3 inserts a review layer between extraction and the formation — no fact reaches a note or the graph without an explicit Keep. With a formation open and the GLiNER model installed:

1. **Stage.** Send a Write-mode message — `Bill Gates founded Microsoft in 1975.` Nothing is written to disk yet; a JSON entry appears under `.chat-notes/staging/` and the **staging tray** at the bottom of the window expands, showing the affected note (`➕ People/Bill Gates.md · +1 fact`).
2. **Review.** Click `view` on a staged row. The note opens in the left pane as a unified diff — green insertion gutter, per-chunk accept/reject controls. The `## Facts` section and `chat-notes` frontmatter are the only managed regions; user prose is never touched.
3. **Keep.** Click `Keep` (whole entry) or `keep` (one note). The note file gains the `## Facts` bullet, SurrealDB gets the entity + bi-temporal fact, the staging entry clears, and an **Undo** toast shows for 10 seconds.
4. **Undo.** Click Undo within the window — the note reverts to its pre-commit snapshot, the written facts are deleted, and the entry returns to the tray.
5. **Discard.** `✗` on a row, or `Discard all` on an entry, removes it with zero effect on disk or graph.
6. **Conflict.** Send a contradicting fact — `Bill Gates works at Berkshire.` after a prior `works_at` — and the row shows a ⚠ banner: `Update` (supersede), `Keep both` (concurrent — skips supersession), `Discard new`.
7. **Temporal.** A single date in the message (`...in 1975`) stamps the fact's `valid_from` to that year instead of "now"; the bullet renders `- Founded Microsoft (1975)`.

The staging, routing, diff-generation, commit, and conflict logic are all covered by `cargo test` with no models needed; the integration test in `commands/staging.rs` drives a hand-built `StagingEntry` through stage → keep → verify and stage → discard → no-op.

## Phase 4 verification: LLM-backed extraction

Phase 4 (ADR-0006) replaces the GLiNER NER+RE extractor with the tier's local chat model behind a `FactExtractor` trait — recovering first-person facts, coreference, tense, tasks, and opinions that a zero-shot NER model structurally cannot express. GLiNER stays as the fallback.

1. **Model setup.** On launch the app checks the active tier's model manifest; if anything is missing a one-click setup screen streams the downloads (Ollama chat + embedding models, the GLiNER ONNX model).
2. **Self + coreference.** Send a first-person Write message — `Standup with the platform team today. Josh mentioned he worked at Cloudflare back in 2019.` The note-taker resolves to `People/Me.md`, `he` binds to Josh, and the Cloudflare fact is filed **past-tense** as a closed interval.
3. **Fallback.** With the chat model absent (or Ollama down) the pipeline drops to the GLiNER extractor instead of failing the turn.

Extraction is split into two test layers (ADR-0006): the deterministic `ScriptedExtractor` pipeline tests in `commands/chat.rs` are the CI gate (`cargo test`, no models); the live `LlmExtractor` recall test is `#[ignore]`d and scores against the real model.

## Project layout

```
sediment/
├── src-tauri/                       # Rust core
│   ├── src/
│   │   ├── lib.rs                   # Bootstrap + command registration
│   │   ├── error.rs                 # AppError + AppResult
│   │   ├── commands/                # #[tauri::command] handlers
│   │   │   ├── formation.rs         # Open / list / read / write notes
│   │   │   ├── memory.rs            # SurrealDB smoke + index + search
│   │   │   ├── ollama.rs            # Status / list / generate streaming
│   │   │   ├── extraction.rs        # GLiNER NER/RE (when models present)
│   │   │   ├── chat.rs              # Write (→ staging) / Ask / classify
│   │   │   ├── staging.rs           # Tray: list / keep / undo / resolve
│   │   │   ├── hardware.rs          # Tier detection + onboarding state
│   │   │   └── models.rs            # Model readiness check + downloaders
│   │   └── core/                    # Long-lived subsystems
│   │       ├── formation_state.rs   # Active formation + AppConfig
│   │       ├── watcher.rs           # Debounced notify watcher
│   │       ├── memory.rs            # Embedded SurrealDB store
│   │       ├── ollama_sidecar.rs    # Daemon lifecycle + client
│   │       ├── extraction.rs        # FactExtractor trait + GlinerExtractor
│   │       ├── llm_extractor.rs     # LlmExtractor — LLM-backed extraction
│   │       ├── models.rs            # Tier → local model manifest
│   │       ├── staging.rs           # StagingEntry model + snapshots
│   │       ├── router.rs            # Fact → note routing
│   │       ├── diff_gen.rs          # Template markdown diff generation
│   │       └── hardware.rs          # RAM / chip / tier scoring
├── src/                             # React + TS
│   ├── App.tsx                      # Title bar + 3-pane layout
│   ├── components/                  # NoteViewer, ChatPane, FileTree, etc.
│   ├── lib/
│   │   ├── tauri.ts                 # Typed invoke wrappers
│   │   ├── store.ts                 # Zustand stores (chat/UI/formation)
│   │   └── codemirror/setup.ts      # CM6 extensions
│   └── styles/globals.css           # Tailwind v4 entry
├── docs/adr/                        # Architecture Decision Records
├── sediment-tech-spec.md            # Spec (v0.2)
└── .claude/launch.json              # Dev server config for Claude Preview
```

## Common scripts

```bash
npm run tauri dev      # Launch the desktop app with hot reload
npm run dev            # Just the Vite frontend (useful for UI-only iteration)
npm run build          # Type-check + produce a production Vite bundle
npm run lint           # Biome lint
npm run typecheck      # tsc --noEmit

cd src-tauri
cargo check            # Fast Rust compile-check
cargo test             # All unit tests (gline-rs round-trip is `#[ignore]` by default)
cargo clippy --all-targets -- -D warnings
```

## Troubleshooting

**`cargo check` fails on `arrow-arith` / chrono trait conflict** — the lockfile pins `chrono = 0.4.41` to dodge a `quarter()` trait clash in newer chrono. If your fresh clone tries a newer version, run `cargo update -p chrono --precise 0.4.41`.

**`ollama serve` doesn't auto-start** — confirm `ollama` is on PATH (`which ollama`). The app calls `Command::new("ollama").arg("serve")` and disowns the child. If the daemon refuses to start, run `ollama serve` manually in a terminal and the app will detect it on next launch.

**Vite port 1420 already in use** — another stray dev server is bound. `lsof -ti:1420 | xargs kill -9` clears it.

**Chat returns an "Ollama error"** — pull the model: `ollama pull llama3.2:3b`. The chat hardcodes that model in Phase 1; M5+ will let you pick.

## License

TBD — repo is private until the license decision is made (see spec §14).
