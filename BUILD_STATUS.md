# FundLens 构建状态（2026-08-13）

## 结论：端到端编译 + 链接 + 打包 全部通过 ✅

### 验证结果

| 步骤 | 命令 | 结果 |
|---|---|---|
| 前端类型检查 | `npx tsc --noEmit` | ✅ 0 错误 |
| 前端生产构建 | `npm run build`（tsc -b + vite build） | ✅ dist 生成，gzip JS ~64KB |
| Rust 类型检查 | `cargo check` | ✅ 通过 |
| Rust 调试链接 | `cargo build`（profile.dev codegen-units=1） | ✅ 链接通过 |
| Rust 发布链接 | `cargo build --release` | ✅ 链接通过 |
| **完整 Tauri 打包** | `npm run tauri build` | ✅ 产出 `.app` + `.dmg` |

### 交付物（本机 Intel x86_64）

```
fundlens/src-tauri/target/release/bundle/
├── macos/FundLens.app        # 5.5MB，可直接打开运行
└── dmg/FundLens_0.1.0_x64.dmg # 2.8MB 安装包
```

> 本开发机为 Intel Mac，故打出 x64 包。Apple Silicon 版需在 ARM Mac 上 `npm run tauri build`；ARM64 Linux 用 `bash build-linux.sh`。

### 修复的关键问题（Tauri 2.x 实战）

1. `tauri.conf.json` 误用 `architectures` 字段（当前 `tauri-build` 不识别）→ 删除，依赖宿主架构默认构建。
2. Cargo.toml 误启 `tauri` 的 `protocol-asset` 特性（与 config allowlist 冲突）→ 移除。
3. **Tauri 2 主应用命令不会自动生成 `allow-<cmd>` 权限** → 在 `src-tauri/permissions/fundlens.toml` 显式定义 11 条 `[[permission]]`（用 `fl-` 前缀避让冲突），capability `default.json` 引用 `core:default` + 这些 `fl-*` 权限。
4. `lib.rs` 内部引用 `crate::` 而非外部 crate 名 `fundlens_lib::`。
5. `FundMetaOut` 补 `#[derive(Clone)]`（`PositionRowOut` 含它且 derive Clone）。
6. `data.rs` 多余 `Datelike` import、腾讯行情解析 `trim_start_matches('v_')`→`"v_"`、`stock_code` 在 `map.insert` 作为 key 后 `.clone()` 再用作字段。
7. macOS 链接命令行超长（debug 默认 codegen-units=256）→ release 已设 `codegen-units=1`；额外给 `[profile.dev]` 也设 `codegen-units=1`，使 `tauri dev` 亦可链接。

### 仍未完成 / 待真机验证（不在编译范围）

- 自由数据源联网字段实测：腾讯 `qt.gtimg.cn`、东财 F10 `jjcc`、天天 `fundgz`（dwjz 基线）。
- PaddleOCR 模型权重（v1.1）接入，替换 `ocr.rs` 的规则解析骨架。
- 债/货/QDII 基金类型标记与"模型不适用"提示（PRD v0.3 已纳入，代码骨架待补）。
- 交易时段（9:30–15:00）每 15–30s 自动刷新 `refresh_valuation` 的调度触发。

### 本地运行

```bash
cd fundlens
npm run tauri dev      # 开发模式（需 Xcode CLT + Rust）
# 或打开已构建的
open src-tauri/target/release/bundle/macos/FundLens.app
```
