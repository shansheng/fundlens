# FundLens 代码审查报告修复记录（7 项）

> 日期：2026-09-19
> 基准：main @ `2504c68`（v2.6.16，工作区干净）
> 依据：`FundLens-审查报告评估-2026-09-18.md` 的「直接照做」清单
> 范围：只做评估报告建议中**证据充分**的 7 项，未做项见第 4 节

---

## 1. 已修 7 项

| # | 修复项 | 关键文件 | 性质 |
|---|--------|----------|------|
| 1 | 云同步网络 IO 移出全局锁闭包 | `cloud.rs` / `commands.rs` | 架构级：持锁做网络 IO |
| 2 | 时区口径统一（UTC → 本地） | 新增 `lib/date.ts` + 9 个文件 | 数据正确性：写入错误交易日 |
| 3 | `left-4.5` 无效类 → `left-[18px]` | `StrategyPage.tsx` | 交互失效：开关看不出开/关 |
| 4 | `FundDetailPage.load` 补异常处理 | `FundDetailPage.tsx` | 可用性：失败卡死「加载中…」 |
| 5 | `handleAdd` 依赖数组补 `txnTime` | `LedgerPage.tsx` | 数据正确性：交易时间被静默丢弃 |
| 6 | 锁中毒统一为 `into_inner` 恢复 | `db.rs` / `data.rs` / `ocr.rs` | 健壮性：一次 panic 放大为全库不可用 |
| 7 | 负零 + 窄屏假 `0.00%` | 新增 `lib/num.ts` + 3 个文件 | 显示正确性：绿色 `-0.00%` 自相矛盾 |

---

## 2. 逐项说明

### 2.1 云同步：网络 IO 移出全局锁闭包

**根因**：`db::with_conn` 的全局锁覆盖闭包全程，而 `cloud::push` / `pull_plan` / `pull_apply`
把 `transport.put` / `list` / `get` 都放在了闭包内 → 上传下载期间（重试可达数十秒）
所有 DB 命令（含 UI 的 `get_overview` / `get_stats`）全部排队阻塞。

**修法**：`cloud.rs` 拆成「取连接算 → **放锁**做网络 → 再取连接写」三段，命令层走分段接口；
一体化函数 `push` / `pull` 降级为**仅供内核测试**（文档注释已标注 ⚠️）。

| 阶段 | 旧 | 新 |
|------|----|----|
| push ① 生成快照 | 闭包内 | `push_plan(conn)` |
| push ② 上传 | `push()` 内 `transport.put` | 命令层在**锁外** `transport.put(&push_request(&plan))` |
| push ③ 记水位 | 闭包内 | `push_finish(conn, &plan, entry.key)` |
| pull ① 读身份/水位 | 闭包内 | `pull_basis(conn)` |
| pull ② 列清单 + 下载 | `pull_plan` / `pull_apply` 闭包内 | 命令层在**锁外** `list()` + `fetch_snapshots()` |
| pull ③ 回放 | 闭包内 | `apply_fetched(conn, &fetched, plan.len())` |

**语义等价性**：事务粒度仍是每份快照一个事务；`skipped_own` 由
`planned − fetched.len()` 精确得出（`fetch_snapshots` 只丢本机来源的条目，其余错误外抛）；
「来源以快照头为准」的回退逻辑原样保留。

**代价**：快照正文在内存中暂存（单份量级 MB 内）—— 换网络 IO 全在锁外，值。

### 2.2 时区：新增 `src/lib/date.ts` 收口

`Date.prototype.toISOString()` 是 **UTC**，GMT+8 下 00:00–08:00 会跨日偏差一天。
新建单一入口，**禁止**再写裸 `toISOString().slice(0, 10)`：

| 导出 | 用途 |
|------|------|
| `localDateKey(d)` | 本地 `YYYY-MM-DD`（业务日期键） |
| `todayStr()` | 今天的本地日期 |
| `localStamp()` | 本地 `YYYY-MM-DD HH:MM:SS`（与后端 `chrono::Local` 的 `as_of` 同格式） |
| `localFileStamp()` | 本地 `YYYY-MM-DD-HH-MM-SS`（文件名词干，无冒号） |

替换点：`LedgerPage`（默认交易日 + `todayStr` + 导入预览）、`OverviewPage`、`PositionTable`
（三处重复的 `todayStr` 合并为一处）、`ReportsPage`（`localKey` 去重 + 导出/分享文件名）、
`SyncPage`、`AboutPage`（文件名戳）、`api.ts`（mock 序列日期 + 5 处 `asOf`）。

> `asOf` 那条是**额外发现**：后端用 `chrono::Local`，mock 却用 `toISOString` → 展示时间差 8 小时。
> mock 序列日期也必须本地化，否则与组件用 `todayStr()` 算出的「今天」对不上，浏览器预览里当天恒为「—」。

### 2.3 `left-4.5`：无效 Tailwind 类

Tailwind 默认 spacing scale 含 `0.5/1.5/2.5/3.5` 但**不含 `4.5`**，本项目也未扩展
`tailwind.config.js` 的 spacing → `left-4.5` 不生成任何 CSS 规则 → 开关开启态滑块不右移。

- 轨道 `w-9`(36px) − 滑块 `w-4`(16px) − `top-0.5`(2px) = **18px**，故改 `left-[18px]`（任意值必定生成）
- 顺带补齐可达性：`focus-visible` 焦点环 + `aria-label` 指明策略代码（原有 `role="switch"` /
  `aria-checked` / `<button>` 已具备键盘激活能力，缺的是可见焦点与可读名称）
- `transition-all` → `transition-[left]`（该元素只变 left）

### 2.4 `FundDetailPage.load` 异常处理

