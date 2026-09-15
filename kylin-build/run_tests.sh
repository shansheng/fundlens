#!/usr/bin/env bash
# FundLens (feat/kylin-v10-aarch64) 单测运行封装（复用 sysroot 前缀环境）
# 用法: bash kylin-build/run_tests.sh [额外的 cargo test 参数]
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
export MNN_LIB_DIR=/home/sheng/opt/mnn-build

cd "$SRC/src-tauri"
exec cargo test --lib "$@"
