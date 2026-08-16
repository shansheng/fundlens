# FundLens 专家团交付盘点（MvpDevExpertTeam 状态审查）

> 审查时间：2026-08-16 13:21（GMT+8）
> 审查视角：Project Director
> 当前在跑任务：arm64 Linux 安装包构建（后台 `BdL7Ww`，QEMU 仿真编译中）

图例：✅ 已交付　🔄 进行中　⚠️ 有问题　💡 可优化

---

## 1. DevOps（运维/构建）—— 🔄 进行中 + 💡 可优化
- ✅ 已有 `build-linux.sh`（Ubuntu 22.04 标准 x64 路径）。
- 🔄 **arm64 包构建进行中**：本机 Docker QEMU 跑 `arm64v8/ubuntu:22.04`（webkit2gtk-4.1 + Rust + Node 22），源码经「本机解包 + `docker cp`」入容器（已验证 58 文件、0 报错），`vite build` 已完成（1m21s），当前 cargo 正在解析/下载依赖。**预计 1–3 小时**。
- ⚠️ 传输环节曾两次踩坑：① 容器内 GNU tar 解 PAX 头触发 `EINVAL`；② 仿真容器内 tar 直接僵死。已改用「本机原生解包 + `docker cp`」绕过（关键经验，建议沉淀为技能）。
- 💡 优化：把 cargo registry / node_modules 缓存进 Docker volume，可大幅加速二次构建；QEMU 仿真编译偏慢，长期可上原生 arm64 runner 或交叉编译。

## 2. Architect（架构师）—— ⚠️ 有结论待拍板
- ⚠️ **麒麟银河 V10 SP1 原生包：技术上不可行**。Tauri 2 全系（2.0–2.11）只认 `webkit2gtk-4.1` + `glibc ≥ 2.34`，无 4.0 支持（降级路径不存在）；而麒麟 V10 SP1 只有 `webkit2gtk-4.0` + `glibc 2.31`。
- ✅ 可行交付：标准 arm64 包（Ubuntu 22.04 基线），可在主流 arm64 Linux（Ubuntu 22.04+/Debian 12+/树莓派 OS 64 位）直接运行。
- 💡 麒麟落地路径待 PM 拍板：① 目标机先装 `libwebkit2gtk-4.1`（麒麟社区 deb）+ 升 SP2；或 ② 目标机用 Ubuntu 22.04 容器/VM 跑。

## 3. PM（产品经理）—— ✅ 需求已交付 + ⚠️ 待收口
- ✅ 本轮/前序需求均交付：交易导入携带平台修复、基金明细页交易记录展示、一键批量抓取披露持仓、持仓总览性能优化、UTF-8 panic 崩溃修复。
- ⚠️ 待收口：「支持麒麟 V10 SP1 原生包」这一原始需求需正式闭环——重定向为「标准 arm64 包 + 麒麟兼容指引」，并与用户确认目标发行版支持矩阵。

## 4. Backend（后端/Rust）—— ⚠️ 健壮性问题
- ✅ OCR 模块、db、commands、valuation 主体完备；db.rs 含内联单元测试。
- ⚠️ **网络解析缺少容错**：`data.rs` 多处对上游返回直接 `.unwrap()`（`parse_fund_estimate` L1012/1023、`parse_nav_history` L1191/1203、`from_ymd_opt` L1171/1222），上游格式一变即 `panic` 崩溃；`commands.rs` L1476/1494 对 `snaps` 取 `.last()/.unwrap()` 空数据时同理。
- 💡 优化：改 `?` + `Result`，对解析失败做降级/重试/用户提示，而非进程崩溃。`from_ymd_opt` 应改用当前日期而非硬编码（L1171/L1222 写死 2026-08-13 / 2026-02-01）。

## 5. Frontend（前端）—— ✅ 已交付 + 💡 可优化
- ✅ 功能落地：`FundDetailPage.tsx`、`LedgerPage.tsx`、`OverviewPage.tsx`、`api.ts` 均含交易/披露相关实现痕迹；批量抓取按钮、明细交易展示、性能优化已在位。
- ⚠️ **零前端测试**：`src` 下无任何 `.test./.spec.` 文件。
- 💡 优化：`vite build` 告警单个 chunk 707 KB（>500 KB）——建议按路由 `dynamic import()` 做代码分割，降低首屏体积与白屏。

## 6. QA（测试）—— ⚠️ 覆盖严重不足
- ⚠️ 仅 `src-tauri/tests/ocr_e2e.rs` 一个端到端用例；`data.rs`/`commands.rs`/`valuation.rs` 无单测（仅 db.rs 有内联测试）。
- ⚠️ **无 CI**：仓库无 `.github/`，无自动化构建/测试流水线；本次 arm64 产物尚未验证「能否真正启动」（本机无 arm64 环境，需进容器 `exec` AppImage 验证）。
- 💡 优化：补 `data.rs` 解析单测 + `commands.rs` 边界单测；加 GitHub Actions 跑 `cargo test` + 多平台构建。

## 7. Designer（设计）—— ✅ 无明显问题
- UI/UX 本次无报错项；💡 可配合前端的代码分割，顺带评估首屏较重页面的加载体验。

---

## 优先级建议（Director 拍板）
1. **P0（当前）**：让 arm64 构建跑完 → 验证 `.deb`/`.AppImage` 产物并在容器内实际启动一次（QA）。
2. **P1**：PM 收口麒麟需求 → Architect 输出「标准 arm64 包 + 麒麟兼容指引」文档。
3. **P1**：Backend 修 `data.rs`/`commands.rs` 的 `.unwrap()` 容错（健壮性硬伤）。
4. **P2**：补前端/后端单测 + 引入 CI；前端代码分割。
5. **P2（治理）**：**fundlens 当前不是 git 仓库**——任何改动都不受版本控制，风险高，建议立即 `git init` 并提交基线。

> 备注：fundlens 目录 `git status` 报 "not a git repository"，所有改动均为本地未跟踪状态，这是最该先补的基础设施缺口。
