#!/bin/zsh
# FundLens Android 出包脚本（v2.3.0）— 构建 + zipalign + apksigner 签名 + 校验 + 重命名
# 用脚本自身所在目录定位仓库根（原先写死旧机器路径 /Users/sheng/WorkBuddy/...，
# 换机后脚本直接 cd 失败；与 fl-build-desktop.sh 保持一致的自适应写法）
cd "$(dirname "$0")" || exit 1
set -e
# 版本号从 tauri.conf.json 自动读取（此前硬编码，发版时漏改会产出错误命名的 APK）
VER=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')
if [ -z "$VER" ]; then echo "[fl-android] 无法从 tauri.conf.json 读取版本号"; exit 1; fi

# ⛔ 出包前锁文件自检
# v2.6.10~v2.6.14 曾因 commit 6927683 把麒麟分支（Tauri 1）的 Cargo.lock 提交进 main，
# tauri CLI 直接以 "tauri (v1.8.3) : @tauri-apps/api (v2.11.1)" 退出，整段 version 无法出包。
# Android 与 macOS 同处 main，必须锁 tauri 2.x。
TAURI_LOCK_VER=$(awk '/^name = "tauri"$/{getline; gsub(/[^0-9.]/,"",$0); print; exit}' src-tauri/Cargo.lock)
case "$TAURI_LOCK_VER" in
  2.*) ;;
  *)
    echo "[fl-android] ⛔ Cargo.lock 中 tauri=$TAURI_LOCK_VER，main 分支应为 2.x。"
    echo "             参见 build-lockfile-fix-v2.6.14-2026-09-15.md"
    exit 1
    ;;
esac

# ⛔ OCR 模型同步自检
# src-tauri/gen/android/app/src/main/assets/ocr/ 是**被 git 跟踪**的 Android 资源副本。
# 它不随 resources/ocr/ 自动更新——v2.6.9 升 PP-OCRv5 时只改了 resources/ocr/，
# 该副本仍是 v4 权重，导致 APK 内装的其实是旧模型，而 git 上完全看不出来。
# 这里在构建前强制同步 + 校验，避免静默装错模型。
SRC_OCR=src-tauri/resources/ocr
GEN_OCR=src-tauri/gen/android/app/src/main/assets/ocr
if [ ! -f "$SRC_OCR/det.mnn" ]; then
  echo "[fl-android] ⛔ 源模型缺失：$SRC_OCR/det.mnn 不存在，请先运行 src-tauri/download_ocr_models.sh"
  exit 1
fi
mkdir -p "$GEN_OCR"
for f in det.mnn rec.mnn dict.txt cls.mnn MODEL_VERSION; do
  [ -f "$SRC_OCR/$f" ] || continue
  cp -f "$SRC_OCR/$f" "$GEN_OCR/$f"
done
for f in det.mnn rec.mnn dict.txt cls.mnn; do
  a=$(shasum -a 256 "$SRC_OCR/$f" | awk '{print $1}')
  b=$(shasum -a 256 "$GEN_OCR/$f" | awk '{print $1}')
  if [ "$a" != "$b" ]; then
    echo "[fl-android] ⛔ OCR 资源同步失败：$f 哈希不一致 ($a != $b)"
    exit 1
  fi
done
OCR_VER=$(cat "$SRC_OCR/MODEL_VERSION" 2>/dev/null || echo unknown)

# --- Android SDK 定位（原先两处都写死 $HOME/Library/Android/sdk；换机后 cask 装在
#     /opt/homebrew/share/android-commandlinetools，写死会静默拿到空 ZIPALIGN）---
SDK_ROOT="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$SDK_ROOT" ] || [ ! -d "$SDK_ROOT/build-tools" ]; then
  for cand in "$HOME/Library/Android/sdk" /opt/homebrew/share/android-commandlinetools; do
    if [ -d "$cand/build-tools" ]; then SDK_ROOT="$cand"; break; fi
  done
fi
if [ -z "$SDK_ROOT" ] || [ ! -d "$SDK_ROOT/build-tools" ]; then
  echo "[fl-android] ⛔ 未找到 Android SDK（build-tools 目录不存在）"
  echo "             装法：brew install --cask android-commandlinetools && sdkmanager 'build-tools;36.0.0'"
  exit 1
