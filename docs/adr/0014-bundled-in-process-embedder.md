# ADR-0014: Bundled in-process embedder — local semantic search without Ollama

## Status

Accepted. Refined by **ADR-0016**, which makes the bundled model strictly
file-based (no runtime Hugging Face fetch) and moves weight acquisition into an
explicit download/import setup step.

## Context

ADR-0013 made note search a chooseable **provider** (semantic via Ollama, or
keyword/BM25 with no model). That left a gap in the middle: users who want
*semantic* search but don't want to run the **Ollama daemon**. In practice the
Ollama dependency is also a reliability liability — `ollama serve` can fail to
bind/answer on `:11434` ("Spawned `ollama serve` but health endpoint never
responded"), which blocks the only semantic path.

The vector store is already fully local: SurrealDB is embedded in-process with
an HNSW cosine index. The only thing tying semantic search to a daemon is the
**embedder**.

## Decision

Add a third provider, **`Bundled`**: an in-process embedding model with no
daemon, via [`fastembed`](https://crates.io/crates/fastembed) (ONNX Runtime
through `ort`).

- Model: **`nomic-embed-text-v1.5` (768-d)** — matches the existing HNSW index
  dimension, so no schema change.
- `core::bundled_embed`: the model is loaded lazily once per process behind a
  `Mutex`; weights are fetched to a fixed, app-owned cache (`~/.sediment/
  fastembed`) on first use and shared between the main process and the
  `--mcp-stdio` subprocess. The CPU-bound `embed` runs on a blocking thread.
- The Ollama-vs-bundled choice is centralised in `embedding::embed_query`, which
  every call site (indexer, pre-pass, `search_notes`) now goes through; it
  returns `None` for keyword mode.
- `check_model_readiness` gates **only** the Ollama provider; bundled and keyword
  report ready and skip the Ollama probe (the bundled model downloads lazily, or
  eagerly via `warmup_embedding_model`).
- UI: the model-setup screen offers **"Use on-device search (no Ollama)"** (sets
  bundled + warms the model), and Settings → Note search is a three-way toggle
  **On-device / Ollama / Keyword**.

## Consequences

- **No-daemon semantic search.** The default-quality experience no longer
  requires installing or running Ollama. This is the recommended path for new
  users and the fix for the `ollama serve` failure.
- **Costs.** Adds the ONNX Runtime native library (`ort`, fetched at build time)
  and a ~80 MB model download (once, to the cache dir). Build time and binary
  footprint grow; distribution must bundle the `ort` runtime per platform.
- **Switching providers needs a re-index.** Ollama-`nomic` and bundled-`nomic-
  v1.5` vectors are not interchangeable (different model build), and keyword mode
  stores no vectors — the Settings copy says "re-index to apply".
- **Default unchanged for existing installs.** `embedding_provider` still
  defaults to `ollama` when unset, so current formations keep their indexed
  vectors; bundled is opt-in (and the prominent choice on the setup screen).
- **Still open:** pre-bundling the weights into the app (no first-run download),
  streamed download progress, and cloud BYOK embeddings — all behind the same
  `EmbeddingProvider` seam.
