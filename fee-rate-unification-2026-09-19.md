# 第 2 批修复：卖出费率双口径统一 + 非零费率回归测试

> 日期：2026-09-19
> 基准：main @ `dba6389`（第 1 批修复后，工作区干净）
> 依据：`FundLens-业务审查报告评估-2026-09-19.md` §6 的「第 2 批（费率口径统一 + 补非零费率测试）」
> 范围：费率口径 1 处换算边界 + 2 条回归测试 + 2 处注释锚定；**未改**前端、未改校验、未改引擎
> 行号基准：**修复后**。`commands_grid.rs` 因新增换算函数整体下移 20 行（装配点 修复前 695 → 修复后 715；校验 330 → 350）

---

## 1. 问题：同一字段两种口径，缺口在唯一装配点

`sell_fee_rate` 在链路上被两套口径各自理解，**中间缺一次换算**：

| 环节 | 位置 | 口径 | 例（用户填 0.5%） |
|------|------|------|-------------------|
| 前端输入换算 | `StrategyPage.tsx:196` `Math.round(n*100)/100/100` | 小数 | 0.005 |
| 前端回显 | `StrategyPage.tsx:582` `Math.round(c.sellFeeRate*10000)/100` | 小数 | 0.005 → 显示 0.5 |
| 校验上界 | `commands_grid.rs:350` `(0.0..=0.02)` | 小数 | 0.02 = 2% |
| 存储 | `db.rs:3850` `json!({"sell": r})` | 小数 | `{"sell":0.005}` |
| **装配（缺陷点）** | **`commands_grid.rs:715`** | **未换算** | **0.005 直接喂引擎** |
| 引擎使用 | `engine.rs:172/335/470…` `fee_rate / 100.0` | **百分数** | 期望 0.5 |
| 引擎止损线 | `helpers.rs:767` `effective_stop = stop_loss_adj - fee_rate` | **百分数** | 期望 0.5 |
| 引擎文案 | `engine.rs:237/255` `"将产生{}%高费率"` | **百分数** | 期望显示 0.5% |

**实算后果**：填 0.5% → 引擎按 `0.005/100 = 0.00005` 计，也就是 **0.005%**，
费率被低估 **100 倍**。且引擎文案直接把该值拼进用户可见告警
（`"灾难保护卖出将产生0.005%高费率"`）——**用户能直接看见错值**。

### 为什么旧测试看不见（关键）

`strategy/mod.rs` 的 4 个 `StrategyInput` 夹具（:116/151/194/234）**全部**写
`sell_fee_rate: 0.0`。0 ÷ 100 仍是 0，**100 倍偏差在数学上不可观测**——
这正是 bug 能存活的原因。

### 为什么这是"一处口径冲突"而非散乱错误

引擎侧 8 个使用点口径**完全自洽**（一律百分数）：

- `engine.rs:172/173/192/193/335/337/470/471` 一律 `/100.0`
- `engine.rs:237/255` 文案直接拼 `fee_rate` + `%`
- `helpers.rs:588` `profit_pct <= fee_rate*2.0`（`profit_pct` 为百分数）
- `helpers.rs:767` `effective_stop = stop_loss_adj - fee_rate`（`stop_loss_adj` 为百分数）
- `helpers.rs:621` `fee_drag = -fee_rate*5.0`、`helpers.rs:1199` `max(1.5, fee_rate*2.5+…)`

全链路**唯一**的构造 `StrategyInput` 生产入口是 `commands_grid.rs:715`
（`grep "StrategyInput {"` 仅命中此处 + `mod.rs` 测试），所以缺口是**单点**。

### 另一个同名取值点不能动（易踩）

`commands_grid.rs:286` 也有 `cfg.fee_schedule.as_deref().map(db::fee_schedule_sell_rate)`，
但它是 `GridConfigOut` —— **存储 → 前端回显**通道，前端按小数还原展示。
**它绝不能乘 100**（乘了会显示 50%）。这正是"只在 `:712` 换算"的精确含义，
已在两处函数文档里显式标注防混淆。

---

## 2. 决策：为什么**不**采纳评估报告推荐的方案

评估报告 §6 推荐「存储=百分数」：改前端 2 处 + 后端校验改 `0.0~2.0`，引擎 `/100` 不动。

**本批改用「存储=小数，只在装配边界 ×100」**，决定性论据是**存量数据**：

| | 方案 A：存储改百分数（报告推荐） | 方案 B：只改边界换算（本批采纳） |
|---|---|---|
| 改动面 | 前端 2 处 + 校验 1 处 = 3 处 | **1 行** |
| 存量 DB（已存 0.005） | ⛔ **需写数据迁移**：不迁移则 0.005 被新口径当成 0.005% → **错得更彻底** | ✅ **自动修正**：存量 0.005 即刻按 0.5% 正确计算 |
| 回显链路 | 需同步改 `:582` 与 `:286`，漏一处即显错 | 不动 |
| 回归风险 | 3 处协同 + 迁移脚本，任一处漏改即新 bug | 单点，且被测试与注释双重锁定 |

方案 B 与"最小改动修正存量"原则一致：**存量数据一次性全部变正确，零迁移脚本**。

---

## 3. 修法：把换算做成命名边界，而非结构体字面量里的裸乘法

