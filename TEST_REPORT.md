# FundLens 测试报告 · 前端代码分割 + 前后端单测

> 生成时间：2026-08-16
> 范围：前端路由级代码分割、前端单元测试（Vitest）、后端命令层单测（cargo）、生产构建校验

---

## 1. 本次变更总览

| 类别 | 改动 | 状态 |
| --- | --- | --- |
| 前端代码分割 | `App.tsx` 全部页面改为 `React.lazy` + `Suspense`；`vite.config.ts` 增加 `manualChunks` 拆分 vendor | ✅ 构建通过 |
| 前端单测基建 | 安装 `vitest` / `@testing-library/react` / `jest-dom` / `jsdom`；新增 `test` 配置与 `src/test-setup.ts`；`tsconfig.json` 排除测试文件以免污染生产构建 | ✅ |
| 前端单测 | 4 个测试文件，18 个用例，全部通过 | ✅ 18/18 |
| 后端单测 | 暴露 `db::tests` 助手为 `pub(crate)`；`commands.rs` 新增 `#[cfg(test)]` 模块，覆盖此前零覆盖的 4 个命令 | ✅ 4 个新增用例 |
| 生产构建 | `npm run build`（tsc + vite）通过，产物按页面/ vendor 拆分 | ✅ |

---

## 2. 前端代码分割

### 2.1 路由级懒加载
`src/App.tsx` 原先在文件顶部**同步** `import` 全部 7 个页面，意味着首屏必须解析整包。现改为：

```tsx
const OverviewPage = lazy(() => import('./pages/OverviewPage'));
const FundDetailPage = lazy(() => import('./pages/FundDetailPage'));
// …其余页面同理
<main>
  <Suspense fallback={<PageLoading />}>
    <Routes>…</Routes>
  </Suspense>
</main>
```

每个页面成为独立 chunk，进入对应路由时才按需拉取；并新增 `<PageLoading />` 骨架占位，避免懒加载间隙白屏。

### 2.2 Vendor 分包（长期缓存）
`vite.config.ts` 的 `rollupOptions.output.manualChunks` 将大体积、低变更频率的三方库拆成独立包：

| 分包 | 内容 | 体积（gzip） |
| --- | --- | --- |
| `react-vendor` | react / react-dom / react-router-dom | 53.6 KB |
| `charts` | recharts | 114.9 KB |
| `icons` | lucide-react | 3.6 KB |

### 2.3 首屏收益（关键结论）
经 `grep` 确认，**recharts 仅被 `StatsPage` 与 `FundDetailPage` 引用，默认路由 `OverviewPage` 不依赖它**。因此：

- **拆分前**：所有页面 + recharts 同步打包，首屏必须下载 recharts（114.9 KB gzip）等全部资源。
- **拆分后**：首屏仅下载入口 `index`（5.4 KB gzip）+ `react-vendor`（53.6 KB gzip）+ `OverviewPage` 自身 chunk（3.2 KB gzip）≈ **62 KB gzip**；recharts 直到用户打开「收益统计 / 基金详情」才加载，且作为独立 vendor 包可被浏览器长期缓存复用。

---

## 3. 前端单元测试（Vitest）

命令：`npm test`（即 `vitest run`）。环境：jsdom + `@testing-library/react`。

| 测试文件 | 用例数 | 覆盖点 |
| --- | --- | --- |
| `src/valuation/engine.test.ts` | 6 | `valueFund`：无持仓/净值非正不估算、披露加权、基准近似未披露部分、缺行情贡献为 0；`summarizePortfolio`：市值/成本/盈亏/当日估算收益聚合 |
| `src/components/GainLossBadge.test.tsx` | 4 | A 股红涨绿跌语义（正→`--color-gain`、负→`--color-loss`）、平值无箭头、金额格式 `¥` 与正负号 |
| `src/components/ui.test.tsx` | 6 | `StatTile` / `TrendChip` 涨跌色 / `ConfidenceBadge` 高置信样式 / `PlatformBadge` 平台名与未知 code 回退 |
| `src/lib/mockData.test.ts` | 2 | `PLATFORMS` 三平台结构自洽；`MOCK_FUNDS` 披露权重和 ≤ 1 |

**结果：4 个文件 / 18 个用例，全部通过（18 passed）。**

---

## 4. 后端单元测试（cargo）

命令：`cargo test --lib`（`src-tauri`）。

新增 `commands.rs` 测试模块，复用 `db::tests` 的「唯一临时库 + 全局连接串行锁」基础设施，覆盖此前**零测试**的命令层：

| 新增用例 | 覆盖点 |
| --- | --- |
| `write_text_file_creates_parent_dirs_and_content` | `write_text_file` 自动建父目录并写入内容（报告导出场景） |
| `export_import_db_roundtrip_preserves_positions` | `export_db` → 破坏数据 → `import_db` 恢复，持仓不丢失（SPEC §F5 备份恢复核心链路） |
| `update_position_resolves_existing_platform_without_phantom` | `update_position` 省略平台时回退既有平台，不产生空平台幻影行 |
| `update_position_defaults_empty_platform_for_new_fund` | 全新基金无既有平台时正确落到空平台（默认分支） |

**结果：62 个用例全部通过（62 passed，较此前 58 新增 4 个），0 失败。**

> 说明：`get_fund_detail` 的 `valuation_source` 判定逻辑依赖实时行情/基准网络拉取，属于网络依赖型命令，未在离线单测中覆盖——后续可通过为 `data` 模块引入 mock 适配层补齐（建议作为后续项）。

---

## 5. 生产构建校验

`npm run build`（`tsc -b && vite build`）通过。关键产物（节选）：

```
dist/index.html                      0.57 kB
dist/assets/index-*.js             13.81 kB (gzip 5.40)   ← 入口
dist/assets/react-vendor-*.js     164.01 kB (gzip 53.57)  ← 框架
dist/assets/charts-*.js           433.43 kB (gzip 114.92)  ← recharts（按需）
dist/assets/icons-*.js             16.84 kB (gzip  3.57)
dist/assets/OverviewPage-*.js       7.91 kB (gzip  3.22)   ← 首屏页
dist/assets/FundDetailPage-*.js    20.93 kB (gzip  6.77)
dist/assets/LedgerPage-*.js        22.35 kB (gzip  6.95)
dist/assets/ReportsPage-*.js       11.77 kB (gzip  4.58)
dist/assets/StatsPage-*.js          5.14 kB (gzip  2.27)
…（ImportPage / AboutPage / ui / GainLossBadge 等各自独立 chunk）
```

`tsc -b` 因 `tsconfig.json` 已 `exclude` 测试文件，生产类型检查不受测试代码影响。

---

## 6. 测试覆盖与后续建议

**已覆盖（本次新增/强化）**
- 估值引擎纯算法（前后端同源，前端已单测，Rust 端 `valuation.rs` 原有覆盖）。
- 命令层核心副作用：备份导出/恢复、改仓平台透传、文本写出。
- 关键 UI 语义：红涨绿跌双重编码、平台徽标、置信度徽标。

**建议后续补齐**
1. `get_fund_detail` 的 `valuation_source`（realtime/local/none）需为 `data` 网络层引入 mock 适配，做确定性单测。
2. 前端可对 `ReportsPage` 的 `buildReportMarkdown`、路由跳转做集成测试。
3. 接入 `cargo-tarpaulin` / `vitest --coverage` 输出量化覆盖率，纳入 CI 门禁。
