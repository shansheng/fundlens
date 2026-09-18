# v2.6.17 发版记录 —— 业务审查修复 + 挂单状态机连带回归

> 日期：2026-09-19
> main：`f43dfed`（版本位）／本记录的父提交链 `a801740` → `8850d89` → `f43dfed`
> 麒麟：`00083ed`（Merge branch 'main'）
> 交付范围：**仅 macOS 出包落地**。麒麟按主人裁定**只同步分支、不出包**（改在麒麟机器拉 GitHub 打包）。

---

## 1. 交付内容

| # | 内容 | 来源 | commit |
|---|------|------|--------|
| 1 | 数据安全：`import_db` 恢复前自动备份 | 审查报告 P0 | `dba6389` |
| 2 | OCR 年份不再写死（`chrono::Local::now().year()`） | 审查报告 P0 | `dba6389` |
| 3 | 前端：时区收口 + 开关失效 + 异常处理 + 委托依赖 + 负零显示 | 审查报告 | `5be3fc3` |
| 4 | 锁中毒统一 `into_inner` 恢复 / 云同步网络 IO 移出全局锁闭包 | 审查报告 | `9348a3e` `6e85d25` |
| 5 | 卖出费率双口径统一（装配边界显式 ×100）+ 非零费率用例 | 审查报告 P0 | `fa19453` |
| 6 | 策略引擎：趋势同源去重 / 无持仓分支统一 `estimate_current_nav` / 挂单触发移到风控闸门后 + `notified` 中间态 | 审查报告 P0 | `29afcf7` |
| 7 | 报表 `est_act_diff` 三方口径对齐（Rust 注释 + mock + 文案） | 审查报告 P1 | `29afcf7` |
| 8 | **挂单 `notified` 连带回归**：引擎消费对象排除 `notified` | **本轮自查新增** | `a801740` |

> 第 8 项不是报告条目，是第 6 项引入 `notified` 中间态后的**连带缺陷**（详见第 4 节）。

---

## 2. 变更清单（文件级）

**后端（Rust）**：`commands.rs` `commands_grid.rs` `db.rs` `lib.rs` `ocr.rs` `data.rs` `cloud.rs` `strategy/engine.rs` `strategy/helpers.rs` `strategy/mod.rs`
**前端（TS/TSX）**：`api.ts` `pages/StrategyPage.tsx` `pages/ReportsPage.tsx` `pages/SyncPage.tsx` `pages/LedgerPage.tsx` `pages/OverviewPage.tsx` `pages/FundDetailPage.tsx` `pages/AboutPage.tsx` `components/*`
**新增测试**：`src/lib/date.test.ts` `src/lib/num.test.ts` `src/api.report.test.ts`
**新增工具**：`src/lib/date.ts` `src/lib/num.ts`
**配置**：`permissions/fundlens.toml`（新增 `fl-grid → grid_pending_confirm`）、`capabilities/default.json`
**版本位**：`package.json` `package-lock.json` `src-tauri/tauri.conf.json` `src-tauri/Cargo.toml` `src-tauri/Cargo.lock`

---

## 3. 质量验证

| 门禁 | 结果 |
|------|------|
| `npx tsc -b` | ✅ 通过 |
| `npx vitest run` | ✅ **105 passed**（16 files） |
| `cargo test --lib --no-default-features`（main） | ✅ **274 passed / 0 failed** |
| `cargo test --lib --no-default-features`（麒麟合并后） | ✅ **274 passed / 0 failed**（与 main 同数） |
| `cargo check --lib --no-default-features`（麒麟） | ✅ 通过（连带 `tauri-build` 配置校验） |
| 变异测试 | ✅ 本轮共 **9 处**变异逐一确认断言会红后恢复 |

**变异测试明细（本轮 2 处，均已恢复）**：

| 变异 | 期望 | 实测 |
|---|---|---|
| `grid_pending_next_to_trigger` 的 `status='pending'` → `IN ('pending','notified')` | 2 条红 | ✅ `left: Some(1) right: Some(2)`（饿死现场） |
| `sweep_expired_pending` 的 `IN ('pending','notified')` → `= 'pending'` | 1 条红 | ✅ `left: "notified" right: "expired"` |

