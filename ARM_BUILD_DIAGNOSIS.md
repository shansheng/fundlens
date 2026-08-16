# FundLens ARM Linux 构建失败诊断

> 现象：ARM Linux 上 `npm run tauri build`（或 `cargo build --target aarch64-*`）在编译
> `rusto-mnn-sys` 时失败，报错 `Unable to find libclang ... bindgen`，随后 `cmake --build ... --target MNN` 失败。
> 本机（macOS）此前构建一直正常。

## 根因（已确证）

### 1. libclang 缺失（必现，与是否预编译无关）
`rusto-mnn-sys`（OCR 引擎 PaddleOCR / MNN，由 `rusto-rs` 引入）的 `build.rs` **无条件**调用
`bindgen` 为 `mnn_wrapper.h` 生成 FFI 绑定：

```rust
let bindings = bindgen::Builder::default()
    .header("mnn_wrapper.h")
    .generate()
    .expect("Unable to generate bindings");
```

`bindgen` 构建期必须能找到 `libclang.so`。
- macOS：Xcode Command Line Tools 自带 clang → 此前一直能过。
- 裸 Linux CI / 容器（Debian/Ubuntu/Alpine 基础镜像）：默认**没有** libclang → 报 `Unable to find libclang`。

### 2. 走了 MNN 源码编译路径（日志里出现 `cmake --target MNN`）
该 crate 仅对以下目标提供**预编译** MNN 包：

```rust
let supported = matches!(target.as_str(),
    "x86_64-unknown-linux-gnu"
  | "aarch64-unknown-linux-gnu"
  | "x86_64-apple-darwin"
  | "aarch64-apple-darwin"
  | "x86_64-pc-windows-msvc");
```

若 ARM 目标**不在**清单（常见如 `aarch64-unknown-linux-musl`、`armv7-unknown-linux-gnueabihf`），
或 GitHub 预编译包下载失败，就会回退到 `build_from_source`：克隆 MNN 源码 → `cmake` 编译。
此路径额外需要 `git` + `cmake` + C++ 工具链（`g++`/`clang++`）。

> 结论：即使走预编译包，**libclang 仍然必需**（bindgen 无条件运行）；而源码编译还需 cmake/git/g++。

### 3. AppImage 打包：`linuxdeploy` 的 appimage plugin 是 static-pie AppImage，QEMU 下无法执行（必现）
Rust 应用 + `.deb` 都编完后，最后一步 AppImage 打包调用 `linuxdeploy-aarch64.AppImage`，
它运行时再拉起 **`linuxdeploy-plugin-appimage`**（它本身也是 AppImage）。
- 主体 `linuxdeploy-aarch64.AppImage` 是 **dynamically linked** 的 aarch64 ELF，设
  `APPIMAGE_EXTRACT_AND_RUN=1` 后能在 QEMU 下正常跑（实测 `--version` 输出正常）。
- 但其 appimage plugin AppImage 是 **`static-pie linked`** 的 aarch64 ELF；在 QEMU 用户态仿真下
  直接 exec 会报 `cannot execute binary file: Exec format error` → plugin 子进程退出码 126/2 →
  linuxdeploy 报 `subprocess failed (exit code 2)` → Tauri 报 `failed to run linuxdeploy`
  （stderr 为空，极难直接看出根因）。
- **这是 QEMU 用户态对 static-pie 运行时 AppImage 的兼容限制，不是代码/包缺陷**，在真实 arm64
  硬件（银河 V10 arm64、任意 aarch64 Linux 桌面）上 plugin 能正常执行。

> 注：容器内确实也**没有 `/dev/fuse`**，但 FUSE 只影响"挂载式运行"，`APPIMAGE_EXTRACT_AND_RUN=1`
> 已让主体 linuxdeploy 解压运行绕开 FUSE；真正过不去的是 plugin 的 static-pie 运行时，与 FUSE 无关。

