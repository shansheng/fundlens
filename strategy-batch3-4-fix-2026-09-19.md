# 第 3/4 批修复：策略引擎三项 + 报表口径对齐

> 日期：2026-09-19
> 基准：main @ `8f5bf45`（第 2 批修复后）。本批提交：`29afcf7`
> 依据：`FundLens-业务审查报告评估-2026-09-19.md` §6 的「第 3 批」⑤⑥⑦ + 「第 4 批」⑧
> 范围：策略引擎 3 项 + 报表偏差口径 1 项；含连带修出的 3 个报告未提及的缺陷

---

## 0. 先行更正的认知（与报告不同）

| 报告说法 | 实际取证 | 影响 |
|---|---|---|
| ⑦「挂单不查 `in_cooldown`」 | ✅ 结构上属实，但 **`in_cooldown` 本身恒为 `false`** —— `config::COOLDOWN_DAYS = 0`，`gap < 0` 永不成立。该常量注释为「v5.5: 冷却期 1→0 天（卖出后立即可重新入场）」→ **有意设计，非 bug** | 闸门里保留该项（与空仓买入分支同口径，未来调大即生效），但**测试只能针对另两项** |
| ⑧「先判定哪个是设计意图」 | 判定依据就在代码里：`commands.rs:4398` 注释明写「实际侧统一用『窗口内逐日实际盈亏之和』…**与估算和同口径对比**」 | 实现与单测是对的，注释与 mock 是错的 |

---

## 1. ⑤ 趋势同源重复计入

### 缺陷链

```rust
// commands_grid 装配 today_change（两个同源分支）
:670  盘中估值失败降级 → chg = (nav_hist[0]/nav_hist[1] - 1.0) * 100.0
:675  盘前/盘后/休市   → chg = (nav_hist[0]/nav_hist[1] - 1.0) * 100.0

// helpers::analyze_trend（:212）
:214  trend_navs = nav_hist.filter(|h| h.date != today)   ← 只按日期剔除
:220  hist_changes[i] = (trend_navs[i-1]/trend_navs[i] - 1) * 100      ← 同一表达式
:232  all_changes = [today_change, ...hist_changes]
```

`helpers.rs:176` 的移植注释认为「凡是 `date == today` 的记录剔除就够了」——**这只覆盖
`source=="nav"` 且今日净值已发布的情形**。当今日净值**尚未发布**时
`nav_hist[0].date != today`，不被剔除，而 `today_change` 与 `hist_changes[0]`
是同一表达式的两次计算 → 最新一日计两次。

连带偏移：`consecutive_down`（一次下跌算两天 →「连跌≥3」实际 2 天触发）、`short_3d`、
`mid_10d`、`volatility`、`volume_proxy`。

### 报告未提及的两处（本次一并修掉）

**(a) `nav0_adj` 被放大**：同源时 `navs[0]` 本身就是"当前净值"，再乘
`(1+today_change/100)` 会凭空多算一个涨幅（1.05 → 1.1025，不是任何真实净值）。

**(b) `latest_is_today` 恒为 false**：它取自已剔除 today 的 `trend_navs`，
故 `else` 分支是死代码。移除后判据改由 `same_source` 承担。

### 修法（`helpers.rs:232-242`）

```rust
let latest_is_today = nav_hist.first().map_or(false, |h| h.date == today);
let nav_diff_latest = if nav_hist.len() >= 2 && nav_hist[1].nav > 0.0 {
    (nav_hist[0].nav / nav_hist[1].nav - 1.0) * 100.0
} else { f64::NAN };
let same_source = !latest_is_today && (nav_diff_latest - today_change).abs() < 1e-9;
```

> **为什么用重算式而不是比对 `hist_changes[0]`**：后者已 `py_round(,2)`，
> 而 `today_change` 未 round，直接比会引入半个刻度的**假阴性**（1.052631 vs 1.05）。
> 重算式与调用方是同一表达式 → 严格相等，`1e-9` 容差成立。

`nav0_adj` 同步改为 `same_source` 时取 `navs[0]`（不放大）。

---

## 2. ⑥ 净空估算口径统一 + 死分支

```rust
// 修复前 engine.rs：三条路径两种口径
有持仓 + oldest>0 → estimate_current_nav(...)                  ← 休市时返回最新净值，不施加涨幅
有持仓 + oldest≤0 → nav_hist[0] * (1.0 + today_change/100.0)   ← 裸乘
无持仓            → nav_hist[0] * (1.0 + today_change/100.0)   ← 裸乘
```

休市（`market_closed = !intraday`）时，`today_change` 本就来自
`nav_hist[0]/nav_hist[1]-1`，裸乘等于**把同一涨幅施加两次**。

