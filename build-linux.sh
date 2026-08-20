#!/usr/bin/env bash
#
# FundLens - ARM64 Linux one-shot build script
# ============================================================
# Target: native build on an ARM64 (aarch64) Linux host.
# Verified path: Ubuntu 22.04+ / Debian 12+ (apt).
# Built-in (untested) paths: Fedora/RHEL (dnf), Alpine (apk).
#
# Usage:
#   bash build-linux.sh                   # native build on an aarch64 host
#   DRY_RUN=1 bash build-linux.sh         # print commands only, do not execute
#   SKIP_DEPS=1 bash build-linux.sh       # skip system dependency install
#   SKIP_TOOLCHAIN=1 bash build-linux.sh  # skip Rust / Node install
#   CROSS=1 bash build-linux.sh           # cross-compile aarch64 from x86_64
#                                         #   (needs a prepared aarch64 sysroot/linker)
#
# Artifacts: src-tauri/target/release/bundle/{deb,appimage}/
#
# Notes:
#   - Bundled SQLite (rusqlite "bundled") and TLS (reqwest rustls) mean no
#     system sqlite/openssl runtime is required; this script only installs
#     build-time dependencies.
#   - FundLens performs all valuation locally; it depends on no third-party
#     valuation API.
set -euo pipefail

# ---------- configurable (override via env) ----------
DRY_RUN="${DRY_RUN:-0}"
SKIP_DEPS="${SKIP_DEPS:-0}"
SKIP_TOOLCHAIN="${SKIP_TOOLCHAIN:-0}"
CROSS="${CROSS:-0}"
NODE_MAJOR="${NODE_MAJOR:-22}"

# test-only hook (DRY_RUN=1): simulate aarch64 on a non-arm host
if [ "${DRY_RUN:-0}" = "1" ] && [ -n "${ARCH_OVERRIDE:-}" ]; then
  ARCH="$ARCH_OVERRIDE"
else
  ARCH="$(uname -m)"
fi

# ---------- output helpers ----------
log()  { printf '\033[1;34m[FundLens build]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[FundLens build]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[FundLens build]\033[0m WARN: %s\n' "$*"; }
err()  { printf '\033[1;31m[FundLens build]\033[0m ERROR: %s\n' "$*" >&2; }

# run: print under DRY_RUN, execute otherwise
run() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '  $ %s\n' "$*"
  else
    eval "$@"
  fi
}

# ---------- 0. architecture guard ----------
log "detected architecture: $ARCH"
if [ "$ARCH" != "aarch64" ]; then
  if [ "$CROSS" = "1" ]; then
    warn "host is $ARCH but CROSS=1: producing aarch64 via cross-compile (needs aarch64 linker/sysroot)."
  else
    err "this script does a native build for ARM64 (aarch64) Linux hosts; host is $ARCH."
    err "to cross-compile from x86_64 set CROSS=1; otherwise run on an aarch64 machine."
    exit 1
  fi
fi

# ---------- 1. privilege / package manager detection ----------
if [ "$(id -u)" = "0" ]; then
  SUDO=""
else
  SUDO="sudo"
fi

if [ "${DRY_RUN:-0}" = "1" ] && [ -n "${PKG_MGR_OVERRIDE:-}" ]; then
  PKG_MGR="$PKG_MGR_OVERRIDE"
elif command -v apt-get >/dev/null 2>&1; then
  PKG_MGR="apt"
elif command -v dnf >/dev/null 2>&1; then
  PKG_MGR="dnf"
elif command -v apk >/dev/null 2>&1; then
  PKG_MGR="apk"
else
  err "no supported package manager found (apt/dnf/apk). Install deps manually, then rerun with SKIP_DEPS=1."
  exit 1
fi
log "package manager: $PKG_MGR"

# ---------- 2. system build dependencies ----------
# ⚠️ 麒麟适配分支：Tauri 1.x 使用 webkit2gtk-4.0（4.1 是 Tauri 2 的要求）。
# 银河麒麟 V10 SP1 只有 libwebkit2gtk-4.0-dev，装 4.1 会直接导致运行时无法启动。
install_apt() {
  run "$SUDO apt-get update"
  run "$SUDO apt-get install -y --no-install-recommends build-essential cmake curl wget file pkg-config libxdo-dev libssl-dev libwebkit2gtk-4.0-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf libfuse2"
}
install_dnf() {
  run "$SUDO dnf install -y gcc-c++ cmake curl wget file pkgconf-pkg-config openssl-devel webkit2gtk4.0-devel gtk3-devel libappindicator-gtk3-devel librsvg2-devel patchelf fuse"
}
install_apk() {
  run "$SUDO apk add --no-cache build-base cmake curl wget file pkgconf openssl-dev webkit2gtk-4.0-dev gtk+3.0-dev libappindicator-dev librsvg-dev patchelf fuse"
}

