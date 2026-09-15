#!/bin/zsh
# FundLens 桌面版构建脚本（2.3.0）
# 背景：CLT 21 更新后 /Library/Developer/CommandLineTools/usr/include/c++/v1 只剩空壳，
# C++ 头文件真实位置在 SDK 内，cc-rs（clipper-sys/rusto-rs OCR 链路）必须显式 -isystem。
cd /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens || exit 1
export CXXFLAGS="-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1"

# ⛔ 分支守卫：本脚本只适用于 main（Tauri 2）
# 麒麟分支 feat/kylin-v10-aarch64 是 Tauri 1 永久分叉，本机 tauri build 会抹掉
# tauri.conf.json 的 dialog-all feature（铁证 aaa0de1）；麒麟打包必须走
# Docker fl-build(ubuntu20.04) + arm64-build/inc-build.sh。这里提前拦下，避免
# 后续的 tauri 2.x 锁文件检查给出误导性报错。
CUR_BRANCH=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)
if [ "$CUR_BRANCH" = "feat/kylin-v10-aarch64" ]; then
  echo "[fl-build] ⛔ 当前分支为 $CUR_BRANCH（Tauri 1 永久分叉），不可用本脚本出包。"
  echo "           麒麟权威打包：Docker fl-build(ubuntu20.04) + arm64-build/inc-build.sh"
  echo "           本机直接 tauri build 会破坏 dialog-all feature（见 commit aaa0de1）。"
  exit 1
fi

# ⛔ 出包前锁文件自检
# v2.6.10~v2.6.14 曾因 commit 6927683 把麒麟分支（Tauri 1）的 Cargo.lock 提交进 main，
# 导致 tauri CLI 在自检阶段直接退出：
#   Found version mismatched Tauri packages ... tauri (v1.8.3) : @tauri-apps/api (v2.11.1)
# 该锁文件与 Cargo.toml 的 tauri = "2" 冲突，且 git diff 上不易察觉（Cargo.lock 不与
# tauri.conf.json 同处一个冲突文件），整段 version 都无法出包。
# 规则：main（macOS / Android）必须锁 tauri 2.x；麒麟分支必须锁 1.8.3。
# 详见 build-lockfile-fix-v2.6.14-2026-09-15.md
TAURI_LOCK_VER=$(awk '/^name = "tauri"$/{getline; gsub(/[^0-9.]/,"",$0); print; exit}' src-tauri/Cargo.lock)
case "$TAURI_LOCK_VER" in
  2.*) ;;
  *)
    echo "[fl-build] ⛔ Cargo.lock 中 tauri=$TAURI_LOCK_VER，main 分支应为 2.x。"
    echo "           疑似把麒麟 feat/kylin-v10-aarch64 的 Tauri1 锁文件提交进来了。"
    echo "           修复：git checkout <最后一个可构建的 main 提交> -- src-tauri/Cargo.lock"
    echo "                 再仅把 [[package]] name = \"fundlens\" 的 version 改为当前版本。"
    echo "           参见 build-lockfile-fix-v2.6.14-2026-09-15.md"
    exit 1
    ;;
esac

echo "[fl-build] cwd=$(pwd) version=$(python3 -c "import json;print(json.load(open('package.json'))['version'])") tauri_lock=$TAURI_LOCK_VER"
npm run tauri build
