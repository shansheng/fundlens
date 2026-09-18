# 挂单 `notified` 中间态的连带回归 —— 引擎消费对象必须排除 `notified`

> 日期：2026-09-19
> 起因：第 3 批（`29afcf7`）为修复「挂单给出建议即销毁」而引入 `notified` 中间态后续查
> 严重度：**P1（功能性）** —— 延迟回补功能对第 2、3 张挂单静默失效
> 提交：`<commit>`（待填）

---

## 1. 一句话

第 3 批把挂单触发态从 `triggered` 改为 `notified`，但引擎**每轮只消费"最早一条活跃挂单"**，
而 `notified` 单按设计**留在活跃列表**里 —— 于是它永久占住"最早"位置，导致
**① 同一张单每轮重复发信号（重复吃 P3 每日 20% 买入预算）；② 后面的挂单永远轮不到检查。**

---

## 2. 取证（改之前先复现）

### 2.1 消费点

| 位置 | 事实 |
|------|------|
| `commands_grid.rs:701-713`（修前） | `db::grid_pending_list_active(&code).and_then(|rows| rows.into_iter().next())` —— **只取第一条**，原注释即写明"引擎触发检查只消费最早" |
| `engine.rs:68-112` | 命中触发价即 `return gs`，**一张单一张单轮**，命中就结束本次计算 |

### 2.2 状态机改动（第 3 批）

第 3 批之前：引擎命中 → `grid_pending_transition(…, "triggered")` → 挂单**退出**活跃列表。
第 3 批之后：引擎命中 → `db::grid_pending_mark_notified(…)` → 挂单**留在**活跃列表
（这是刻意的：见 `db.rs` 的注释"若不计入，挂单在给出建议当天就从列表里消失"）。

### 2.3 两者相交 ⇒ 回归

`grid_pending_list_active` 的排序是 `ORDER BY gp.id ASC`（最早在前）。
一张单变成 `notified` 后：

- 它仍是活跃列表的**首元素** → 引擎下一轮拿到的是**同一张单**；
- 引擎再次判定 `nav ≤ trigger_nav` 成立 → **再次**返回 buy 信号；
- 排在它后面的 `pending` 单（id 更大）**永远不会被 `.next()` 取到**。

> 修复前不存在此问题，因为被消费的单会退出活跃列表，"最早"位置自然让给下一张。
> 这是**引入中间态带来的连带缺陷**，不是原实现的 bug。

---

## 3. 修复

**核心：把"展示/额度集合"与"引擎消费对象"两个语义分开。**

### 3.1 `db.rs` —— 抽出过期兜底 + 新增引擎消费函数

```rust
/// 过期兜底清理：pending 与 notified 都要清（notified 同样占额度与"最早"位置）
fn sweep_expired_pending(conn: &Connection, fund_code: &str) -> SqlResult<()> { … }

/// 展示 / 额度语义的活跃集合（UI 显示 notified、软上限数它）——**不是**引擎消费对象
pub fn grid_pending_list_active(fund_code: &str) -> SqlResult<Vec<GridPendingRow>> { … }

/// 引擎消费对象：最早创建的 **pending** 单，最多一条（⛔ 排除 notified）
pub fn grid_pending_next_to_trigger(fund_code: &str) -> SqlResult<Option<GridPendingRow>> {
    // WHERE status='pending' AND expire_date >= today ORDER BY id ASC LIMIT 1
}
```

两个函数各自先跑 `sweep_expired_pending`，过期兜底行为不变。

### 3.2 `commands_grid.rs` —— 消费点切换

```rust
let pending_rebuy = db::grid_pending_next_to_trigger(&code)
    .ok()
    .flatten()
    .filter(|p| p.trigger_nav.is_some() && p.amount.is_some())
    .map(|p| RebuyOrder { … });
```

### 3.3 修复后语义（与修复前"一次性"语义对齐）