**报告只说了"口径不一致"，未指出实际业务后果**：挂单因此**漏触发** ——
最新净值 1.05、触发价 1.05，裸乘得 1.1025 > 1.05 → 永不触发。

修法：三分支统一走 `estimate_current_nav`（`engine.rs:81`），锚定值用
`batches.find(is_holding).map(nav).filter(>0).unwrap_or(0.0)`。

同时清掉 `estimate_current_nav`（`helpers.rs:1167`）内的死分支：

```rust
if latest.date == today_str { return latest.nav; }
return latest.nav;                       // ← 两条路同值
```

`today` 参数只服务于该恒同值判断 → **连同参数一并移除**（调用方仅 2 处，均已改）。

---

## 3. ⑦ 挂单闸门 + 「已触发未执行」中间态

### 3.1 闸门（`engine.rs:89-92`）

挂单块此前位于一切闸门之前、命中即 `return`。`in_cooldown` 在其上方第 59 行已算出
却从未被它使用。现与空仓买入分支同口径：

```rust
let gate_blocked = in_cooldown
    || vol_state == "extreme_vol"
    || (source == "estimation" && confidence < 0.5);
if !gate_blocked && nav_for_check > 0.0 && nav_for_check <= pr.trigger_nav {
```

被拦时挂单**保留、不消费**，留待下次检查。

### 3.2 中间态 `notified`（`db.rs:4382` + `commands_grid.rs:744`）

```
修复前：引擎给出建议 → grid_pending_transition(pid,"triggered")  → 挂单销毁
        ⇒ 用户没真买入也再收不到提醒（"建议给出即销毁"）

修复后：引擎给出建议 → grid_pending_mark_notified(pid)  → 挂单留在 active
        用户点「已买入」→ grid_pending_confirm → triggered → 关闭
        用户点「取消」  → cancelled
```

配套改动（**报告未提及**）：
- `grid_pending_list_active` 的过期清理与主查询都纳入 `notified`（`db.rs:4329/4337`）
  —— 否则挂单在给出建议当天就从列表消失。
- `grid_pending_add` 的软上限统计纳入 `notified`（`db.rs:4299`）
  —— 否则触发后不再占额度，**可无限建单**。
- `grid_pending_transition` 条件放宽为 `status IN ('pending','notified')`，
  且 `triggered_date = COALESCE(triggered_date, today)` 保留首次触发日。

新增 Tauri 命令 `grid_pending_confirm`（`commands_grid.rs:406`），
按「三步」注册：`lib.rs` + `permissions/fundlens.toml`（`fl-grid` 组）+ 前端 `api.ts`。
前端 `StrategyPage.tsx` 挂单区新增 `notified → 「已触发待确认」` 状态与
「已买入 / 取消」双按钮。

---

## 4. ⑧ 报表「估算偏差」口径三方对齐

| 位置 | 修复前 | 修复后 |
|---|---|---|
| `commands.rs:4296` 结构体注释 | `est_delta_pnl − delta_pnl` ❌ | `est_delta_pnl − act_delta_pnl` ✅ |
| `commands.rs:4423` 实现 | `est_delta_pnl − act_delta_pnl` ✅ | 不动 |
| `commands.rs` 单测 `est_act_diff == -60.0`（"偏差 = 20 − 80"） | ✅ | 不动 |
| `api.ts:318` 类型注释 | `estDeltaPnl − deltaPnl` ❌ | `estDeltaPnl − actDeltaPnl` ✅ |
| `api.ts:733` mock `estActDiff` | `estDeltaPnl − deltaPnl` ❌ | `estDeltaPnl − actDeltaPnl` ✅ |
| `api.ts:735` mock `diffRate` | 同上 ❌ | 同上 ✅ |
| `ReportsPage.tsx` 文案 | `偏差 = 估算 − 实际` | **本就正确，不动** |

`actDeltaPnl` 由 `series.reduce((acc,s) => acc + s.dayPnl, 0)` 求得（`api.ts:721`），
与后端 `act_delta_pnl += s.day_pnl` 同源。

**后果差异**：`deltaPnl` 是期初/期末**存量差**（少算区间首日），
`actDeltaPnl` 是**逐日和** → 两者差一个首日项（mock 里为 218 元）。
故修复前**浏览器预览与桌面端显示的偏差是两个不同的数**。

---

## 5. 测试与变异验证

新增 8 条（Rust 7 + 前端 1）。

