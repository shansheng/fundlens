#!/usr/bin/env bash
# Download PaddleOCR (PP-OCRv5 mobile) MNN model weights for FundLens local OCR.
#
# These are the official PaddleOCR models converted to MNN format by the RapidOCR
# project, fetched from ModelScope. They are bundled with the app and never leave
# the user's machine. Output: src-tauri/resources/ocr/{det.mnn,rec.mnn,cls.mnn,dict.txt}
#
# Model set: det/rec = PP-OCRv5 mobile; dict = ppocr_keys_v1 (shared with v4, the
# RapidOCR repo ships no v5-specific Chinese dictionary); cls = v2.0 mobile (also
# shared, only exists under the v4 path).
#
# Upgrade safety: MODEL_VERSION marker. When the marker differs from the version
# this script expects, stale weights are deleted and re-downloaded - otherwise
# `fetch` would skip existing files and silently keep an old model generation.
#
# Pure ASCII (no emoji) so it parses cleanly on any shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SCRIPT_DIR}/resources/ocr"
BASE="https://www.modelscope.cn/api/v1/models/RapidAI/RapidOCR/repo?Revision=master&FilePath="
MODEL_VERSION="ppocrv5-mobile"

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

fetch() {
  local filename="$1"
  local modelscope_path="$2"
  local dest="${OUT_DIR}/${filename}"
  if [ -s "$dest" ]; then
    echo "  skip (exists): ${filename}"
    return 0
  fi
  echo "  downloading: ${filename}"
  if curl -sSL -f "${BASE}${modelscope_path}" -o "$dest"; then
    echo "  ok: ${filename} ($(du -h "$dest" | cut -f1))"
  else
    echo "  FAILED: ${filename}" >&2
    rm -f "$dest"
    return 1
  fi
}

echo "== FundLens OCR model download (PP-OCRv5 mobile / MNN) =="
echo "   target: ${OUT_DIR}"

# detection + recognition + dictionary (required)
fetch "det.mnn" "mnn%2FPP-OCRv5%2Fdet%2Fch_PP-OCRv5_det_mobile.mnn"
fetch "rec.mnn" "mnn%2FPP-OCRv5%2Frec%2Fch_PP-OCRv5_rec_mobile.mnn"
fetch "dict.txt" "paddle%2FPP-OCRv4%2Frec%2Fch_PP-OCRv4_rec_mobile%2Fppocr_keys_v1.txt"

# angle classifier (optional but improves accuracy on rotated captures)
fetch "cls.mnn" "mnn%2FPP-OCRv4%2Fcls%2Fch_ppocr_mobile_v2.0_cls_mobile.mnn" || \
  echo "  (cls optional, skipped)"

echo "$MODEL_VERSION" > "${OUT_DIR}/MODEL_VERSION"

echo "== done =="
echo "   models in: ${OUT_DIR}"
echo "   now build with: npm run tauri build --features ocr"