| 状态 | 在活跃列表 | 引擎消费 | UI 提醒 |
|------|-----------|----------|---------|
| `pending` | ✅ 占额度 | ✅ 唯一候选（最早一条） | ✅ |
| `notified` | ✅ 占额度（继续占，防无限建单） | ❌ 已给过建议 | ✅ 显示「已触发待确认」+ 已买入/取消 |
| `triggered` / `cancelled` / `expired` | ❌ | ❌ | 仅历史列表 |

**净效果**：一张单**只发一次**回补信号（回到修复前的一次性语义），但**不再从 UI 消失**；
后面的挂单能正常轮到检查；P3 买入预算不再被同一张单重复消耗。

---

## 4. 测试与变异证据

新增 3 条 db 测试（`db.rs::tests`）：

| 测试 | 断言 | 变异后表现 |
|------|------|-----------|
| `pending_rebuy_next_to_trigger_excludes_notified` | pending 单是消费对象；标 notified 后**不再**被消费；但仍留在活跃列表；确认后彻底退出 | ✅ 红 |
| `pending_rebuy_notified_does_not_starve_younger_orders` | 第一张标 notified 后，消费对象应为**第二张** | ✅ 红（`left: Some(1) right: Some(2)` —— 饿死现场） |
| `pending_rebuy_expired_notified_is_swept` | 过期的 notified 单被兜底标 `expired` 并释放额度 | ✅ 红（`left: "notified" right: "expired"`） |

**变异 ①**：`grid_pending_next_to_trigger` 的 `status='pending'` → `status IN ('pending','notified')`
（即退回缺陷态）→ 前两条测试**如期变红**，后一条不受影响。
**变异 ②**：`sweep_expired_pending` 的 `IN ('pending','notified')` → `= 'pending'` → 第三条**如期变红**。
两处变异均已恢复，复跑全绿。

---

## 5. 门禁

| 项 | 命令 | 结果 |
|----|------|------|
| Rust | `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features` | **274 passed**（基线 271 + 3） |
| 前端 | `npx vitest run` | **105 passed**（无改动，保持） |
| 类型 | `npx tsc -b` | 通过 |

---

## 6. 设计裁定（有意为之，勿再"修"）

- **`notified` 不回退 `pending`**：即使净值又涨回触发价之上。用户已收到过一次提醒，
  回退会导致同一张单反复"提醒 → 收回 → 提醒"，噪音大于收益。
- **`notified` 持续占软上限额度**（3 条上限）：这是刻意的摩擦 —— 用户应尽快确认或取消，
  否则 28 天后自然过期释放。若只数 `pending`，挂单一被触发就不再占额度，可无限建单。
- **`notified` 不计入引擎消费**：它不是"待检查"状态，而是"已检查且已告知"状态。

---

## 7. 未验证 / 遗留（诚实标注）

- ⏳ **GUI 未跑渲染**：本次为后端逻辑修复，未出包做界面级验证。
- ⏳ **跨天行为未实测**：`notified` 单跨自然日的提醒表现（UI 每天重新拉列表应能看到）
  未在真机连续两天实测，仅由 db 层状态断言覆盖。
- ⏳ **移动端未走通**：Android 端挂单区未实测。
- ⏳ **麒麟分支未同步**：`feat/kylin-v10-aarch64` 仍停在 `19fa382`（v2.6.16），
  未含 `5be3fc3` / `dba6389` / `fa19453` / `8f5bf45` / `29afcf7` / `6437b37` 及本次提交。

---

## 8. 教训（已回写 skill `verifying-ai-review-reports`）

> **状态机改动要遍历的不只是"判定点"，还有"选集顺序"。**

第 3 批当时已注意到"要遍历所有牵连判定点"，但漏了一类：**以顺序为语义的消费点**。
`"只消费最早一条"` 这种隐式顺序契约，不会在类型或断言里体现，一旦新增一个**留在集合内**
的中间态，就会从"让位给下一张"变成"永久霸占首位"。

**判别法**：改动状态机后，问三句 ——
1. 有哪些地方**按顺序取一条/取第一条**？（排序 + `LIMIT 1` / `.next()` / `[0]`）
2. 我新增/保留的这个状态，是否**留在**那个集合里？
3. 若在，它会不会**永久占据**那个位置？

三问全"是" ⇒ 就是本次这类缺陷。
