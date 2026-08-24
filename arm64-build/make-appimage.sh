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
# 容错：tauri 生成的 AppDir 可能是小写 fund-lens.AppDir（Linux）或大写 FundLens.AppDir（macOS）
if [ ! -d "$APPDIR" ]; then
    ALT="$(dirname "$APPDIR")/fund-lens.AppDir"
    if [ -d "$ALT" ]; then
        echo "AppDir not found as given, using lowercase variant: $ALT" >&2
        APPDIR="$ALT"
    fi
fi
if [ ! -d "$APPDIR" ]; then
    echo "AppDir not found: ${1:-}" >&2
    exit 3
fi

CACHE="/root/.cache/tauri"
# appimagetool 来源：优先 linuxdeploy-plugin-appimage.AppImage（tauri 缓存），
# 缺失时回退 linuxdeploy-aarch64.AppImage（linuxdeploy 发行版自带 appimagetool-prefix）。
PLUGIN=""
for cand in "$CACHE/linuxdeploy-plugin-appimage.AppImage" "$CACHE/linuxdeploy-aarch64.AppImage"; do
    if [ -f "$cand" ]; then PLUGIN="$cand"; break; fi
done
if [ -z "$PLUGIN" ]; then
    echo "no linuxdeploy/plugin AppImage found under $CACHE" >&2
    exit 4
fi

TMP="$(mktemp -d)"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# ---- 1. 从 linuxdeploy/plugin AppImage 解出 appimagetool ----
# static-pie 的 AppImage runtime 无法在 QEMU 下直接运行，但它是合法 type-2 AppImage，
# 内含 squashfs（魔术字 hsqs）。定位偏移后用 unsquashfs 解包取出其中的 appimagetool。
OFF="$(grep -abo hsqs "$PLUGIN" | head -1 | cut -d: -f1)"
if [ -z "$OFF" ]; then
    echo "cannot locate squashfs superblock in plugin AppImage" >&2
    exit 5
fi
unsquashfs -o "$OFF" -f -d "$TMP" "$PLUGIN" >/dev/null
# 两种解包布局：plugin AppImage 的 usr/bin/appimagetool；linuxdeploy 的 appimagetool-prefix
TOOL=""
for cand in "$TMP/usr/bin/appimagetool" \
            "$TMP/plugins/linuxdeploy-plugin-appimage/appimagetool-prefix/usr/bin/appimagetool"; do
    if [ -x "$cand" ]; then TOOL="$cand"; break; fi
done
if [ -z "$TOOL" ]; then
    echo "appimagetool not found after extraction" >&2
    exit 6
fi
# appimagetool 需要同 prefix 的 desktop-file-validate 与动态库
AI_PREFIX="$(dirname "$(dirname "$TOOL")")"
export PATH="$AI_PREFIX/bin:$PATH"
export LD_LIBRARY_PATH="$AI_PREFIX/lib:${LD_LIBRARY_PATH:-}"

# ---- 2. 修图标名大小写（desktop 引用 fundlens，根目录只有 FundLens.png）----
if [ -f "$APPDIR/FundLens.png" ] && [ ! -f "$APPDIR/fundlens.png" ]; then
    cp "$APPDIR/FundLens.png" "$APPDIR/fundlens.png"
fi

# ---- 2.5 排除 Wayland 客户端库（麒麟 Mali 私有 EGL 符号冲突）----
# 麒麟 V10 SP1 上 AppImage 报 "undefined symbol: wl_proxy_unref"：
# AppImage 内置了 libwayland-client.so.0 等（Tauri bundler 顺带打入），而 EGL 来自系统
# Mali 驱动 /usr/lib/aarch64-linux-gnu/mali/libEGL.so.1（AppImage 内无 libEGL）。
# 运行时 LD_LIBRARY_PATH 优先，Mali EGL 加载到 AppImage 内置 wayland → 符号表与驱动编译
# 时链接的系统 wayland 版本不一致。系统自带 wayland（webkit2gtk 依赖链，deb 可跑即证），
# 删除内置库让 EGL 回落到系统库即可，同时避免 X11 会话下 GTK/WebKit 误走 Wayland 渲染。
rm -f "$APPDIR"/usr/lib/libwayland-client.so.0* \
      "$APPDIR"/usr/lib/libwayland-server.so.0* \
      "$APPDIR"/usr/lib/libwayland-egl.so.1* \
      "$APPDIR"/usr/lib/libwayland-cursor.so.0*

# ---- 3. 打包 ----
APPIMAGE_EXTRACT_AND_RUN=1 "$TOOL" "$APPDIR" "$OUT_APPIMAGE"
echo "AppImage written: $OUT_APPIMAGE ($(stat -c %s "$OUT_APPIMAGE") bytes)"
