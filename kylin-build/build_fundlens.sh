#!/usr/bin/env bash
# FundLens (feat/kylin-v10-aarch64) 前缀编译封装
# dev 包已解包到 /home/sheng/opt/sysroot；libclang 来自 libclang1-11 (LIBCLANG_PATH 指向 llvm-11)
set -e
export HOME=/home/sheng
SYSROOT=/home/sheng/opt/sysroot
SRC=/media/sheng/data/P/fundlens-src

export PATH="/home/sheng/.workbuddy/binaries/node/versions/22.22.2/bin:$HOME/.cargo/bin:$PATH"
export PKG_CONFIG_SYSROOT_DIR="$SYSROOT"
export PKG_CONFIG_PATH="$SYSROOT/usr/lib/aarch64-linux-gnu/pkgconfig:$SYSROOT/usr/share/pkgconfig:$SYSROOT/usr/lib/pkgconfig"
export PKG_CONFIG_ALLOW_SYSTEM_LIBS=1
export PKG_CONFIG_ALLOW_SYSTEM_CFLAGS=1
export LIBCLANG_PATH="$SYSROOT/usr/lib/llvm-11/lib"
# MNN 静态库（2.8.3，已从 gitee 镜像本地编译）注入，绕开 GitHub 下载/克隆
export MNN_LIB_DIR=/home/sheng/opt/mnn-build
ls "$MNN_LIB_DIR"/libMNN.a >/dev/null 2>&1 && echo "MNN_LIB_DIR: OK (libMNN.a present)" || { echo "MNN_LIB_DIR: MISSING libMNN.a"; exit 1; }

echo "=== 环境校验 ==="
pkg-config --modversion webkit2gtk-4.0 2>/dev/null && echo "webkit2gtk-4.0: OK" || { echo "webkit2gtk-4.0: MISSING"; exit 1; }
echo "LIBCLANG_PATH=$LIBCLANG_PATH"; ls "$LIBCLANG_PATH"/libclang.so* >/dev/null 2>&1 && echo "libclang: OK" || echo "libclang: MISSING"
echo "node=$(node -v) cargo=$(cargo --version 2>/dev/null)"

cd "$SRC"
echo "=== tauri build (--features ocr) ==="
npm run tauri build -- --features ocr
echo "BUILD_DONE rc=$?"
echo "=== 产物 ==="
ls -lh src-tauri/target/release/bundle/deb/*.deb 2>/dev/null
ls -lh src-tauri/target/release/bundle/appimage/*.AppImage 2>/dev/null
