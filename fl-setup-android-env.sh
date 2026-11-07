#!/bin/zsh
# FundLens Android 构建环境一键搭建（macOS arm64）
#
# 用途：换机 / 重装系统后，一条命令把 Android 出包环境从零拉齐，
#       之后 `zsh fl-build-android.sh` 即可直接出包。
#
#   zsh fl-setup-android-env.sh
#
# 幂等：每一步先检测，已满足就打印 [skip] 跳过，可反复执行。
# ⛔ 本脚本**不写任何凭据**：签名密码需你自己放进 ~/.gradle/gradle.properties（脚本末尾会检查并提示）。
#
# 背景（2026-10-17 换机后从零重建的真实教训，全部固化在这里）：
#   1. brew 装的 rust 是完整发行版但**没有 rustup target 机制**，不含
#      aarch64-linux-android 的 std ⇒ cargo 编 android 直接报 "can't find crate for `std`"。
#      必须用 rustup 装的 toolchain。
#   2. brew cask 的 SDK 在 /opt/homebrew/share/android-commandlinetools，
#      **不是** ~/Library/Android/sdk（后者是 Android Studio 的布局）。
#   3. services.gradle.org 在国内只有 ~40KB/s，gradle 发行版 137MB 要下 17 分钟；
#      腾讯云镜像可达 10MB/s。gradle wrapper 按 distributionUrl 的 md5(base36) 建缓存目录，
#      所以可以直接把 zip 塞进那个目录预置。
#   4. maven 依赖走阿里云镜像（~/.gradle/init.gradle），否则首次构建卡在 repo.maven.apache.org。
#   5. OCR 依赖的 MNN 在 Android 目标没有预编译库，必须源码编译；而
#      rusto-mnn-sys/build.rs 会从 cargo registry 目录逐级向上找 vendor/mnn/CMakeLists.txt，
#      找不到才 clone GitHub（本机 github.com 不通、codeload.github.com 通）。
#      ⇒ 下载 tarball 放到**持久目录**再软链进 registry（⛔ 别放 /tmp，会被系统清理，
#      软链变成断链后全量重建才暴露，排查成本很高）。
set -e
# zsh 默认 nomatch：glob 不匹配会直接报错终止（如全新机器上 cargo registry 尚不存在）。
# 这些 glob 本来就是「有就处理、没有就跳过」的语义。
setopt null_glob 2>/dev/null || true
cd "$(dirname "$0")" || exit 1

# ---------------------------------------------------------------- 可覆盖的版本参数
COMPILE_SDK="${COMPILE_SDK:-36}"           # 与 gen/android/app/build.gradle.kts 的 compileSdk 对齐
BUILD_TOOLS="${BUILD_TOOLS:-36.0.0}"
NDK_VER="${NDK_VER:-27.2.12479018}"        # AGP 8.x + Tauri 2 推荐线
RUST_TC="${RUST_TC:-1.99.1}"               # 与 brew rustc 同版本，Cargo.lock 行为一致
MNN_VER="${MNN_VER:-2.8.3}"                # rusto-mnn-sys 使用的 MNN 版本
DEPS_DIR="$HOME/Library/Application Support/FundLens/build-deps"
GRADLE_MIRROR="${GRADLE_MIRROR:-https://mirrors.cloud.tencent.com/gradle}"

step() { echo ""; echo "──── [$1] $2"; }
ok()   { echo "  ✓ $1"; }
skip() { echo "  [skip] $1"; }
warn() { echo "  ⚠️  $1"; }
die()  { echo ""; echo "  ⛔ $1"; exit 1; }

# ---------------------------------------------------------------- [0] 前置检查
step 0/7 "前置检查"
[ "$(uname -s)" = "Darwin" ] || die "本脚本只支持 macOS（当前 $(uname -s)）"
[ "$(uname -m)" = "arm64" ]  || warn "当前架构 $(uname -m)，脚本按 arm64 编写，Apple Silicon 才建议继续"
command -v brew >/dev/null 2>&1 || die "缺 Homebrew。装法见 https://brew.sh"
ok "macOS $(sw_vers -productVersion) / $(uname -m) / brew $(brew --version | head -1 | awk '{print $2}')"
xcode-select -p >/dev/null 2>&1 && ok "Xcode CLT: $(xcode-select -p)" || die "缺 Xcode Command Line Tools：xcode-select --install"