if [ "$SKIP_DEPS" = "1" ]; then
  warn "SKIP_DEPS=1: skipping system dependency install."
else
  log "[1/4] installing system dependencies ($PKG_MGR)"
  case "$PKG_MGR" in
    apt) install_apt ;;
    dnf) install_dnf ;;
    apk) install_apk ;;
  esac
fi

# ---------- 3. Rust toolchain ----------
if [ "$SKIP_TOOLCHAIN" = "1" ]; then
  warn "SKIP_TOOLCHAIN=1: skipping Rust / Node install."
else
  log "[2/4] installing Rust toolchain"
  if ! command -v cargo >/dev/null 2>&1; then
    run "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal"
  else
    ok "Rust already present: $(cargo --version 2>/dev/null || echo unknown)"
  fi
  if [ "$CROSS" = "1" ]; then
    run "rustup target add aarch64-unknown-linux-gnu"
  fi
fi
# ensure cargo is on PATH (whether just installed or pre-existing)
# shellcheck disable=SC1090
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env" 2>/dev/null || true

# ---------- 4. Node.js ----------
if [ "$SKIP_TOOLCHAIN" = "1" ]; then
  :
else
  log "[3/4] installing Node.js $NODE_MAJOR"
  if ! command -v node >/dev/null 2>&1 || [ "$(node -v 2>/dev/null | tr -d v | cut -d. -f1)" -lt 18 ]; then
    case "$PKG_MGR" in
      apt)
        run "curl -fsSL https://deb.nodesource.com/setup_${NODE_MAJOR}.x | $SUDO -E bash -"
        run "$SUDO apt-get install -y nodejs"
        ;;
      dnf)
        run "$SUDO dnf install -y nodejs npm"
        ;;
      *)
        err "package manager $PKG_MGR has no built-in NodeSource install; install Node 18+ manually then rerun (or SKIP_TOOLCHAIN=1)."
        exit 1
        ;;
    esac
  else
    ok "Node already present: $(node -v 2>/dev/null || echo unknown)"
  fi
fi

# ---------- 5. OCR model weights (local PaddleOCR / PP-OCRv4) ----------
# Downloads MNN-format model weights into src-tauri/resources/ocr. If the
# download fails (e.g. no network), we still build; OCR will report "models
# missing" at runtime instead of crashing.
if [ "$SKIP_DEPS" = "1" ]; then
  warn "SKIP_DEPS=1: skipping OCR model download (run src-tauri/download_ocr_models.sh later)."
else
  log "downloading PaddleOCR (PP-OCRv4) model weights"
  if [ -f src-tauri/download_ocr_models.sh ]; then
    if ! bash src-tauri/download_ocr_models.sh; then
      warn "OCR model download failed; the app will build but local OCR will be unavailable until models are present."
    fi
  fi
fi

# ---------- 6. frontend deps + Tauri build (with OCR feature) ----------
log "[4/4] installing frontend deps and building the Tauri app (--features ocr)"
cd "$(dirname "$0")"

run "npm ci"

if [ "$CROSS" = "1" ]; then
  run "npm run tauri build -- --target aarch64-unknown-linux-gnu --features ocr"
  BUNDLE_DIR="src-tauri/target/aarch64-unknown-linux-gnu/release/bundle"
else
  run "npm run tauri build -- --features ocr"
  BUNDLE_DIR="src-tauri/target/release/bundle"
fi

ok "build complete! artifacts in: $BUNDLE_DIR"
if [ "$DRY_RUN" = "1" ]; then
  log "(DRY_RUN mode: the above are the commands that would run; nothing was built)"
else
  ls -lh "$BUNDLE_DIR" 2>/dev/null || true
fi
echo
echo "Local self-computation: FundLens depends on no third-party valuation API."
echo "All estimates are computed locally from disclosed holdings + live quotes."
