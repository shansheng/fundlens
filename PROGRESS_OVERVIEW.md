# FundLens 进展总览（续）

## 本次继续完成的内容
延续此前已锁定的方向（Tauri 桌面应用 / 本地 OCR+规则 / 免费公开 API / **本地自算估值** / 命名 FundLens / 先 macOS 后 Linux 脚本），完成了 **完整前端骨架 + Rust/Tauri 后端脚手架 + 图标 + Linux 构建脚本**。

## 1. 前端（已编译通过 ✅）
- `src/App.tsx`：HashRouter 路由 + 左侧导航外壳（持仓总览 / 截图导入 / 收益统计），Lucide 图标，遵循 P0 设计令牌。
- 4 个页面：
  - `OverviewPage`：组合汇总 + 持仓明细表，交易时段每 15 秒自动刷新，非交易时段给出提示。
  - `ImportPage`：三平台（支付宝 / 京东金融 / 腾讯理财通）模板选择 + 截图上传 + 识别结果预览。
  - `FundDetailPage`：单基金估值拆解（披露持仓对净值贡献 + 当日个股行情）。
  - `StatsPage`：累计收益、最佳/最差、分平台分布、估算覆盖率。
- `src/api.ts`：Tauri `invoke` 桥接；**浏览器环境下回退到本地 mock**，保证 UI 无需后端即可预览。
- `src/lib/mockData.ts`：4 支演示基金（含披露持仓 + 实时行情），覆盖医疗/白酒/食品饮料等场景。
- 核心组件 `GainLossBadge`（P0：红涨绿跌 + `+/-`号 + 箭头图标双重编码，绝不单靠颜色表意）。

## 2. 后端脚手架（Tauri 2.x，无 cargo 未能编译实测 ⚠️）
- `src-tauri/`：`Cargo.toml`（Tauri 2 + rusqlite bundled + reqwest rustls-tls）、`tauri.conf.json`（macOS aarch64 + Linux arm64 打包）、`build.rs`、`main.rs`、`lib.rs`。
- 模块：
  - `valuation.rs`：`engine.ts` 的 Rust 镜像（`est_nav = official_nav × (1 + Σ 占比ᵢ × (现价ᵢ/昨收ᵢ − 1))`）。
  - `data.rs`：腾讯 `qt.gtimg.cn` 实时行情 + 东方财富 F10 披露持仓拉取 + A 股交易时段判定。
  - `db.rs`：rusqlite 本地 SQLite，含 11 张表建表语句与核心读写。
  - `commands.rs`：SPEC 约定的 11 条 Tauri 命令全实现。
  - `ocr.rs`：PaddleOCR sidecar 接入设计 + 三平台规则解析骨架（v1.1 接模型权重）。

## 3. 图标与构建
- 用 Python 原生 PNG 编码器生成 `icons/`（32/128/256/128@2x PNG + ICO/ICNS），满足 Tauri 打包要求。
- `build-linux.sh`：ARM64(aarch64) 一键安装 Rust + WebKit2GTK + 依赖并 `tauri build`。

## 验证结果
| 项 | 结果 |
|---|---|
| `npx tsc --noEmit` | ✅ 0 错误 |
| `npx vite build` | ✅ 成功（dist 生成，gzip JS ~64KB） |
| Rust 后端编译 | ⚠️ 本机无 cargo/rustc，未实测（代码已按 Tauri 2.x 最佳实践编写并人工审查） |

## 已知限制 & 下一步
1. **本机无 Rust 工具链**：macOS 交付需用户本地安装 Xcode CLT + Rust 后执行 `npm run tauri build`；Linux 用 `bash build-linux.sh`。
2. **自由数据源需联网实测**：腾讯/东财接口字段与稳定性需在真机验证（见 memory 待办）。
3. **OCR 为占位**：v1.1 接入 PaddleOCR PP-OCRv5 mobile 权重，替换现有规则解析骨架。
4. 后端目前为 best-effort 代码，首次实机编译可能需微调依赖版本与 trait 引入。