---

## 4. 第 8 项详解：挂单 `notified` 连带回归

**根因**：引擎每轮只消费**最早一条**活跃挂单（`commands_grid.rs:701` 取 `.next()`；`engine.rs:68` 命中即 `return`），而第 6 项把触发态从 `triggered` 改成 `notified` 后，挂单**不再退出**活跃列表 → 它**永久霸占"最早"位置**：

1. 同一张单每轮重复返回 buy 信号 → 重复消耗 P3 每日 20% 买入预算；
2. 排在它后面的 `pending` 单**永远轮不到检查**（饿死），触发价到了也不提醒。

修复前不存在此问题（被消费的单会退出列表、"最早"自然让位）→ 属**引入中间态的连带缺陷**。

**修复**：把「展示/额度集合」与「引擎消费对象」拆成两个语义 —— 新增 `db::grid_pending_next_to_trigger`（只取最早 `pending`），`commands_grid.rs:701` 消费点切换；`grid_pending_list_active` 保留 `notified`（UI 提醒 + 占额度）并加互相指名注释。

**真实库只读核查**（`~/Library/Application Support/com.fundlens.app/fundlens.db`）：
- 挂单状态分布 `cancelled 4 / pending 11 / triggered 12`，**`notified` = 0 行** → 回归**未污染任何真实数据**（出包前被拦下）；
- `006503` 有 **6 条**活跃 pending（超软上限 3）→ 经查为历史数据（09-07/08 创建，早于软上限所在构建部署），**非旁路写入**；若当时部署的是含 `notified` 的构建，除最早一条外其余 5 条全部会被饿死 —— 这就是本次回归的具体影响面。

---

## 5. 出包与部署（macOS）

| 项 | 值 |
|---|---|
| 构建命令 | `zsh fl-build-desktop.sh` |
| 耗时 | **5m09s** |
| 产物 | `src-tauri/target/release/bundle/macos/FundLens.app`（17M） |
| 产物 sha256 | `544d87c1b4b6f20db9537f00ffaca5836eefd3165f29feb6bf23a78e64d23898` |
| 部署 | `pkill` → 旧版 `mv` 到 `/tmp/FundLens.app.prev-20260919-023737` → `/bin/cp -R` |

**部署核验五项**：

| # | 项 | 结果 |
|---|---|---|
| 1 | 版本号 | `2.6.17` ✅ |
| 2 | 部署 mtime ≥ 产物 | 02:37:38 ≥ 02:36:39 ✅ |
| 3 | 二进制 sha256 产物==部署 | 一致 ✅ |
| 4 | 启动冒烟 | 进程稳定存活 ✅ |
| 5 | **二进制指纹（决定性）** | ✅ 见下 |

**第 5 项做法**：用本次 `vite build` 产出的 asset **内容哈希名**做字节搜索 —— `StrategyPage-Dt7kzz6E.js` / `api-BXccjuJ7.js` / `ReportsPage-C-zdjXlE.js` 在新版二进制中命中 1 次、旧版 0 次，且与 `dist/assets/` 精确一致 → **证明部署的 app 就是当前源码构建的**（sha256 只能证明复制完整，不能证明来源）。另 `pre-restore`（第 1 项）、`挂单不存在或状态已变更`（第 6/8 项）新版 1 / 旧版 0。

---

## 6. 麒麟分支同步

`git merge --no-commit --no-ff main` → 三处冲突，四处固定动作逐条落实：

