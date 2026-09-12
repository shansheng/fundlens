# 当日收益休市回显 + 盈亏日历/报表持仓×净值重算 v2.6.4（2026-09-12）

## 用户报告的两个缺陷

1. 「当日估算收益，当日实际收益过了 12 点就没有了，应该是第二日开盘前还显示。如果当日不开盘，一直显示上次开盘的记录。」（经确认「12 点」= 午夜 0 点跨日）
2. 「盈亏日历数值不对，应该根据持仓和净值计算。日报、周报、年报也如此，估值可以靠当日快照。」

## 根因（诊断 + 数据对账证实）

### 缺陷 1：前端整块隐藏，后端其实算对了
- 后端 `market_phase()`（data.rs:258）三态正确（intraday/post_close/closed），午休 11:30–13:00 判 intraday；休市时 `day_pnl_act`（valuation.rs:443，不被时段门控）算出的就是上一交易日实际值。
- 真凶：`OverviewPage.tsx:138` `showDay = marketSession !== 'closed'` 隐藏总开关 → 头条/窄屏在休市时变 `—`，PositionTable `hideEst` 让估算列整列消失。

### 缺陷 2：snapshots.day_pnl 是「两次快照市值差」，混入盘中估算净值
- 真实库证据：2026-09-09 假盈亏 **−450,806**、09-10 **+444,892**（同期零交易）；新旧对拍 13/17 天符号或量级不一致。
- 次因：`sum_cash_flow_on` 只算 deposit/withdraw 漏 buy/sell（R2）。
- **对账关键结论**：`transactions` 是不完整台账（约 28.6 万元持仓为截图导入直写 positions、无流水）→ 正向重放 199 持仓 0 个对得上 → **必须用 Route 2（以 positions 为锚反向回推份额）**。

## 已裁定的口径（硬约束，勿回退）

1. 份额序列 = Route 2：`shares(d) = positions.shares − Σ(该日后流水 delta)`；无流水持仓起点 = nav_history 最早日。
2. 净值缺口顺延上一交易日净值（否则出现缺口恢复日暴涨，如实测 09-03 +42,198 伪动）。
3. 成本基准 = `positions.cost_amount` 汇总（交易现金流只用于当日 cashflow 项）。
4. `day_pnl(d) = Σ_f mv(f,d) − Σ_f mv(f,d_prev) − cashflow(d)`；`d_prev` = 上一交易日。
5. 回填历史日的 `day_pnl_est` = `day_pnl_act`（已收盘日最终估算收敛为实际，代码注释已说明）。
6. `valuation.rs` 的 day_pnl_act / reference_nav / hasDayActual / baseline 硬口径零改动。

## 改动清单

### 后端（commit c15d6db，含 WIP fcb59af）
- `db.rs`：`rebuild_position_daily`（Route 2 回填，幂等）+ `ensure_position_daily_backfill`（init 一次性回填，meta 标记守卫）+ `latest_position_daily_map` 等访问函数；`aggregate_position_daily_by_day` 日聚合。
- `commands.rs`：持仓行新增 `last_day_pnl_est/last_day_pnl_act/last_nav_date` + `summary.last_nav_date`；`record_daily_snapshot` 逐仓 upsert 当日 position_daily（发版起估算值自动累积）；`build_period_report` / `get_pnl_calendar` 数据源从 snapshots 改为 position_daily（SnapshotPoint/PeriodReport 字段名不变）；`rebuild_position_daily_command` 手动重跑入口。
- 权限：`fl-rebuild-position-daily` 组 + capability 挂载 + gen/schemas 重生成。

### 前端（commit 723c081 + fcb59af）
- OverviewPage/PositionTable/FundDetailPage：去掉 closed 整块隐藏；休市显示「上一交易日 MM-DD」（带日期角标）；估算列休市回填 `lastDayPnlEst`（null 则 `—`）；横幅/窄屏文案改准确表述。
- ReportsPage：6 处「已剔除出入金」→「已剔除当日买入卖出现金流」。
- 测试：新增 OverviewPage.test.tsx（4 用例）+ PositionTable 用例同步；覆盖降级路径（新字段缺失不崩）。

## 门禁与实测

| 项 | 结果 |
|---|---|
| `tsc -b` | 0 错误 |
| `vitest run` | 77/77 |
| `cargo test --lib --no-default-features` | **223 passed** / 0 failed（含重写 3 个报表窗口测试 + 新增 4 个回填单测：幂等/净值顺延/买入日/清仓日） |
| 副本库回填实测 | 2466 行 / ~2.0s；二次 rebuild 行数一致（幂等） |
| 新旧对拍 | 旧 snapshots 17 天合计 −435,723（假）；新口径 +80,300（自洽） |
| 真实库备份 | `fundlens-backup-before-pd-20260912-151445.db`（9.5M，回填前手动备份） |

## 已知局限（诚实记录）

- 截图导入直写 positions、无流水的持仓，回填起点 = 其基金 nav_history 最早日（代码注释已标），导入前的历史市值按当前份额外推，属可解释近似。
- 历史日 `day_pnl_est` 以实际值代替（已收盘收敛），发版起新增的每日落库才是真实估算。
