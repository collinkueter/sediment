# ADR-0001: Tauri + Rust core, no Python sidecar

**Status:** Accepted (2026-05-19)
**Supersedes:** the earlier draft architecture that used a Python sidecar for Graphiti, FalkorDB for graph storage, and LanceDB for vectors.

## Context

Sediment is a local-first desktop app that runs a chat-driven extraction pipeline alongside one or more local LLMs. The app's memory budget on the lowest tier is 16 GB RAM with a 3B model already loaded — every megabyte of baseline runtime matters.

The v0.1 spec called for:
1. Tauri shell + React UI + Rust core
2. Ollama for LLM inference
3. **Graphiti** (Python) for temporal entity tracking
4. **FalkorDB** for the underlying graph store
5. **LanceDB** as a separate columnar vector store

That stack ships three separate data systems and a Python runtime, all of which need to coexist with the LLM in RAM. It also drags in cross-language IPC, a second package manager, and PyInstaller-style packaging headaches.

We considered alternative app shells (Electron + Python sidecar; PyWebView + FastAPI + React; PyQt) and rejected them because either the memory baseline was too high for the 16 GB tier or the UI quality fell short (PyQt loses the CodeMirror-style inline-diff experience that's central to the spec's staging UX).

## Decision

- **Shell:** Tauri 2.x. Native-feeling window, small binary, low memory baseline.
- **Backend language:** Rust only. No Python runtime.
- **LLM:** Ollama (existing decision, unchanged).
- **Memory store:** **SurrealDB embedded** (`kv-surrealkv` backend). One in-process store replaces the planned Graphiti + FalkorDB + LanceDB trio. SurrealDB handles:
  - The bi-temporal `fact` graph (entities + relation edges with `valid_from` / `valid_to`)
  - HNSW vector indexes on entity + note-chunk embeddings
  - Document storage for chat history and staging state
- **Extraction:** **`gline-rs`** (GLiNER ONNX inference) for deterministic NER + relation extraction. LLMs are reserved for the genuinely generative steps (fact routing, diff drafting, Q&A).
- **UI:** React 19 + TypeScript + Vite + Tailwind v4 + CodeMirror 6 + Zustand.

We initially evaluated `graphrag-rs` (automataIA) for extraction but discovered it is a 50KLOC opinionated full RAG framework whose `persistent-storage` feature pulls LanceDB back as a transitive dep — undoing the consolidation. `gline-rs` is the focused alternative; one multitask GLiNER model covers both NER and RE.

## Consequences

- **Positive**
  - One backend language, one in-process store. No cross-language IPC, no Python packaging, no second runtime in RAM.
  - SurrealQL lets graph traversal and vector search compose in a single query — the spec's hybrid retrieval (graph + vector) becomes one round trip.
  - Bi-temporal semantics are first-class in SurrealDB (typed `RELATION` tables + `option<datetime>` validity fields), not bolted on.
  - User-facing language updated to **formation** in place of **vault** to match the geological metaphor already in the spec ("the vault is the formation").

- **Negative**
  - SurrealDB is a large dep (the embedded build adds non-trivial weight to the Tauri binary). Release-build size impact is on the Phase 1 measurement list (spec §14, open question 3).
  - `gline-rs` model files are ~500 MB and not bundled — bootstrap is a documented `curl` step.
  - The KNN HNSW operator in SurrealDB 3.x uses the literal `<|K,EF|>` form (no parameter binding for K). The query builder must template these in safely.
  - Rust ecosystem rough edges hit us during Phase 1: chrono 0.4.42+ introduced a `Datelike::quarter()` default that collides with `arrow-arith` 52.x, so the lockfile pins chrono to `0.4.41`. Removing lancedb removed the broader Arrow/AWS-SDK tree.

## Notes

- This decision was made mid-Phase-1 after the original three-sidecar plan reached M3. M1 and M2 were stack-agnostic; M7 was rewritten from "LanceDB integration" to "SurrealDB smoke test" as part of the pivot.
- A `core::extraction::EntityExtractor` trait isolates the call site from the concrete `GlinerExtractor`. If real-world recall on user-style notes is poor, the LLM-grammar fallback path (open question §14.1) can drop in behind the same trait.