fi
export ANDROID_HOME="$SDK_ROOT"
export ANDROID_SDK_ROOT="$SDK_ROOT"
NDK_DIR=$(ls -d "$SDK_ROOT"/ndk/* 2>/dev/null | tail -1)
if [ -z "$NDK_DIR" ]; then
  echo "[fl-android] ⛔ 未找到 Android NDK（$SDK_ROOT/ndk 为空），Tauri 交叉编译必须"
  exit 1
fi
export NDK_HOME="$NDK_DIR"
echo "[fl-android] sdk=$SDK_ROOT ndk=$(basename "$NDK_DIR")"

# --- Rust 交叉编译工具链 ---
# ⛔ brew 装的 rust（/opt/homebrew/bin/cargo）是完整发行版但**没有 rustup target 机制**，
#    不含 aarch64-linux-android 的 std ⇒ cargo 编 android 会直接报 "can't find crate for `std`"。
#    必须用 rustup 装的 toolchain。优先 1.99.1（与 brew rustc 同版本，Cargo.lock 行为一致）。
TC_ROOT=""
for cand in "$HOME/.rustup/toolchains/1.99.1-aarch64-apple-darwin" "$HOME"/.rustup/toolchains/*; do
  if [ -x "$cand/bin/rustc" ] && [ -d "$cand/lib/rustlib/aarch64-linux-android" ]; then
    TC_ROOT="$cand"; break
  fi
done
if [ -z "$TC_ROOT" ]; then
  echo "[fl-android] ⛔ 缺 Rust android 交叉编译工具链（需 rustup toolchain + aarch64-linux-android target）"
  echo "             装法：rustup toolchain install 1.99.1 --profile minimal --target aarch64-linux-android"
  exit 1
fi
export PATH="$TC_ROOT/bin:$PATH"
echo "[fl-android] rust_toolchain=$(basename "$TC_ROOT")"

# ⛔ CMake 4.x 已移除对 cmake_minimum_required(<3.5) 的兼容，而 OCR 依赖 MNN 2.8.3
#    的 CMakeLists 声明的是 VERSION 3.0 → 不设此下限会直接失败：
#    "Compatibility with CMake < 3.5 has been removed from CMake."
export CMAKE_POLICY_VERSION_MINIMUM=3.5

echo "[fl-android] build start: $(date '+%F %T') version=$VER tauri_lock=$TAURI_LOCK_VER ocr_model=$OCR_VER"
npx tauri android build -t aarch64 --apk
APK_DIR=src-tauri/gen/android/app/build/outputs/apk/universal/release
UNSIGNED=$APK_DIR/app-universal-release-unsigned.apk
ALIGNED=$APK_DIR/app-universal-release-aligned.apk
SIGNED=$APK_DIR/app-universal-release-signed.apk
# 用 sort -V 而非 ls|tail：字典序下 build-tools/9.x 会排在 36.x 之后，取错版本
ZIPALIGN=$(ls "$SDK_ROOT"/build-tools/*/zipalign 2>/dev/null | sort -V | tail -1)
if [ ! -x "$ZIPALIGN" ]; then
  echo "[fl-android] ⛔ 未找到 zipalign（$SDK_ROOT/build-tools/*/zipalign）"
  exit 1
fi
APKSIGNER_DIR=$(dirname "$ZIPALIGN")

# --- 签名凭据 ---
# ⛔ 密码**不得写进本脚本**：脚本入库（换机后 pull 即用），仓库必须保持零凭据。
#    本脚本原先因含密码明文被 .gitignore 忽略 → 从没入库 → 换机后整段失效。
# 密码来源优先级：
#   1) 环境变量 FUNDLENS_KS_PASS（CI / 单次覆盖，不落盘）
#   2) ~/.gradle/gradle.properties 的 fundlensKeystorePassword（本机常驻，Android 官方推荐位置）
# 首次配置：
#   printf 'fundlensKeystorePassword=<密码>\n' >> ~/.gradle/gradle.properties && chmod 600 ~/.gradle/gradle.properties
# ⚠️ 签名一旦变更，手机上的旧版本无法覆盖安装（Android 强制同签名），须先卸载重装（清空本地库）。
KS="${FUNDLENS_KS:-$HOME/.fundlens-android.keystore}"
KS_ALIAS="${FUNDLENS_KS_ALIAS:-fundlens}"
if [ ! -f "$KS" ]; then
  echo "[fl-android] ⛔ 签名 keystore 缺失：$KS"
  echo "             未签名 APK 已生成，可先用它验证；正式出包需恢复此 keystore。"
  exit 1
