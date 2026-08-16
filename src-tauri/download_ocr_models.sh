#!/usr/bin/env bash
# Download PaddleOCR (PP-OCRv4 mobile) MNN model weights for FundLens local OCR.
#
# These are the official PaddleOCR models converted to MNN format by the RapidOCR
# project, fetched from ModelScope. They are bundled with the app and never leave
# the user's machine. Output: src-tauri/resources/ocr/{det.mnn,rec.mnn,cls.mnn,dict.txt}
#
# Pure ASCII (no emoji) so it parses cleanly on any shell.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${SCRIPT_DIR}/resources/ocr"
BASE="https://www.modelscope.cn/api/v1/models/RapidAI/RapidOCR/repo?Revision=master&FilePath="

mkdir -p "$OUT_DIR"

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

echo "== FundLens OCR model download (PP-OCRv4 mobile / MNN) =="
echo "   target: ${OUT_DIR}"

# detection + recognition + dictionary (required)
fetch "det.mnn" "mnn%2FPP-OCRv4%2Fdet%2Fch_PP-OCRv4_det_mobile.mnn"
fetch "rec.mnn" "mnn%2FPP-OCRv4%2Frec%2Fch_PP-OCRv4_rec_mobile.mnn"
fetch "dict.txt" "paddle%2FPP-OCRv4%2Frec%2Fch_PP-OCRv4_rec_mobile%2Fppocr_keys_v1.txt"

# angle classifier (optional but improves accuracy on rotated captures)
fetch "cls.mnn" "mnn%2FPP-OCRv4%2Fcls%2Fch_ppocr_mobile_v2.0_cls_mobile.mnn" || \
  echo "  (cls optional, skipped)"

echo "== done =="
echo "   models in: ${OUT_DIR}"
echo "   now build with: npm run tauri build --features ocr"