缺 try-catch 时：命令失败 → `setLoading(false)` 不执行 → **页面永久停在「加载中…」**。
按 `OverviewPage` / `StatsPage` 同一范式补齐：`setError(null)` 开头 + catch 上屏 +
`finally` 收 loading + 错误态带「重试」按钮（渲染顺序 `loading && !data` → `error` → `!data`）。

> `loadSeries` **未动**：它不管理 loading 状态，失败不会卡死；把它接到页面级错误态反而会把
> 一个已经能正常显示（只是图表为空）的页面整页替换成「加载失败」，得不偿失。

### 2.5 `handleAdd` 依赖数组

函数体内用了 `txnTime.trim()` 但 deps 里没有 `txnTime` → `useCallback` 返回旧闭包 →
提交时写入初始值 `""`，用户填的交易时间被静默丢弃。已补入依赖。

### 2.6 锁中毒恢复

`Mutex::lock()` 在持锁线程 panic 后返回 `Err(PoisonError)`，裸 `unwrap()` 会把
「一次局部失败」放大成「此后每个 DB 调用都 panic」。统一改为
`unwrap_or_else(|e| e.into_inner())` —— 本项目真正保护的状态只有一个
`rusqlite::Connection`，SQLite 连接没有会被半途破坏的内存不变量（事务由 SQLite 自己回滚），
中毒后继续用是安全的。

- `db.rs`：新增 `pub fn lock_db()`，替换 4 处生产调用点（含 `with_conn`）+ 1 处测试辅助
- `data.rs`：新增 `cal_cache_guard()` / `cal_loaded_guard()`（缓存是可再生状态）+ `throttle_wait` 内联
- `ocr.rs`：`ensure_engine` 的引擎锁（MNN 引擎初始化后是只读推理对象）
- 剩余 2 处 `lock().unwrap()` 在 `cloud.rs` 的 `#[cfg(test)]` HTTP 打桩内，不属生产路径

### 2.7 负零 + 窄屏假百分比

新建 `src/lib/num.ts` 导出 `FLAT_EPSILON` / `normalizeFlat()`，两处展示组件共用同一判定：

- `TrendChip`（`ui.tsx`）：此前用**裸 `value < 0`** 判色、却用 `value !== 0` 判前缀 →
  `-1e-10` 渲染成 **绿色下行箭头 + `-0.00%`**（颜色说跌、文本说零）。归一后图标/颜色/文本同源。
- `GainLossBadge`：`isFlat` 只作用于颜色与符号，`fmtPct(-1e-10)` 仍得到 `-0.00%`（nav 格式得到 `-0.0000`）。
  归一后三个格式一起收敛，flat 渲染与「真正的 0」完全一致（muted 色、无 `+` 前缀）。
- `PositionTable` 窄屏估算列：`estVal == null ? '—' : <badge estPct ?? 0>` → `estPct ?? 0`
  会把「不知道」谎报成「持平 0.00%」。改为与桌面同列（`:339`）同口径：`estPct == null → '—'`。

---

## 3. 门禁与端到端验证

| 门禁 | 结果 |
|------|------|
| `npx tsc -b` | ✅ exit 0 |
| `npx vitest run` | ✅ **15 files / 104 passed**（改前 13 / 96，新增 `date.test.ts` 5 + `num.test.ts` 3） |
| `cargo test --lib --no-default-features` | ✅ **257 passed / 0 failed / 6 ignored**（与改前基线一致） |
| `cargo check --lib --no-default-features` | ✅ 0 warning |
| `npm run build` | ✅ built in 11.46s |

**产物级验证**（`dist/assets/index-BAXgMb19.css`）——这是第 3 项修复的机器证据：

```
left-4\.5      → 无规则        （证明确实是无效类）
left-\[18px\]  → left:18px     （证明修复生效）
left-0\.5      → left:.125rem  （关闭态未受影响）
```

---

## 4. 按要求**不做** / 暂缓的项

| 项 | 处置 | 原因 |
|----|------|------|
| 北交所 `nq` → `bj` 前缀 | ❌ **不做** | 实测 `bj` 与 `nq` 返回的名称/现价/昨收完全一致，`data.rs:427` 解析不看首字段 → 误报 |
| `chartTheme` 加缓存 | ❌ **不做** | 17 个 `readColorVar` 调用点全在 `useMemo([theme])` 内 → 误报；照改可能引入 stale 颜色 |
| 启用 SQLite WAL | ⏸ **暂缓** | 单连接 + 全局 Mutex 串行架构下并发优势用不上，却会引入 `-wal`/`-shm` 影响备份/导出/云同步链路完整性 |

另外两项建议（`data.rs` 的 `Client::builder()` 池化 17 处、`TrendChip` 与 `GainLossBadge` 去重）
属 P1/P3 且改动面较大，未纳入本轮。

---

## 5. 未做的验证（诚实标注）

- **真实持仓截图端到端复验**（`TEST_IMG=... cargo test --test ocr_e2e`）未跑 —— 本轮未触碰 OCR 权重与字典。
- **App 启动冒烟**未跑 —— 本轮未出包，Rust 侧仅有 `cargo test` 单元级验证；
  第 1 项（云同步三段式）的**真实云通道端到端**（配好 endpoint/token 后点「推送」「拉取」）
  需要在出包后手工验证一次。单元测试覆盖的是内核语义等价性，不覆盖「锁是否真的在网络上不持有」。
- 建议出包后进行：①云同步推送/拉取各一次并观察 UI 是否仍响应 ②记账页填交易时间提交后回读
  ③策略页开关的开/关两态视觉 ④跨日（00:00–08:00）记一笔账看默认日期。
