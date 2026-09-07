#!/bin/zsh
# FundLens 桌面版构建脚本（2.2.0）— 由基金穿透 P0 发版创建
# 背景：CLT 21 更新后 /Library/Developer/CommandLineTools/usr/include/c++/v1 只剩空壳，
# C++ 头文件真实位置在 SDK 内，cc-rs（clipper-sys/rusto-rs OCR 链路）必须显式 -isystem。
cd /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens || exit 1
export CXXFLAGS="-isystem /Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/usr/include/c++/v1"
echo "[fl-build] cwd=$(pwd) version=$(python3 -c "import json;print(json.load(open('package.json'))['version'])")"
npm run tauri build