fi
KS_PASS="${FUNDLENS_KS_PASS:-}"
GP="$HOME/.gradle/gradle.properties"
if [ -z "$KS_PASS" ] && [ -f "$GP" ]; then
  KS_PASS=$(grep -E '^[[:space:]]*fundlensKeystorePassword[[:space:]]*=' "$GP" | tail -1 | sed -E 's/^[^=]*=[[:space:]]*//' | tr -d '\r' | tr -d '"')
fi
if [ -z "$KS_PASS" ]; then
  echo "[fl-android] ⛔ 未配置签名密码（keystore 在，但密码不随仓库走）"
  echo "             常驻配置：printf 'fundlensKeystorePassword=<密码>\\n' >> ~/.gradle/gradle.properties"
  echo "             单次覆盖：FUNDLENS_KS_PASS=<密码> zsh fl-build-android.sh"
  exit 1
fi
# 预检：密码/别名错误在此明确报出，而不是让 apksigner 抛 "Keystore was tampered with"
if ! keytool -list -keystore "$KS" -storepass "$KS_PASS" -alias "$KS_ALIAS" >/dev/null 2>&1; then
  echo "[fl-android] ⛔ keystore 校验失败：密码错误或别名不对（当前别名 $KS_ALIAS）"
  echo "             查看真实别名：keytool -list -keystore $KS -storepass <密码>"
  exit 1
fi
# 打印证书指纹前 17 位，出包后可与「上次出包记录」对照，确认没换章
KS_SHA=$(keytool -list -v -keystore "$KS" -storepass "$KS_PASS" -alias "$KS_ALIAS" 2>/dev/null | grep -m1 "SHA256:" | sed -E 's/.*SHA256:[[:space:]]*//')
echo "[fl-android] keystore ok: alias=$KS_ALIAS sha256=${KS_SHA:0:17}..."

echo "[fl-android] zipalign..."
"$ZIPALIGN" -f -p 4 "$UNSIGNED" "$ALIGNED"
echo "[fl-android] apksigner sign..."
"$APKSIGNER_DIR/apksigner" sign --ks "$KS" --ks-key-alias "$KS_ALIAS" \
  --ks-pass "pass:$KS_PASS" --key-pass "pass:$KS_PASS" --out "$SIGNED" "$ALIGNED"
echo "[fl-android] verify..."
"$APKSIGNER_DIR/apksigner" verify --print-certs "$SIGNED" | head -4
cp "$SIGNED" "FundLens-${VER}-arm64.apk"

# ⛔ 出包后终检：断言 APK **内部**装的模型与 resources/ocr 完全一致。
# 这是唯一能证明「用户装到的模型是对的」的检查（间接检查 gen/ 副本仍可能漏掉打包环节）。
for f in det.mnn rec.mnn; do
  a=$(shasum -a 256 "$SRC_OCR/$f" | awk '{print $1}')
  b=$(unzip -p "FundLens-${VER}-arm64.apk" "assets/ocr/$f" | shasum -a 256 | awk '{print $1}')
  if [ "$a" != "$b" ]; then
    echo "[fl-android] ⛔ APK 内 $f 与 $SRC_OCR/$f 不一致，打包环节可能用了陈旧副本！"
    exit 1
  fi
done
APK_OCR_VER=$(unzip -p "FundLens-${VER}-arm64.apk" assets/ocr/MODEL_VERSION 2>/dev/null || echo unknown)
if [ "$APK_OCR_VER" != "$OCR_VER" ]; then
  echo "[fl-android] ⛔ APK 内 MODEL_VERSION=$APK_OCR_VER 与源 $OCR_VER 不一致"
  exit 1
fi
echo "[fl-android] ocr check ok: model=$APK_OCR_VER (det/rec sha256 与 $SRC_OCR 一致)"
echo "[fl-android] done: $(ls -la "FundLens-${VER}-arm64.apk" | awk '{print $5}') bytes @ $(date '+%F %T')"