# ---------------------------------------------------------------- [1] Homebrew 依赖
step 1/7 "Homebrew 依赖（android-commandlinetools / rustup / cmake / openjdk@21）"
have_formula() { brew list --versions "$1" >/dev/null 2>&1; }
have_cask()    { brew list --cask --versions "$1" >/dev/null 2>&1; }

if have_cask android-commandlinetools; then
  skip "android-commandlinetools $(brew list --cask --versions android-commandlinetools | awk '{print $2}')"
else
  echo "  安装 android-commandlinetools（含 sdkmanager）..."
  brew install --cask android-commandlinetools
fi
for f in rustup cmake; do
  if have_formula "$f"; then
    skip "$f $(brew list --versions "$f" | awk '{print $2}')"
  else
    echo "  安装 $f ..."
    brew install "$f"
  fi
done
if have_formula openjdk@21; then
  skip "openjdk@21（Gradle 需要 JDK 17+）"
else
  echo "  安装 openjdk@21 ..."
  brew install openjdk@21
fi
if /usr/libexec/java_home >/dev/null 2>&1; then
  ok "JDK: $(/usr/libexec/java_home)"
else
  warn "系统未注册 JDK（Gradle 会找不到 java）。执行："
  warn "  sudo ln -sfn /opt/homebrew/opt/openjdk@21/libexec/openjdk.jdk /Library/Java/JavaVirtualMachines/openjdk-21.jdk"
fi

# ---------------------------------------------------------------- [2] Android SDK 组件
step 2/7 "Android SDK 组件"
SDK_ROOT="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$SDK_ROOT" ] || [ ! -d "$SDK_ROOT/build-tools" ]; then
  for cand in /opt/homebrew/share/android-commandlinetools "$HOME/Library/Android/sdk"; do
    if [ -d "$cand/cmdline-tools" ]; then SDK_ROOT="$cand"; break; fi
  done
fi
[ -n "$SDK_ROOT" ] && [ -d "$SDK_ROOT" ] || die "找不到 Android SDK 目录，请检查 android-commandlinetools 是否装好"
SDKMANAGER="$SDK_ROOT/cmdline-tools/latest/bin/sdkmanager"
[ -x "$SDKMANAGER" ] || SDKMANAGER=$(command -v sdkmanager 2>/dev/null || true)
[ -n "$SDKMANAGER" ] && [ -x "$SDKMANAGER" ] || die "找不到 sdkmanager（$SDK_ROOT/cmdline-tools/latest/bin/）"
ok "SDK: $SDK_ROOT"
export ANDROID_HOME="$SDK_ROOT" ANDROID_SDK_ROOT="$SDK_ROOT"

# 接受许可（非交互），否则 sdkmanager install 会卡在 y/n
yes 2>/dev/null | "$SDKMANAGER" --licenses >/dev/null 2>&1 || true

