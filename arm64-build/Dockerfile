# FundLens arm64 (aarch64) Linux builder — 麒麟银河 V10 SP1 (2403) 适配
# Base: Ubuntu 20.04 on arm64 (glibc 2.31 + libwebkit2gtk-4.0-dev)
# Target: 银河麒麟 V10 SP1 2403 (aarch64)，基于 Ubuntu 20.04：
#   - glibc 2.31（因此构建容器也必须 glibc <= 2.31，否则二进制运行时报
#     "version `GLIBC_2.34' not found" —— 之前 Ubuntu 22.04 打的包在麒麟上起不来就是这个原因）
#   - 系统只有 libwebkit2gtk-4.0-dev（WebKitGTK 4.0），Tauri 1.x 使用 webkit2gtk-4.0
#     API，与之原生匹配；Tauri 2.x 强制 webkit2gtk-4.1，在麒麟上无法运行。
# 本容器产出原生 aarch64 .deb + .AppImage。
FROM arm64v8/ubuntu:20.04

ENV DEBIAN_FRONTEND=noninteractive
ENV PATH="/root/.cargo/bin:${PATH}"
# bindgen（rusto-mnn-sys 生成 FFI 绑定）需要显式指向 libclang.so；
# Ubuntu 20.04 (focal) 的 libclang-dev 提供 llvm-10。
ENV LIBCLANG_PATH=/usr/lib/llvm-10/lib
# 容器内无 /dev/fuse，AppImage 无法挂载自身；改用解压运行，linuxdeploy 及其 plugin 一并生效。
ENV APPIMAGE_EXTRACT_AND_RUN=1

# ---- system build dependencies ----
# Tauri 1.x on Linux 需要：libwebkit2gtk-4.0-dev + libgtk-3-dev + libayatana-appindicator3-dev
# rusto-mnn-sys（OCR）需要：clang/libclang-dev（bindgen）+ cmake/git/g++（MNN 预编译包
# 下载失败时回退源码编译）；AppImage 手工补完需要 squashfs-tools。
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
        libwebkit2gtk-4.0-dev \
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

# ---- Node.js 20 (LTS, 兼容 Ubuntu 20.04 focal) ----
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
CMD ["sleep", "infinity"]
