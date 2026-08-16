#!/usr/bin/env bash
#
# make-appimage.sh — 在 QEMU 仿真容器内手工补完 AppImage 打包。
#
# 背景：Tauri 2 的 linuxdeploy `appimage` 插件本身是一个 static-pie 的 AppImage，
# 在 QEMU 用户态仿真下无法直接 exec（"Exec format error"），导致
# `tauri build --bundles appimage` 在最后一步 `failed to run linuxdeploy`。
# 但这只是仿真环境限制，不是产物问题。本脚本绕开该 plugin：
#   1. 从 Tauri 已缓存下载的 plugin AppImage 里 unsquashfs 取出 appimagetool
#      （appimagetool 是 shell 脚本，QEMU 可正常执行）；
#   2. 修掉桌面文件图标名大小写不一致（Icon=fundlens vs 根目录 FundLens.png）；
#   3. 直接用 appimagetool 把 Tauri 生成的 AppDir 打成 .AppImage。
#
# 用法（在容器内）：
#   make-appimage.sh <AppDir> <output.AppImage>
set -euo pipefail

APPDIR="${1:-}"
OUT_APPIMAGE="${2:-}"
if [ -z "$APPDIR" ] || [ -z "$OUT_APPIMAGE" ]; then
    echo "usage: make-appimage.sh <AppDir> <output.AppImage>" >&2
    exit 2
fi
if [ ! -d "$APPDIR" ]; then
    echo "AppDir not found: $APPDIR" >&2
    exit 3
fi

CACHE="/root/.cache/tauri"
PLUGIN="$CACHE/linuxdeploy-plugin-appimage.AppImage"
if [ ! -f "$PLUGIN" ]; then
    echo "plugin AppImage not found (tauri should have downloaded it): $PLUGIN" >&2
    exit 4
fi

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# ---- 1. 从 plugin AppImage 解出 appimagetool ----
# static-pie 的 plugin AppImage 无法在 QEMU 下直接运行，但它是合法 type-2 AppImage，
# 内含 squashfs（魔术字 hsqs）。定位偏移后用 unsquashfs 解包取出其中的 appimagetool。
OFF="$(grep -abo hsqs "$PLUGIN" | head -1 | cut -d: -f1)"
if [ -z "$OFF" ]; then
    echo "cannot locate squashfs superblock in plugin AppImage" >&2
    exit 5
fi
unsquashfs -o "$OFF" -f -d "$TMP" "$PLUGIN" >/dev/null
TOOL="$TMP/usr/bin/appimagetool"
if [ ! -x "$TOOL" ]; then
    echo "appimagetool not found after extraction" >&2
    exit 6
fi

# ---- 2. 修图标名大小写（desktop 引用 fundlens，根目录只有 FundLens.png）----
if [ -f "$APPDIR/FundLens.png" ] && [ ! -f "$APPDIR/fundlens.png" ]; then
    cp "$APPDIR/FundLens.png" "$APPDIR/fundlens.png"
fi

# ---- 3. 打包 ----
APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" "$APPDIR" "$OUT_APPIMAGE"
echo "AppImage written: $OUT_APPIMAGE ($(stat -c %s "$OUT_APPIMAGE") bytes)"
