#!/usr/bin/env bash
# Download PaddleOCR (PP-OCRv6 tiny) MNN model weights for FundLens local OCR.
#
# These are the official PaddleOCR models converted to MNN format by the RustO! project,
# fetched from the versioned rusto-rs-models release (tag v1.0.0) so the bytes are pinned
# rather than tracking a moving master. They are bundled with the app and never leave
# the user's machine. Output: src-tauri/resources/ocr/{det.mnn,rec.mnn,cls.mnn,dict.txt}
#
# Model set: det/rec = PP-OCRv6 tiny; dict = ppocrv6_tiny_dict.txt; cls = v2.0 mobile.
#
# !! The dictionary MUST match the model generation !!
# PP-OCRv5/V6 rec models are trained against a different character dictionary (different
# index order) than the old PP-OCRv4 ppocr_keys_v1.txt. Pairing a v5/v6 model with the v4
# dictionary does not error out - it silently emits high-confidence garbage (every Chinese
# character shifted). That bug shipped in commit e0838e7 (v2.6.9 -> v2.6.15): det/rec were
# bumped to PP-OCRv5 but dict.txt was left on ppocr_keys_v1.txt. Fixed here by taking the
# dictionary from the same release as the weights.
#
# Note on finding dictionaries: the ModelScope RapidAI/RapidOCR repo has no Chinese
# dictionary under mnn/, and models/PPOCR_v5/dict.txt in the RustO! repo is only 1,416
# bytes (English). The Chinese ones live in the rusto-rs-models release assets.
#
# Upgrade safety: MODEL_VERSION marker. When the marker differs from the version
# this script expects, stale weights are deleted and re-downloaded - otherwise
# `fetch` would skip existing files and silently keep an old model generation.
#
# Pure ASCII (no emoji) so it parses cleanly on any shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SCRIPT_DIR}/resources/ocr"
RELEASE_BASE="https://github.com/byrizki/rusto-rs-models/releases/download/v1.0.0/"
CLS_BASE="https://www.modelscope.cn/api/v1/models/RapidAI/RapidOCR/repo?Revision=master&FilePath="
MODEL_VERSION="ppocrv6-tiny"

mkdir -p "$OUT_DIR"

# --- version check: wipe stale weights from a previous model generation ---
if [ -f "${OUT_DIR}/MODEL_VERSION" ] && [ "$(cat "${OUT_DIR}/MODEL_VERSION")" != "$MODEL_VERSION" ]; then
  echo "  model version changed -> removing stale weights in ${OUT_DIR}"
  rm -f "${OUT_DIR}/det.mnn" "${OUT_DIR}/rec.mnn" "${OUT_DIR}/cls.mnn" "${OUT_DIR}/dict.txt"
elif [ ! -f "${OUT_DIR}/MODEL_VERSION" ] && [ -f "${OUT_DIR}/rec.mnn" ]; then
  # no marker but weights present: pre-marker install (PP-OCRv4) -> force refresh
  echo "  no MODEL_VERSION marker -> assuming legacy PP-OCRv4 weights, refreshing"
  rm -f "${OUT_DIR}/det.mnn" "${OUT_DIR}/rec.mnn" "${OUT_DIR}/cls.mnn" "${OUT_DIR}/dict.txt"
fi

# fetch <filename> <full-url> [expected-bytes]
# Downloads to a .part file and renames on success, so an interrupted transfer can never
# leave a truncated weight behind that a later run would treat as already present.
fetch() {
  local filename="$1"
  local url="$2"
  local expect="${3:-}"
  local dest="${OUT_DIR}/${filename}"
  local part="${dest}.part"
  if [ -s "$dest" ]; then
    if [ -z "$expect" ] || [ "$(wc -c < "$dest" | tr -d ' ')" = "$expect" ]; then
      echo "  skip (exists): ${filename}"
      return 0
    fi
    echo "  size mismatch on existing ${filename} -> re-downloading"
  fi
  echo "  downloading: ${filename}"
  rm -f "$part"
  if curl -sSL -f --retry 3 --retry-delay 2 "${url}" -o "$part"; then
    local got
    got="$(wc -c < "$part" | tr -d ' ')"
    if [ -n "$expect" ] && [ "$got" != "$expect" ]; then
      echo "  FAILED: ${filename} size mismatch (got ${got}, want ${expect})" >&2
      rm -f "$part"
      return 1
    fi
    mv -f "$part" "$dest"
    echo "  ok: ${filename} (${got} bytes)"
  else
    echo "  FAILED: ${filename}" >&2
    rm -f "$part"
    return 1
  fi
}

echo "== FundLens OCR model download (PP-OCRv6 tiny / MNN) =="
echo "   target: ${OUT_DIR}"
echo "   source: ${RELEASE_BASE}"

# detection + recognition + dictionary (required, same release -> guaranteed to match)
fetch "det.mnn"  "${RELEASE_BASE}ppocrv6_det_tiny.mnn"   1745176
fetch "rec.mnn"  "${RELEASE_BASE}ppocrv6_rec_tiny.mnn"   4461484
fetch "dict.txt" "${RELEASE_BASE}ppocrv6_tiny_dict.txt"  27156

# angle classifier (optional but improves accuracy on rotated captures; PP-OCRv6 ships none,
# the v2.0 mobile cls is model-generation independent - it only predicts 0/180 rotation)
fetch "cls.mnn" "${CLS_BASE}mnn%2FPP-OCRv4%2Fcls%2Fch_ppocr_mobile_v2.0_cls_mobile.mnn" 531024 || \
  echo "  (cls optional, skipped)"

echo "$MODEL_VERSION" > "${OUT_DIR}/MODEL_VERSION"

echo "== done =="
echo "   models in: ${OUT_DIR}"
echo "   now build with: npm run tauri build --features ocr"
