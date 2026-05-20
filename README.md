# Sediment

Local-first AI note-taking with a temporal knowledge graph. You chat; Sediment extracts facts, files them into your Obsidian-compatible formation, and stages every change for review before it touches disk.

See [sediment-tech-spec.md](sediment-tech-spec.md) (v0.2) for the full design.

## Status

**Phase 1 + Phase 2 complete.** The desktop shell launches, opens a formation, watches it for external edits, and auto-indexes notes into an embedded SurrealDB. Chat is intent-classified: Write-mode messages run through the gline-rs extraction pipeline and land as bi-temporal facts with provenance; Ask-mode questions run hybrid retrieval (vector + graph) and stream a cited answer. The staging tray UI (review-before-commit) lands in Phase 3.

See [docs/plans/phase-2.md](docs/plans/phase-2.md) for the milestone breakdown and [docs/adr/](docs/adr/) for the architecture decisions.

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

Deterministic entity + relation extraction uses [`gline-rs`](https://github.com/fbilhaut/gline-rs) loading the [multitask GLiNER ONNX model](https://huggingface.co/knowledgator/gliner-multitask-large-v0.5). Files are ~500MB and are not bundled. Open your formation, then:

```bash
cd <formation>/.chat-notes/models
mkdir -p gliner-multitask-large-v0.5/onnx
cd gliner-multitask-large-v0.5
curl -L -o tokenizer.json   https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/tokenizer.json
curl -L -o onnx/model.onnx  https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx
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

The storage half of the pipeline is covered by `cargo test` (20 tests, no models needed). The model-dependent extraction path has `#[ignore]`d tests — run them with the model present via `cargo test -- --ignored`.

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
│   │   │   ├── extraction.rs        # GLiNER NER (when models present)
│   │   │   └── hardware.rs          # Tier detection + onboarding state
│   │   └── core/                    # Long-lived subsystems
│   │       ├── formation_state.rs   # Active formation + AppConfig
│   │       ├── watcher.rs           # Debounced notify watcher
│   │       ├── memory.rs            # Embedded SurrealDB store
│   │       ├── ollama_sidecar.rs    # Daemon lifecycle + client
│   │       ├── extraction.rs        # EntityExtractor trait + GlinerExtractor
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