| 断言 | 变异操作 | 变异后结果 |
|---|---|---|
| `analyze_trend_dedups_same_source_today_change` | `same_source = false` | **FAILED** ✓ |
| `analyze_trend_nav0_adj_not_inflated_when_same_source` | `same_source = false` | **FAILED** ✓ |
| `engine_pending_rebuy_uses_latest_nav_when_market_closed` | 恢复裸乘 | **FAILED** ✓ |
| `engine_pending_rebuy_blocked_by_low_confidence_estimation` | 旁路闸门 | **FAILED** ✓ |
| `pending_rebuy_notified_stays_active_until_user_confirms` | `list_active` 去掉 notified | **FAILED** ✓ |
| `pending_rebuy_soft_cap_counts_notified` | count 去掉 notified | **FAILED**（`left:4 right:0`）✓ |
| `api.report.test.ts` mock 口径 | 退回 `deltaPnl` | **FAILED**（`expected 0 to be greater than 1`）✓ |

两条**反向护栏**（变异后仍应通过，用于防"修过头"）：
- `analyze_trend_keeps_estimated_today_change` —— 盘中估值来源不得被去重误删
- `analyze_trend_keeps_today_change_when_nav_published` —— 今日净值已发布时应计入

> ⚠️ 变异测试中的一次**有效教训**：首轮变异 `list_active` 时
> `pending_rebuy_soft_cap_counts_notified` **未失败** —— 因为它的判据在
> `grid_pending_add` 的 count 查询（另一处改动），与 `list_active` 无关。
> 补做第二次针对性变异后才验证通过。**一处逻辑改动若落在两个 SQL 里，
> 必须分别变异验证。**

---

## 6. 门禁

| 门禁 | 结果 |
|---|---|
| `npx tsc -b` | ✅ exit 0 |
| `npx vitest run` | ✅ **105 passed**（16 files，基线 104 + 1） |
| `cargo test --lib --no-default-features` | ✅ **271 passed / 0 failed**（基线 264 + 7） |

变异测试完毕后已 `git checkout -- src-tauri/` 回滚，复跑 **271 passed** 确认工作树与
提交一致。

---

## 7. 行为变更（用户可见）

- 趋势类指标（连跌天数、3/5/10/20 日累计、波动率、量能代理）在**今日净值未发布时段**
  不再重复计入最新一日 → 信号判定更保守、不再误报"连跌"
- 休市时挂单净值不再被放大 → 此前**永不触发**的挂单现在能正常触发
- 低置信度（盘中估算 `confidence < 0.5`）与极端波动时挂单不再触发
- 挂单触发后**不再自动关闭**，状态显示「已触发待确认」，需点「已买入」关闭；
  未确认则持续提醒至过期或取消
- 浏览器预览页的「估算偏差」将与桌面端一致

---

## 8. 未做 / 未验证（诚实标注）

- ⏳ **GUI 未实机走通**：挂单区新增的「已买入 / 取消」按钮只过了 `tsc` 与单测，
  **未在桌面端渲染验证**（未出包）
- ⏳ **中间态的跨天行为未实测**：`notified` 挂单在次日重算时会被重新评估
  （`estimate_current_nav` 用当日净值），实际交互未走通
- ⏳ 移动端未实机验证
- ⏳ ⑧ 的 mock 修复只覆盖 `mockReport`（周报）；日报/月报/年报共用同一函数 ✓
- ✅ 按要求不做：未改 `COOLDOWN_DAYS`（有意为 0）、未动麒麟分支
- 📌 麒麟分支 `feat/kylin-v10-aarch64` **未同步** `5be3fc3`/`dba6389`/`fa19453`/`8f5bf45`/`29afcf7`

---

## 9. 变更记录

| 日期 | 变更内容 | 原因 | 影响范围 |
|------|----------|------|----------|
| 2026-09-19 | ⑤ 趋势同源去重 + `nav0_adj` 修正 + 清死分支 | 最新一日重复计入致趋势指标全面偏移 | `helpers.rs`、`strategy/mod.rs` |
| 2026-09-19 | ⑥ 净值估算三分支统一 + 清 `estimate_current_nav` 死分支与 `today` 参数 | 休市时涨幅施加两次致挂单漏触发 | `helpers.rs`、`engine.rs` |
| 2026-09-19 | ⑦ 挂单受风控闸门约束 + `notified` 中间态 + `grid_pending_confirm` 命令 | "建议给出即销毁挂单"；软上限未计 notified 可无限建单 | `engine.rs`、`db.rs`、`commands_grid.rs`、`lib.rs`、`permissions/`、`api.ts`、`StrategyPage.tsx` |
| 2026-09-19 | ⑧ 报表估算偏差口径三处对齐 | 浏览器预览与桌面端偏差数字不同 | `commands.rs`（注释）、`api.ts` |
