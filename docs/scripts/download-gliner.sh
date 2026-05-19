#!/usr/bin/env bash
# Bootstrap the GLiNER multitask ONNX model into a formation's .chat-notes/models/
# directory. Run once per formation. Idempotent: skips files that already exist.
#
# Usage:
#   docs/scripts/download-gliner.sh <path/to/formation>
#
# Example:
#   docs/scripts/download-gliner.sh ~/Documents/MyVault
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <formation-path>" >&2
  exit 2
fi

FORMATION="$1"
if [[ ! -d "$FORMATION" ]]; then
  echo "error: $FORMATION is not a directory" >&2
  exit 1
fi

MODEL_DIR="$FORMATION/.chat-notes/models/gliner-multitask-large-v0.5"
mkdir -p "$MODEL_DIR/onnx"

TOKENIZER_URL="https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/tokenizer.json"
ONNX_URL="https://huggingface.co/knowledgator/gliner-multitask-large-v0.5/resolve/main/onnx/model.onnx"

if [[ -f "$MODEL_DIR/tokenizer.json" ]]; then
  echo "tokenizer.json already present, skipping"
else
  echo "downloading tokenizer.json (small)..."
  curl -L --fail -o "$MODEL_DIR/tokenizer.json" "$TOKENIZER_URL"
fi

if [[ -f "$MODEL_DIR/onnx/model.onnx" ]]; then
  echo "model.onnx already present, skipping"
else
  echo "downloading onnx/model.onnx (around 500 MB)..."
  curl -L --fail --progress-bar -o "$MODEL_DIR/onnx/model.onnx" "$ONNX_URL"
fi

echo
echo "done. Sediment will load the model lazily on first extract_facts call."
echo "model dir: $MODEL_DIR"
