# FundLens arm64 (aarch64) Linux builder
# Base: Ubuntu 22.04 on arm64 (glibc 2.35 + libwebkit2gtk-4.1-dev)
# Goal: produce native aarch64 .deb + .AppImage via the Tauri 2 toolchain.
# NOTE: Tauri 2 requires webkit2gtk-4.1 + glibc >= 2.34. Kylin 银河 V10 SP1
# (glibc 2.31 + webkit2gtk-4.0 only) cannot run these binaries natively.
FROM arm64v8/ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
ENV PATH="/root/.cargo/bin:${PATH}"
# bindgen（rusto-mnn-sys 生成 FFI 绑定）需要显式指向 libclang.so；Debian 装在 llvm-14 下。
ENV LIBCLANG_PATH=/usr/lib/llvm-14/lib
# 容器内无 /dev/fuse，AppImage 无法挂载自身；改用解压运行，linuxdeploy 及其 plugin 一并生效。
ENV APPIMAGE_EXTRACT_AND_RUN=1

# ---- system build dependencies ----
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        cmake \
        curl \
        wget \
        file \
        pkg-config \
        git \
        ca-certificates \
        libxdo-dev \
        libssl-dev \
        libwebkit2gtk-4.1-dev \
        libjavascriptcoregtk-4.1-dev \
        libgtk-3-dev \
        libayatana-appindicator3-dev \
        librsvg2-dev \
        patchelf \
        libfuse2 \
        squashfs-tools \
        dpkg-dev \
        clang \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*

# ---- Rust (native aarch64, no cross target needed) ----
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile minimal

# ---- Node.js 22 (LTS) ----
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
CMD ["sleep", "infinity"]