### 4. AppImage 图标名大小写不一致（手工打包时的二阶坑）
Tauri 生成的 `FundLens.AppDir` 中，`.desktop` 引用 `Icon=fundlens`（小写），但 AppDir 根目录
只有 `FundLens.png`（大写），`appimagetool` 据此判定图标缺失并中止（`fundlens{.png,.svg,.xpm}
defined in desktop file but not found`）。`hicolor/` 各级虽有小写 `fundlens.png`，但根级检查未命中。
**修复**：在 AppDir 根目录补一个小写 `fundlens.png`（复制 `FundLens.png` 即可）。

## 修复方案

### 方案一：保留 OCR（在 ARM 构建机装齐依赖）
Debian / Ubuntu：
```bash
apt-get update && apt-get install -y clang libclang-dev cmake git build-essential
```
Alpine：
```bash
apk add clang llvm-dev cmake git build-base
```
若装完 cargo 仍报找不到 libclang，显式指定路径：
```bash
export LIBCLANG_PATH=$(dirname $(find /usr -name 'libclang.so*' | head -1))
```
（典型值如 `/usr/lib/llvm-14/lib`）

**AppImage 打包（linuxdeploy appimage plugin 无法在 QEMU 下执行）**：Tauri 的 `tauri build
--bundles appimage` 在 QEMU 下必败于 plugin 的 static-pie 运行时。绕开方式：让 Tauri 先生成
`FundLens.AppDir`（这一步会创建 AppDir 后失败，可接受），再从 Tauri 已缓存下载的
`linuxdeploy-plugin-appimage.AppImage` 里 `unsquashfs` 取出其中的 `appimagetool`
（它是 shell 脚本，QEMU 可正常执行），直接对 AppDir 打包，并修掉图标名大小写。
本项目已封装为 `arm64-build/make-appimage.sh`，并在 `build.sh` 第 4b 步自动调用；
`Dockerfile` 已加 `squashfs-tools`（提供 `unsquashfs`）。`APPIMAGE_EXTRACT_AND_RUN=1` 仍需保留
（用于让主体 linuxdeploy 及 appimagetool 解压运行）。

```bash
# 手工补完（容器内）：
bash make-appimage.sh <AppDir> <output.AppImage>
```

> 在真实 arm64 硬件上，`tauri build --bundles appimage` 本应能直接成功；此绕行仅针对 QEMU 仿真构建机。

### 方案二：ARM 构建不需要 OCR → 直接关掉 `ocr` 特性（最省事）
`Cargo.toml` 中 `default = ["ocr"]`，`--no-default-features` 只会去掉本 crate 的 `ocr`，
**不影响 `tauri` 自身默认特性**。
```bash
cargo tauri build --no-default-features
# 或纯 cargo：
cargo build --release --no-default-features --target aarch64-unknown-linux-gnu
```
关闭后 `rusto-rs`/`MNN`/`bindgen` 全部不引入，不再需要 libclang / cmake。

## 建议
- 若 ARM 包面向无截图导入需求的场景（如服务器/嵌入式），优先用**方案二**，构建最干净、最快。
- 若必须保留 OCR，把方案一依赖固化进 Dockerfile / CI 镜像，并把 `LIBCLANG_PATH` 写入构建环境变量，
  避免下次裸镜像再次踩坑。
- **AppImage 打包**：`arm64-build/build.sh` 已改为 `tauri build --bundles deb appimage || true`
  （容忍 QEMU 下 plugin 失败），随后第 4b 步调用 `make-appimage.sh` 手工补完 AppImage；
  `Dockerfile` 已加 `squashfs-tools`（提供 `unsquashfs`）与 `clang`/`libclang-dev`，并固化
  `LIBCLANG_PATH` 与 `APPIMAGE_EXTRACT_AND_RUN=1` 两个环境变量。以后从零 `docker build` + `build.sh`
  不会再连卡这三道（libclang / MNN 联网 / linuxdeploy-plugin 的 QEMU 兼容性）。
- 该第三方 crate 的 bindgen 调用无法绕开，无法仅靠改项目代码解决；要么装 libclang，要么去掉 `ocr` 特性。