# 逐个组件检测后再装，避免每次 setup 都触发网络
NEED=()
[ -d "$SDK_ROOT/platform-tools" ]          || NEED+=("platform-tools")
[ -d "$SDK_ROOT/platforms/android-$COMPILE_SDK" ] || NEED+=("platforms;android-$COMPILE_SDK")
[ -x "$SDK_ROOT/build-tools/$BUILD_TOOLS/aapt2" ] || NEED+=("build-tools;$BUILD_TOOLS")
[ -d "$SDK_ROOT/ndk/$NDK_VER" ]            || NEED+=("ndk;$NDK_VER")
if [ ${#NEED[@]} -eq 0 ]; then
  skip "platform-tools / platforms;android-$COMPILE_SDK / build-tools;$BUILD_TOOLS / ndk;$NDK_VER 均已安装"
else
  echo "  安装：${NEED[*]}（NDK 约 2GB，请耐心）"
  "$SDKMANAGER" "${NEED[@]}" 2>&1 | tail -5
fi
[ -x "$SDK_ROOT/build-tools/$BUILD_TOOLS/zipalign" ] && ok "zipalign / apksigner: build-tools/$BUILD_TOOLS"
[ -d "$SDK_ROOT/ndk/$NDK_VER" ] && ok "NDK: $NDK_VER"

# ---------------------------------------------------------------- [3] Rust 交叉编译工具链
step 3/7 "Rust 交叉编译工具链（$RUST_TC + aarch64-linux-android）"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_BIN=""
for cand in /opt/homebrew/opt/rustup/bin/rustup "$HOME/.cargo/bin/rustup" /opt/homebrew/bin/rustup; do
  [ -x "$cand" ] && { RUSTUP_BIN="$cand"; break; }
done
[ -n "$RUSTUP_BIN" ] || die "找不到 rustup（brew install rustup 后重跑本脚本）"
TC_DIR="$RUSTUP_HOME/toolchains/$RUST_TC-aarch64-apple-darwin"
if [ -x "$TC_DIR/bin/rustc" ] && [ -d "$TC_DIR/lib/rustlib/aarch64-linux-android" ]; then
  skip "toolchain $RUST_TC + android target 已就绪（$($TC_DIR/bin/rustc --version)）"
else
  echo "  安装 toolchain $RUST_TC + aarch64-linux-android ..."
  # ⚠️ 若之前有残留的后台 rustup 进程，会与本命令抢锁并把 toolchain 删成空壳
  #    （bin 只剩 cargo、lib/rustlib 消失 ⇒ 链接报 "no such file: libstd-<hash>.rlib"）
  if pgrep -f "rustup toolchain install" >/dev/null 2>&1; then
    warn "检测到其他 rustup 安装进程，先终止避免抢锁"
    pkill -9 -f "rustup toolchain install" 2>/dev/null || true
    sleep 1
  fi
  "$RUSTUP_BIN" toolchain install "$RUST_TC" --profile minimal --target aarch64-linux-android
fi
[ -x "$TC_DIR/bin/rustc" ] && [ -d "$TC_DIR/lib/rustlib/aarch64-linux-android/lib" ] \
  && ok "toolchain $("$TC_DIR/bin/rustc" --version) + android std" \
  || die "toolchain 不完整（可能是抢锁导致的空壳）。修复：rustup toolchain uninstall $RUST_TC 后重跑"

# ---------------------------------------------------------------- [4] Gradle 发行版预置
step 4/7 "Gradle 发行版（走国内镜像预置到 wrapper 缓存）"
WRAPPER_PROPS="src-tauri/gen/android/gradle/wrapper/gradle-wrapper.properties"
GRADLE_URL=""
if [ -f "$WRAPPER_PROPS" ]; then
  # properties 里 URL 被转义成 https\://，计算哈希前必须还原（反斜杠全去掉）
  GRADLE_URL=$(grep -m1 '^distributionUrl' "$WRAPPER_PROPS" | sed -E 's/^distributionUrl=//' | sed -E 's/\\//g' | tr -d '[:space:]')
fi
GRADLE_VER=$(echo "$GRADLE_URL" | sed -E 's#.*/gradle-([0-9]+\.[0-9]+(\.[0-9]+)?)-.*#\1#')
if [ -z "$GRADLE_VER" ] || [ "$GRADLE_VER" = "$GRADLE_URL" ]; then
  die "无法从 $WRAPPER_PROPS 解析 gradle 版本（读到的 distributionUrl=「$GRADLE_URL」）"
fi
ok "项目要求 gradle $GRADLE_VER"

# gradle wrapper 的缓存目录名 = md5(distributionUrl) 的 base36 表示
GRADLE_HASH=$(python3 - "$GRADLE_URL" <<'PY'
import hashlib, sys
n = int.from_bytes(hashlib.md5(sys.argv[1].encode()).digest(), 'big')
digits = "0123456789abcdefghijklmnopqrstuvwxyz"
s = ""
while n:
    s = digits[n % 36] + s
    n //= 36
print(s or "0")
PY
)
DIST="$HOME/.gradle/wrapper/dists/gradle-$GRADLE_VER-bin/$GRADLE_HASH"
if [ -x "$DIST/gradle-$GRADLE_VER/bin/gradle" ]; then
  skip "gradle $GRADLE_VER 已就绪（$DIST）"
else
  mkdir -p "$DIST"
  ZIP="$DIST/gradle-$GRADLE_VER-bin.zip"
  echo "  从镜像下载 gradle-$GRADLE_VER-bin.zip ..."
  echo "  源: $GRADLE_MIRROR/gradle-$GRADLE_VER-bin.zip"
  if ! curl -fL --max-time 900 -o "$ZIP" "$GRADLE_MIRROR/gradle-$GRADLE_VER-bin.zip" \
        -w "  http=%{http_code} size=%{size_download} speed=%{speed_download}B/s\n"; then
    warn "镜像下载失败，回退官方源（会很慢）"
    curl -fL --max-time 1800 -o "$ZIP" "$GRADLE_URL" || die "gradle 发行版下载失败"
  fi
  # wrapper 判定就绪的标志：解压后的目录 + *.zip.ok 标记文件
  ( cd "$DIST" && unzip -q "$ZIP" ) || die "gradle 解压失败（$ZIP 可能不完整）"
  touch "$DIST/gradle-$GRADLE_VER-bin.zip.ok"
  ok "gradle $GRADLE_VER 已预置（$(du -sh "$DIST" | awk '{print $1}')）"
fi

# ---------------------------------------------------------------- [5] Maven 国内镜像
step 5/7 "Gradle 全局配置（阿里云 maven 镜像）"
INIT_GRADLE="$HOME/.gradle/init.gradle"
mkdir -p "$HOME/.gradle"
if [ -f "$INIT_GRADLE" ] && grep -q "aliyun" "$INIT_GRADLE"; then
  skip "已有 aliyun 镜像配置（$INIT_GRADLE）"
else
  [ -f "$INIT_GRADLE" ] && { /bin/cp "$INIT_GRADLE" "$INIT_GRADLE.bak.$(date +%Y%m%d-%H%M%S)"; warn "已备份原 init.gradle"; }
  cat > "$INIT_GRADLE" <<'GRADLE_EOF'
// 国内网络加速（由 fl-setup-android-env.sh 生成）
// 背景：repo.maven.apache.org 在本机只有 ~40KB/s，阿里云镜像可达 MB/s 级
def mirrors = [
    'https://maven.aliyun.com/repository/google',
    'https://maven.aliyun.com/repository/public',
    'https://maven.aliyun.com/repository/gradle-plugin',
]

settingsEvaluated { settings ->
    settings.pluginManagement {
        repositories {
            mirrors.each { u -> maven { url u } }
            gradlePluginPortal()
            google()
            mavenCentral()
        }
    }
}

allprojects {
    buildscript {
        repositories {
            mirrors.each { u -> maven { url u } }
            google()
            mavenCentral()
        }
    }
    repositories {
        mirrors.each { u -> maven { url u } }
        google()
        mavenCentral()
    }
}
GRADLE_EOF
  ok "已写入 $INIT_GRADLE"
fi

# ---------------------------------------------------------------- [6] MNN 源码（OCR 依赖）
step 6/7 "MNN $MNN_VER 源码（OCR 的 rusto-mnn-sys 在 Android 无预编译库）"
mkdir -p "$DEPS_DIR"
MNN_SRC="$DEPS_DIR/MNN-$MNN_VER"
if [ -f "$MNN_SRC/CMakeLists.txt" ]; then
  skip "MNN 源码已在 $MNN_SRC（$(du -sh "$MNN_SRC" 2>/dev/null | awk '{print $1}')）"
else
  TB="$DEPS_DIR/mnn-$MNN_VER.tar.gz"
  echo "  下载 MNN $MNN_VER（github.com 本机不通，走 codeload）..."
  curl -fL --max-time 900 -o "$TB" \
    "https://codeload.github.com/alibaba/MNN/tar.gz/refs/tags/$MNN_VER" \
    -w "  http=%{http_code} size=%{size_download}\n" || die "MNN 下载失败（codeload.github.com 是否可达？）"
  tar -xzf "$TB" -C "$DEPS_DIR" || die "MNN 解压失败"
  rm -f "$TB"
  [ -f "$MNN_SRC/CMakeLists.txt" ] || die "解压后未见 $MNN_SRC/CMakeLists.txt"
  ok "MNN 源码就位（$(du -sh "$MNN_SRC" | awk '{print $1}')）"
fi
# ⛔ 必须链进 cargo registry 目录：build.rs 是从 crate 目录**逐级向上**找 vendor/mnn，
#    放到项目 src-tauri/vendor 里它找不到。
LINKED=0
for REG in "$HOME"/.cargo/registry/src/index.crates.io-*/; do
  [ -d "$REG" ] || continue
  mkdir -p "$REG/vendor"
  ln -sfn "$MNN_SRC" "$REG/vendor/mnn"
  [ -f "$REG/vendor/mnn/CMakeLists.txt" ] || die "软链后仍读不到 $REG/vendor/mnn/CMakeLists.txt"
  LINKED=$((LINKED+1))
done
if [ "$LINKED" -gt 0 ]; then
  ok "已软链到 $LINKED 个 cargo registry（vendor/mnn -> $MNN_SRC）"
else
  warn "未找到 cargo registry 目录（$HOME/.cargo/registry/src/index.crates.io-*）"
  warn "先跑一次 cargo fetch / 构建，让 registry 建立后再重跑本脚本"
fi

# ---------------------------------------------------------------- [7] 签名凭据与自检
step 7/7 "签名凭据与最终自检"
KS="$HOME/.fundlens-android.keystore"
GP="$HOME/.gradle/gradle.properties"
if [ -f "$KS" ]; then
  ok "keystore: $KS（$(ls -l "$KS" | awk '{print $5}') bytes）"
else
  warn "缺签名 keystore：$KS"
  warn "  ⛔ 这是唯一不可再生的资产（丢了无法从 APK 反推私钥，只能卸载重装并清空本地库）"
  warn "  只能从异地备份恢复；确需新建时注意手机须先卸载旧版才能安装"
fi
if [ -f "$GP" ] && grep -qE '^[[:space:]]*fundlensKeystorePassword[[:space:]]*=' "$GP"; then
  ok "签名密码已配置（$GP，权限 $(ls -l "$GP" | cut -c1-10)）"
  PERM=$(ls -l "$GP" | cut -c1-10)
  [ "$PERM" = "-rw-------" ] || warn "建议收紧权限：chmod 600 $GP"
else
  warn "未配置签名密码，出包会在签名阶段停止。执行："
  warn "  printf 'fundlensKeystorePassword=<密码>\\n' >> $GP && chmod 600 $GP"
fi

echo ""
echo "════════════ 环境自检 ════════════"
echo "  - Homebrew        : $(brew --version | head -1 | awk '{print $2}')"
echo "  - Android SDK     : $SDK_ROOT"
echo "  - build-tools     : $([ -x "$SDK_ROOT/build-tools/$BUILD_TOOLS/aapt2" ] && echo "$BUILD_TOOLS ✓" || echo "缺")"
echo "  - NDK             : $([ -d "$SDK_ROOT/ndk/$NDK_VER" ] && echo "$NDK_VER ✓" || echo "缺")"
echo "  - Rust toolchain  : $([ -x "$TC_DIR/bin/rustc" ] && echo "$RUST_TC ✓" || echo "缺")"
echo "  - android std     : $([ -d "$TC_DIR/lib/rustlib/aarch64-linux-android" ] && echo "✓" || echo "缺")"
echo "  - Gradle          : $([ -x "$DIST/gradle-$GRADLE_VER/bin/gradle" ] && echo "$GRADLE_VER ✓" || echo "缺")"
echo "  - Maven 镜像      : $([ -f "$INIT_GRADLE" ] && echo "✓ $INIT_GRADLE" || echo "缺")"
echo "  - MNN 源码        : $([ -f "$MNN_SRC/CMakeLists.txt" ] && echo "$MNN_VER ✓" || echo "缺")"
echo "  - CMake           : $(cmake --version 2>/dev/null | head -1 | awk '{print $3}')"
echo "  - JDK             : $(/usr/libexec/java_home 2>/dev/null || echo '未注册')"
echo "══════════════════════════════════"
echo ""
echo "下一步：zsh fl-build-android.sh"
echo "  首次全量构建会把 MNN 从源码编译一遍，预计 15~30 分钟；之后增量约 30 秒。"