### 3.1 新增命名纯函数（`commands_grid.rs:22-40`，签名在 :38）

```rust
/// ⛔ 费率口径唯一换算边界：存储/前端/校验 = 小数，引擎 = 百分数。
pub(crate) fn engine_sell_fee_rate(stored_fee_schedule: Option<&str>) -> f64 {
    stored_fee_schedule.map(db::fee_schedule_sell_rate).unwrap_or(0.0) * 100.0
}
```

> 为什么抽函数而不是直接写成 `….unwrap_or(0.0) * 100.0`：
> ① 口径边界需要**名字**（`engine_sell_fee_rate` 自带"转给引擎"语义）；
> ② 可被单测直接调用，无需搭 DB；③ 变异测试可精确定位到这一行。

### 3.2 装配点切换（`commands_grid.rs:715`）

```diff
- sell_fee_rate: cfg.fee_schedule.as_deref().map(db::fee_schedule_sell_rate).unwrap_or(0.0),
+ sell_fee_rate: engine_sell_fee_rate(cfg.fee_schedule.as_deref()),
```

### 3.3 注释锚定（防后人误"对齐"两个同名取值点）

- `db.rs::grid_save_config`：标注"此处存**小数**……喂引擎前必须经 `commands_grid::engine_sell_fee_rate` ×100"
- `db.rs::fee_schedule_sell_rate`：标注"返回**存储口径 = 小数**，不是引擎口径"，并指明两个下游（前端回显直接用 / 引擎须换算）

**未改动**（保持小数口径，有意为之）：`StrategyPage.tsx:196/582`、`api.ts`、
`commands_grid.rs:350` 校验、`commands_grid.rs:286` 回显。

---

## 4. 新增回归测试（`commands_grid.rs` 新增测试模块）

该文件此前**无任何测试模块**，本次新建。

```rust
#[test] fn stored_sell_fee_is_scaled_to_engine_percent()      // 边界换算
#[test] fn converted_fee_rate_is_observable_in_fee_sensitive_helper()  // 换算后引擎可见 + 反证判据
```

第 2 条同时固化**缺陷态判据**，让"断言真的能发现问题"可复算：

```
0.5%  →  max(1.5, 0.5*2.5   + max(0.3, 1.0)) = 2.25   ← 修复后
0.005% → max(1.5, 0.0125    + max(0.3, 1.0)) = 1.5    ← 缺陷态（漏乘 100）
```

覆盖用例：`0.005→0.5`、`0.02→2.0`（校验上界）、`None/非法 JSON/缺 sell 字段/显式 0` → `0.0`（不 panic）。

---

## 5. 门禁结果

| 门禁 | 命令 | 结果 |
|------|------|------|
| 类型检查 | `npx tsc -b` | ✅ exit 0 |
| 前端单测 | `npx vitest run` | ✅ **104 passed**（15 files） |
| Rust 单测 | `cargo test --lib --no-default-features` | ✅ **260 passed / 0 failed / 6 ignored**（基线 258 + 新增 2） |

### 变异测试（证明断言非恒绿）

临时把 `engine_sell_fee_rate` 的 `* 100.0` 去掉（还原缺陷态）：

```
test commands_grid::tests::converted_fee_rate_is_observable_in_fee_sensitive_helper ... FAILED
test commands_grid::tests::stored_sell_fee_is_scaled_to_engine_percent ... FAILED
test result: FAILED. 0 passed; 2 failed
```

两条断言**双双失败**，恢复 `* 100.0` 后 260 passed 全绿 → 断言确实锚定该缺陷。

---

## 6. 行为变更提示（用户可见）

修复后，**已配置费率的基金其信号会发生变化**——这是预期且正确的：

- 卖出信号的 `estimated_fee` 由"低估 100 倍"变为正确值（0.5% 就按 0.5% 算）
- `estimated_net_profit` 相应下调（费率成本真实计入）
- 止损线 `effective_stop = stop_loss_adj - fee_rate` 收紧（此前费率项几乎为 0）
- `calc_min_profit_buffer` 抬升（卖出最小盈利缓冲变大）
- 告警文案 `"将产生{}%高费率"` 由 `0.005%` 变为 `0.5%`

**若用户此前觉得"费率填了没效果"，正是本 bug 所致。**

---

## 7. 未做 / 未验证（诚实标注）

- ⏳ **GUI 实机未走通**：未在桌面端实际填写费率后观察信号文案变化（需 `zsh fl-build-desktop.sh` + 启动）
- ⏳ **存量信号未回填**：`grid_signal` / `grid_signal_history` 中**历史已落库**的
  `fee_info` / `alert_msg` 仍是旧值，未做重算或清理（仅影响历史记录展示，不影响新计算）
- ⏳ **移动端路径未实机验证**（与费率链路无差异，但未冒烟）
- ✅ 按要求不做：未改前端口径、未写数据迁移、未动麒麟分支

---

## 8. 变更记录

| 日期 | 变更内容 | 原因 | 影响范围 |
|------|----------|------|----------|
| 2026-09-19 | 新增 `engine_sell_fee_rate` 命名换算边界 + 装配点切换 + 2 条回归测试 + 2 处注释锚定 | 费率双口径致实算低估 100 倍 | `commands_grid.rs`、`db.rs`（仅注释）；行为影响所有已配置费率的策略信号 |
