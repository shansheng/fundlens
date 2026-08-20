#!/usr/bin/env bash
#
# FundLens — arm64 Linux package build inside an emulated (QEMU) aarch64 container.
# The whole build happens on the Docker VM disk; only the final .deb + .AppImage
# are copied out to the host (dist-arm64/).
#
# Source transfer (RELIABLE path):
#   - macOS bsdtar emits PAX extended headers that make GNU tar inside the
#     emulated container fail/hang (EINVAL on PaxHeaders staging). So we DO NOT
#     run tar inside the QEMU container.
#   - Instead we (a) build a clean GNU-format tarball on the NATIVE macOS host
#     (mac metadata stripped, --format=gnutar => no PAX), (b) extract it on the
#     native host, and (c) `docker cp` the directory tree into the container.
#     docker cp's archive is produced by the Docker daemon on the native Linux
#     VM (not QEMU-emulated), which is the well-trodden, reliable macOS path.
#
# Release profile overrides (cargo env) to survive QEMU emulation:
#   - disable full LTO (manifest sets lto=true, far too slow under emulation)
#   - parallel codegen units instead of 1
#   - opt-level 2 (slightly faster to compile than size-optimized "s")
#   - limit parallel jobs to avoid emulated-compiler memory blowups
set -euo pipefail

SRC="/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens"
OUT="/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens/dist-arm64"
IMG="fundlens-builder"
CT="fl-build"
WORK="/tmp/fundlens-build-$$"
TARBALL="$WORK/fundlens-src.tar.gz"
EXTRACT="$WORK/src"
LOG="/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens/arm64-build/build.log"

mkdir -p "$OUT" "$EXTRACT"
: > "$LOG"

log() { printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" | tee -a "$LOG"; }

log "=== FundLens arm64 build started ==="

# ---- 1. container lifecycle ----
docker rm -f "$CT" >/dev/null 2>&1 || true
log "creating container $CT from $IMG"
docker run -d --name "$CT" "$IMG" sleep infinity

# ---- 2. build a clean GNU-format tarball on the host (mac metadata stripped) ----
log "creating source tarball (no mac metadata, gnutar format)"
COPYFILE_DISABLE=1 tar --no-mac-metadata --format=gnutar \
    --exclude=node_modules --exclude=target --exclude=.git --exclude=dist \
    --exclude='*.db' --exclude='*.db.bak' --exclude='*.db.bak2' --exclude='.DS_Store' \
    -C "$SRC" -czf "$TARBALL" .
log "tarball: $(du -h "$TARBALL" | cut -f1) — entries: $(tar -tzf "$TARBALL" | wc -l)"

# ---- 3. extract NATIVELY on host, then docker cp the tree into the container ----
log "extracting tarball on host (native)"
tar -xzf "$TARBALL" -C "$EXTRACT"
log "extracted $(find "$EXTRACT" -type f | wc -l) files on host"
log "docker cp tree into container"
docker cp "$EXTRACT/." "$CT":/build/
log "source in container ($(docker exec "$CT" find /build -type f | wc -l) files)"

# ---- 4. build (native aarch64) ----
log "running npm ci + tauri build --bundles deb appimage"
# LIBCLANG_PATH: rusto-mnn-sys build.rs 用 bindgen 生成 FFI 绑定，必须找得到 libclang.so
#   （Debian 装 libclang-dev 后位于 /usr/lib/llvm-14/lib，bindgen 不会自动探测）。
# APPIMAGE_EXTRACT_AND_RUN=1: 容器内无 /dev/fuse，AppImage 无法挂载自身；
#   改用"解压到临时目录再运行"，linuxdeploy 主体能跑起来（但其 appimage plugin 是
#   static-pie AppImage，QEMU 下仍无法 exec —— 故 appimage 这一步会失败，下面用手工补完）。
docker exec \
    -e CARGO_PROFILE_RELEASE_LTO=false \
    -e CARGO_PROFILE_RELEASE_CODEGEN_UNITS=256 \
    -e CARGO_PROFILE_RELEASE_OPT_LEVEL=2 \
    -e CARGO_BUILD_JOBS=4 \
    -e LIBCLANG_PATH=/usr/lib/llvm-14/lib \
    -e APPIMAGE_EXTRACT_AND_RUN=1 \
    "$CT" bash -c 'set -e; cd /build && npm ci; npm run tauri build -- --bundles deb appimage || true' \
    2>&1 | tee -a "$LOG"
log "tauri build finished (deb produced; appimage step may have failed under QEMU — finishing manually)"

# ---- 4b. 手工补完 AppImage（绕开 QEMU 下无法执行的 static-pie linuxdeploy plugin）----
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
log "manually finishing AppImage via make-appimage.sh"
docker cp "$SCRIPT_DIR/make-appimage.sh" "$CT":/build/
docker exec "$CT" bash -c 'bash /build/make-appimage.sh \
  /build/src-tauri/target/release/bundle/appimage/FundLens.AppDir \
  /build/src-tauri/target/release/bundle/appimage/FundLens_1.1.0_aarch64.AppImage' \
  2>&1 | tee -a "$LOG" || log "WARN: manual AppImage step failed"

# ---- 5. extract artifacts ----
log "copying artifacts to $OUT"
if docker cp "$CT":/build/src-tauri/target/release/bundle/deb "$OUT"/ 2>/dev/null; then
    log "deb copied"
else
    log "WARN: no deb produced"
fi
if docker cp "$CT":/build/src-tauri/target/release/bundle/appimage "$OUT"/ 2>/dev/null; then
    log "appimage copied"
else
    log "WARN: no appimage produced"
fi

# ---- 6. cleanup (free Docker VM disk + host temp) ----
docker stop "$CT" >/dev/null 2>&1 || true
docker rm -f "$CT" >/dev/null 2>&1 || true
rm -rf "$WORK"

log "=== artifacts in $OUT ==="
ls -lhR "$OUT" 2>/dev/null | tee -a "$LOG" || true
log "=== DONE ==="