| # | 项 | 处理 |
|---|---|---|
| 1 | `permissions/fundlens.toml` | main 改 / 麒麟删 → **取删除**；`capabilities/` git 已保留删除，核验不存在 |
| 2 | `src-tauri/tauri.conf.json` | 保留 `$schema .../config/1`；删被并进的顶层 `productName`/`version`/`identifier`；**本次唯一改动 = `package.version` → 2.6.17**；`plugins` 顶键为麒麟原有 |
| 3 | `src-tauri/Cargo.lock` | `tauri = 1.8.3` 未被污染，实际改动仅 `fundlens` version 一行 |
| 4 | `package-lock.json` | `api ^1.6.0` / `cli 1.6.3`、`plugin-dialog` 0 次，改动仅 version 两处 → **本轮未重演污染** |

> ⚠️ 第 3/4 项**先 diff 判定再决定是否需要 `checkout HEAD^1`**：本轮自动合并是干净的，若无条件执行 `checkout HEAD^1` 反而会把刚升好的版本号退回去。

另：`Cargo.toml` 冲突取麒麟侧 `custom-protocol` feature；`src-tauri/gen/schemas/acl-manifests.json` 被 macOS 构建重跑改写 → **已 `git checkout --` 还原麒麟侧**（它是"在 main 上构建"的产物）。

**同步完整性核验**：`git log main ^feat/kylin-v10-aarch64` **为空** → 麒麟已含 main 全部提交。

---

## 7. 交付物状态

| 交付物 | 状态 |
|---|---|
| macOS `FundLens.app` v2.6.17 | ✅ 已构建、已部署 `/Applications`、五项核验通过 |
| Android APK | ⏳ 本版未出包（按要求） |
| 麒麟 deb + AppImage | ⏳ 本版未出包（按要求，改在麒麟机器拉 GitHub 打包） |
| main | ✅ `f43dfed` 已推送 origin |
| feat/kylin-v10-aarch64 | ✅ `00083ed` 已推送 origin |

---

## 8. 已知限制 / 未验证

- ⏳ GUI 未做界面级走查：本轮无前端改动，仅"进程存活"级冒烟。
- ⏳ `notified` 挂单的**跨天行为**未实测（次日重算时该单不再被引擎消费，仅留 UI 提醒至 28 天过期）。
- ⏳ 移动端未实机验证。
- ✅ 已按约定补交 `src-tauri/gen/schemas/acl-manifests.json`：桌面构建重跑会重写它（它是 `permissions/*.toml` 的**生成物**），本次把它刷新为含 `fl-grid.grid_pending_confirm` 的版本，使 manifest 与权限文件一致，并避免下一轮构建再留脏树。该文件在麒麟分支同样被跟踪且两侧内容完全相同，故随同一次 merge 一并同步（保持两分支一致、避免后续冲突）。
- ℹ️ 本机 Docker Desktop 4.71.0 无法启动（`backend crashed … opening tray: starting electron: … broken pipe`），故麒麟包未在本机尝试。

---

## 9. 回滚方案

**应用**：
```bash
pkill -f "FundLens.app/Contents/MacOS/fundlens"
mv /Applications/FundLens.app /tmp/FundLens.app.bad-$(date +%Y%m%d-%H%M%S)
mv /tmp/FundLens.app.prev-20260919-023737 /Applications/FundLens.app     # 回到 v2.6.16
```

**代码**：`git revert f43dfed a801740 8850d89 6437b37 29afcf7 8f5bf45 fa19453 dba6389 5be3fc3`（按需）；麒麟分支同理后重跑四处固定动作。

**数据**：本次无 schema 迁移（`grid_pending_rebuy` 表结构未变，`notified` 是既有 `status` 列的新取值）。费率口径修复是**零迁移**的（只改装配边界换算，自动修正全部存量）。

---

## 10. 下一步

1. 麒麟机器 `git pull` 后自行打包：进入 `feat/kylin-v10-aarch64`，`bash arm64-build/inc-build.sh`（复用 `fl-build` 容器增量 ≈1h；容器不存在则先 `build.sh` 全量重建）。
2. 审查报告 §2 的 13 条 P1、§8「先复现再改」两项（风格箱走成分路径 / `sync_log` 保留期清理）尚未施工。
3. Android APK 本版未出包（按要求）；如需，`zsh fl-build-android.sh`（约 3~8 min）。
