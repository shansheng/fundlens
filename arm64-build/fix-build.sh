#!/usr/bin/env bash
# 修复 MNN 预编译下载失败：把容器内已编译的 libMNN.a 放入 prebuilt/ 让 rusto-mnn-sys 命中，随后重跑构建
set -uo pipefail
SRC="/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens"
OUT="$SRC/dist-arm64"
CT="fl-build"
LOG="$SRC/arm64-build/fix-build.log"
SCRIPT_DIR="$SRC/arm64-build"
: > "$LOG"
log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

log "=== fix-build: place prebuilt libMNN.a + rebuild ==="
docker start "$CT" >/dev/null 2>&1 || true

# 1) 找到本次构建用的 rusto-mnn-sys out（含 build/libMNN.a 且 prebuilt 为空的）
docker exec "$CT" bash -c '
set -e
OUT_DIR=""
for d in /build/src-tauri/target/release/build/rusto-mnn-sys-*/out; do
  if [ -f "$d/build/libMNN.a" ] && [ ! -f "$d/prebuilt/libMNN.a" ]; then OUT_DIR="$d"; fi
done
if [ -z "$OUT_DIR" ]; then
  HIT=$(ls /build/src-tauri/target/release/build/rusto-mnn-sys-*/out/prebuilt/libMNN.a 2>/dev/null | head -1)
  if [ -n "$HIT" ]; then
    echo "prebuilt libMNN.a already in place: $HIT ($(stat -c %s "$HIT") bytes)"
  else
    echo "ERROR: no rusto-mnn-sys out with build/libMNN.a found"; exit 3
  fi
else
  mkdir -p "$OUT_DIR/prebuilt"
  cp "$OUT_DIR/build/libMNN.a" "$OUT_DIR/prebuilt/libMNN.a"
  echo "placed prebuilt libMNN.a in $OUT_DIR/prebuilt/ ($(stat -c %s "$OUT_DIR/prebuilt/libMNN.a") bytes)"
  ls -la "$OUT_DIR/prebuilt/"
fi
' 2>&1 | tee -a "$LOG" || { log "ERROR: libMNN placement failed"; exit 1; }

# 2) 重跑 tauri build（deb + appimage）
log "tauri build (deb+appimage) — incremental, MNN prebuilt hit expected"
docker exec \
    -e CARGO_PROFILE_RELEASE_LTO=false \
    -e CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
    -e CARGO_PROFILE_RELEASE_OPT_LEVEL=2 \
    -e CARGO_BUILD_JOBS=4 \
    -e LIBCLANG_PATH=/usr/lib/llvm-10/lib \
    -e APPIMAGE_EXTRACT_AND_RUN=1 \
    "$CT" bash -c 'set -e; cd /build && npm run tauri build -- --bundles deb appimage' \
    2>&1 | tee -a "$LOG" || log "WARN: tauri build returned non-zero (QEMU 下 linuxdeploy appimage 插件无法 exec，属预期；deb 已产出，AppImage 走步骤 3 手工补完)"
# 校验 deb 产出
if ! docker exec "$CT" bash -c 'ls /build/src-tauri/target/release/bundle/deb/*.deb >/dev/null 2>&1'; then
  log "ERROR: no .deb produced — build failed"; exit 1
fi
log "deb produced OK"

# 3) 手工 AppImage（含 wayland 排除）
VER="$(docker exec "$CT" node -e "console.log(require('/build/package.json').version)" 2>/dev/null | tr -d '\r' || echo 1.1.0)"
docker cp "$SCRIPT_DIR/make-appimage.sh" "$CT":/build/ 2>&1 | tee -a "$LOG"
docker exec "$CT" bash -c "bash /build/make-appimage.sh /build/src-tauri/target/release/bundle/appimage/FundLens.AppDir /build/src-tauri/target/release/bundle/appimage/FundLens_${VER}_aarch64.AppImage" \
    2>&1 | tee -a "$LOG" || { log "ERROR: appimage step failed"; exit 1; }
log "appimage OK"

# 4) 拷出产物（覆盖旧包）
log "copying artifacts"
rm -rf "$OUT/deb" "$OUT/appimage"
docker cp "$CT":/build/src-tauri/target/release/bundle/deb "$OUT"/ 2>&1 | tee -a "$LOG"
docker cp "$CT":/build/src-tauri/target/release/bundle/appimage "$OUT"/ 2>&1 | tee -a "$LOG"
log "=== artifacts ==="
ls -la "$OUT"/deb/*.deb "$OUT"/appimage/*.AppImage 2>&1 | tee -a "$LOG"
log "=== DONE ($(date)) ==="
