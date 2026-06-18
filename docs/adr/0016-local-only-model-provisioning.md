# ADR-0016: Local-only on-device model provisioning — explicit setup, no runtime network

## Status

Accepted. Refines ADR-0014.

## Context

ADR-0014 added the **`Bundled`** provider: an in-process `fastembed` embedder so
users get semantic search without the Ollama daemon. But "bundled" was a
misnomer — nothing was bundled. The weights were fetched **lazily from Hugging
Face on first use**, inside the indexing / search hot path, via
`TextEmbedding::try_new`. Two failures followed from that:

1. **Spooky, silent breakage.** Selecting "Use on-device search" persisted
   `embedding_provider = "bundled"` and reported ready immediately
   (`check_model_readiness` skipped the gate for non-Ollama providers). If the
   first-use download then failed — blocked network, offline, corporate proxy —
   indexing and `search_notes` kept failing with no actionable signal.
2. **Not actually local.** A provider sold as "runs on your machine" reached out
   to a model host at runtime, which is exactly what locked-down / air-gapped
   deployments cannot allow.

## Decision

Make the on-device provider **strictly file-based**, and make model acquisition
an **explicit, recoverable setup step** — never something that happens during
indexing or search.

- **Runtime is local-only.** `core::bundled_embed` loads the model from a fixed,
  app-owned directory (`~/.sediment/models/nomic-embed-text-v1.5/`) via
  fastembed's `try_new_from_user_defined` / `UserDefinedEmbeddingModel`. No
  `hf-hub` fetch. The same `nomic-embed-text-v1.5` ONNX file, **mean pooling**,
  **no quantization** reproduce the exact 768-d vectors the old path produced, so
  an existing bundled index stays valid — **no re-index required**.
- **Acquisition has two explicit routes** (`commands::models`):
  - `download_bundled_model` — streams the five files into a staging dir with
    per-file byte progress, validates them by loading a session, then atomically
    promotes them into the model dir. The base URL is **configurable**
    (`AppConfig.bundled_model_url` or the `SEDIMENT_MODEL_BASE_URL` env var),
    defaulting to the Hugging Face repo, so a mirror can be used. This is the
    *only* place model acquisition touches the network.
  - `import_bundled_model` — installs the files from a user-chosen folder (repo
    layout or flat), for offline / air-gapped setups. Zero network.
  - Both validate by **loading a session** before install — a bad or incomplete
    pack is rejected up front rather than at search time.
- **Readiness is honest.** `check_model_readiness` now reports the active
  provider and, for `bundled`, gates on whether the model files are present on
  disk. The launch / settings flow shows a **Download / Import / keyword** setup
  screen when they are missing instead of claiming ready.
- **Fail loud.** Switching to on-device persists the provider and re-runs the
  readiness check; a missing model routes to setup. The terse
  `init bundled embedder` error became an actionable message.

## Consequences

- **Truly local semantic search.** After a one-time download (or an offline
  import), indexing and search never reach the network. Air-gapped installs are
  supported via folder import + a configurable mirror.
- **No more silent failures.** A model that can't be acquired surfaces an
  explicit, recoverable setup screen; it can never masquerade as ready.
- **Full-precision kept.** We stayed on `onnx/model.onnx` (not the quantized
  build), so no existing bundled index is invalidated. The pack is larger
  (~0.5 GB) to download/import as a result.
- **Re-acquisition on upgrade.** The old lazy `~/.sediment/fastembed` hf-hub
  cache is not migrated to the new `~/.sediment/models/...` layout; users who had
  bundled will be prompted to download/import once. Their indexed vectors remain
  valid.
- **In-session model swap needs a relaunch.** The process-wide model loads once
  behind a `OnceLock`; replacing the files mid-session takes effect on the next
  launch (a first-time install in the same session loads fine).
- **Still open:** pre-bundling the weights into the installer (zero first-run
  download), zip-archive import (folder import covers the offline case today),
  and content-hash verification of downloaded files.
