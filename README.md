# FundLens

本地优先的基金持仓实时估值与收益统计桌面工具。

FundLens 是一款基于 [Tauri 2](https://v2.tauri.app/) 的跨平台桌面应用：前端用 React + TypeScript，后端用 Rust，OCR 识别在本地离线完成，所有持仓与交易数据均保存在本机，**不上传任何隐私**。

---

## 功能特性

- **交易记录导入**：支持支付宝交易截图 OCR 识别，自动识别日期 / 时间 / 平台 / 买卖方向 / 金额，并携带「平台」字段入库。
- **基金明细**：单只基金详情页展示持仓与对应交易记录。
- **一键批量抓取披露持仓**：批量抓取所有持仓基金的公开披露持仓，免去逐只手动录入。
- **持仓总览**：组合层面的持仓汇总、实时估值与收益统计（已做性能优化，千级持仓规模依然流畅）。
- **本地 OCR**：内置 MNN 推理模型（det / rec / cls），离线识别，零云端依赖。

---

## 技术栈

| 层 | 技术 |
| --- | --- |
| 前端 | React 18 · TypeScript · Vite 5 · TailwindCSS · recharts · lucide-react · react-router-dom |
| 桌面 / 后端 | Tauri 2 (Rust)，WebView 渲染前端 |
| OCR | MNN 模型（本地推理，模型文件随安装包分发） |

---

## 环境要求

- **Node.js** 18 / 20 / 22（任意受支持版本）
- **Rust** stable 工具链
- Tauri 2 官方列出的系统前置依赖（见下方各平台说明）

---

## 开发

```bash
npm install

npm run dev        # 仅前端热更新 (http://localhost:1420)
npm run tauri dev  # 启动完整桌面应用（含 Rust 后端）
```

---

## 构建桌面安装包

### macOS（本机原生）

```bash
npm install
npm run tauri build
```

产物位于 `src-tauri/target/release/bundle/`（`macOS minimumSystemVersion = 10.15`）。

### Linux（Ubuntu 22.04 及以上，x86_64 / arm64 原生）

先安装 WebView 与系统依赖：

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf \
  libappindicator3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  build-essential curl wget file

npm install
npm run tauri build -- --bundles deb appimage
```

产物为 `.deb` 与 `.AppImage`（OCR 资源已随包打入 `resources/ocr`）。

> 仓库根目录的 [`build-linux.sh`](./build-linux.sh) 是一份可直接复用的 Ubuntu 22.04 原生构建脚本。

### arm64 Linux（在无 arm64 物理机的 macOS / x86 主机上交叉构建）

本项目提供基于 **Docker + QEMU** 的 arm64 构建方案，位于 [`arm64-build/`](./arm64-build/)：

```bash
cd arm64-build
bash build.sh          # 在 arm64v8/ubuntu:22.04 容器内原生编译 aarch64，产物输出到 dist-arm64/
```

该方案在本机（macOS）通过 QEMU 仿真以 aarch64 为原生目标编译，**无需交叉链接器**，产出标准 arm64 的 `.deb` / `.AppImage`。

---

## ⚠️ 麒麟银河 V10 SP1 (aarch64) 兼容性说明

Tauri 2 全系依赖 **`webkit2gtk-4.1`**（要求 **glibc ≥ 2.34**）与 **`libsoup-3.0`**。

而 **麒麟银河 V10 SP1（含 2403）默认仅提供 `webkit2gtk-4.0` + glibc 2.31**，缺少 4.1 与 libsoup-3.0。

**结论：用 Tauri 2 构建的桌面包无法在麒麟 V10 SP1 上原生运行。**

可行的变通方案：

1. 目标机先安装 `libwebkit2gtk-4.1`（麒麟开发者社区提供的 deb），并升级到 **SP2（glibc ≥ 2.34）**，之后再运行标准 arm64 包；
2. 在目标机内以 **Ubuntu 22.04 容器 / 虚拟机**方式运行；
3. 主流 arm64 Linux（**Ubuntu 22.04+ / Debian 12+ / 树莓派 OS 64 位新版**等）可直接运行标准包。

> 本项目不采用「降级 Tauri 以支持 webkit2gtk-4.0」的路线——Tauri 2 所有版本（2.0–2.11）均无该 feature，技术上不可行。

---

## 项目结构

```
fundlens/
├── src/                 # React 前端源码
├── src-tauri/           # Tauri (Rust) 后端
│   ├── src/             # Rust 命令与业务逻辑
│   ├── resources/ocr/   # MNN OCR 模型 (det / rec / cls)
│   └── tauri.conf.json  # 应用与打包配置
├── build-linux.sh       # Linux 原生构建脚本 (Ubuntu 22.04)
└── arm64-build/         # Docker + QEMU arm64 构建方案
```

---

## 隐私

- 所有数据（交易记录、持仓）存储于本地 SQLite：`src-tauri/fundlens.db`。
- OCR 推理完全在本地完成，不上传任何隐私数据。
- 应用仅向行情数据源（`push2.eastmoney.com` / `hq.sinajs.cn` / `qt.gtimg.cn`）发起只读行情请求。

---

## License

私有项目，保留所有权利。
