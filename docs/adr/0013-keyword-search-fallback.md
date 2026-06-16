# ADR-0013: Keyword-search fallback — a no-local-model option

## Status

Accepted.

## Context

Since ADR-0009 retired GLiNER and the hardware-tier strategy, the **only** model
Sediment provisions locally is the Ollama embedding model (`nomic-embed-text`),
which backs semantic note retrieval: the indexer embeds note chunks, the
deterministic pre-pass (ADR-0011) embeds the message to pull related notes, and
the agent's `search_notes` tool embeds its query for an HNSW vector search.

That makes a ~0.3 GB download and a running Ollama daemon a hard dependency for
note search. For users who don't want to install Ollama — or who simply want a
zero-setup, fully offline experience — there was no path: the launch-time
`check_model_readiness` gate either downloaded the model or left search disabled.

## Decision

Make the note-search backend a **provider** the user chooses, defaulting to the
existing semantic path. Add a second provider, **keyword search**, that needs no
local model and no network.

- `core::embedding::EmbeddingProvider { Ollama, None }`, persisted in
  `AppConfig.embedding_provider` (`"ollama"` default | `"none"`).
- The three embed call sites become provider-aware:
  - **indexer** — in keyword mode stores text-only chunks (`embedding = NULL`);
  - **pre-pass** — keyword mode pulls related notes via the keyword search;
  - **`search_notes`** — keyword mode runs the keyword search.
- `search_chunks_text` does case-insensitive substring **term matching** over
  `note_chunk.text` (tokenise the query, match chunks containing any term, rank
  by distinct-term count). No full-text index is defined — SurrealDB 3.0.5 does
  not accept the `SEARCH ANALYZER … BM25` index syntax, and an unindexed scan is
  ample for personal-scale formations. `note_chunk.embedding` becomes optional.
- The MCP `search_notes` subprocess learns the provider via a new
  `SEDIMENT_EMBEDDING_PROVIDER` env var (alongside `SEDIMENT_FORMATION` /
  `SEDIMENT_SOURCE_CHAT_ID`), forwarded by both engines' MCP config.
- `check_model_readiness` reports ready (and skips the Ollama probe) when the
  provider is `None`, so the setup gate never blocks keyword mode. The model
  setup screen offers **"Use keyword search instead"**, and Settings exposes a
  **Semantic / Keyword** toggle (applies immediately; re-index to populate).

## Consequences

- **No-model, offline path.** A user can run Sediment with zero downloads; note
  search still works, just lexically.
- **Quality trade-off.** Keyword search is substring/term matching, not
  semantic — it misses paraphrase. This is the explicit cost of the mode; the
  default stays semantic.
- **Switching providers needs a re-index.** Chunks indexed in keyword mode have
  no embedding (and vice-versa, older chunks lack the populated text path only
  if they predate this change — they don't). Switching to semantic and back is a
  formation-wide re-index away; the Settings copy says so.
- **Not pursued:** a bundled in-process embedder (ONNX/fastembed — removes
  Ollama but keeps a model and adds native deps + binary size) and cloud BYOK
  embeddings (sends note text off-device; needs secret storage). Both remain
  open as future providers behind the same `EmbeddingProvider` seam.
