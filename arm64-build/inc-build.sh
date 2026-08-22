#!/usr/bin/env bash
#
# FundLens — 增量 arm64 Linux 构建（复用已存在的 fl-build 容器，保留 target/ 与 node_modules 缓存）
# 与 build.sh 的区别：build.sh 每次 docker rm -f + 重建容器（丢失缓存，全量 ~7.5h）；
# 本脚本 docker start 现有容器，仅刷新源码，增量续编 ~1h。
#
# 产物：src-tauri/target/release/bundle/{deb,appimage}/  →  拷到宿主机 dist-arm64/
set -uo pipefail

SRC="/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens"
OUT="$SRC/dist-arm64"
CT="fl-build"
WORK="/tmp/fundlens-inc-$$"
TARBALL="$WORK/src.tar.gz"
EXTRACT="$WORK/src"
LOG="$SRC/arm64-build/inc-build.log"

mkdir -p "$OUT" "$WORK" "$EXTRACT"
: > "$LOG"
log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

log "=== incremental arm64 build started ($(date)) ==="

# ---- 1. 宿主机打 GNU 格式 tarball（剔除缓存/db/元数据） ----
log "tarballing source (exclude node_modules/target/.git/dist/*.db)"
COPYFILE_DISABLE=1 tar --no-mac-metadata --format=gnutar \
    --exclude=node_modules --exclude=target --exclude=.git --exclude=dist \
    --exclude='*.db' --exclude='*.db.bak' --exclude='*.db.bak2' --exclude='.DS_Store' \
    -C "$SRC" -czf "$TARBALL" .
log "tarball: $(du -h "$TARBALL" | cut -f1) — entries: $(tar -tzf "$TARBALL" | wc -l)"

# ---- 2. 宿主机解压后 docker cp 进容器（保留容器内 node_modules + target 缓存） ----
log "extracting on host"
tar -xzf "$TARBALL" -C "$EXTRACT"
log "docker cp source tree into $CT:/build/ (container node_modules/target preserved)"
docker cp "$EXTRACT/." "$CT":/build/ 2>&1 | tee -a "$LOG"
log "source in container: $(docker exec "$CT" find /build -type f 2>/dev/null | wc -l) files"

# ---- 3. 容器内构建（native aarch64，QEMU 兼容的 cargo profile 覆盖） ----
log "npm install (reconcile, tolerate offline) + tauri build --bundles deb appimage"
docker exec \
    -e CARGO_PROFILE_RELEASE_LTO=false \
    -e CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
    -e CARGO_PROFILE_RELEASE_OPT_LEVEL=2 \
    -e CARGO_BUILD_JOBS=4 \
    -e LIBCLANG_PATH=/usr/lib/llvm-14/lib \
    -e APPIMAGE_EXTRACT_AND_RUN=1 \
    "$CT" bash -c 'set -e; cd /build && (npm install --no-audit --no-fund || echo "[warn] npm install skipped/failed; using cached node_modules"); npm run tauri build -- --bundles deb appimage' \
    2>&1 | tee -a "$LOG" || log "WARN: tauri build step returned non-zero (likely QEMU appimage plugin); continuing"

# 校验 deb 是否产出
if ! docker exec "$CT" bash -c 'ls /build/src-tauri/target/release/bundle/deb/*.deb >/dev/null 2>&1' 2>/dev/null; then
  log "ERROR: no .deb produced — build failed"
  ls -lhR "$OUT" 2>/dev/null | tee -a "$LOG"
  rm -rf "$WORK"
  exit 1
fi
log "deb produced OK"

# ---- 4. 手工补完 AppImage（绕开 QEMU 下无法执行的 static-pie linuxdeploy plugin） ----
SCRIPT_DIR="$SRC/arm64-build"
VER="$(docker exec "$CT" node -e "console.log(require('/build/package.json').version)" 2>/dev/null | tr -d '\r' || echo 1.1.0)"
log "app version: $VER"
docker cp "$SCRIPT_DIR/make-appimage.sh" "$CT":/build/ 2>&1 | tee -a "$LOG"
docker exec "$CT" bash -c "bash /build/make-appimage.sh /build/src-tauri/target/release/bundle/appimage/FundLens.AppDir /build/src-tauri/target/release/bundle/appimage/FundLens_${VER}_aarch64.AppImage" \
    2>&1 | tee -a "$LOG" || log "WARN: manual AppImage step failed"

# ---- 5. 拷出产物 ----
log "copying artifacts to $OUT"
docker cp "$CT":/build/src-tauri/target/release/bundle/deb "$OUT"/ 2>/dev/null && log "deb copied" || log "WARN: no deb"
docker cp "$CT":/build/src-tauri/target/release/bundle/appimage "$OUT"/ 2>/dev/null && log "appimage copied" || log "WARN: no appimage"

log "=== artifacts in $OUT ==="
ls -lhR "$OUT" 2>/dev/null | tee -a "$LOG"
log "=== DONE ($(date)) ==="
rm -rf "$WORK"
