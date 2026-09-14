# v2.6.7 发版记录 — 同基金多平台持仓修复

> 日期：2026-09-14
> 分支：main → feat/kylin-v10-aarch64
> 类型：缺陷修复（P0 数据正确性）

---

## 1. 问题与根因

用户上报两条（均以 008923 建信医疗健康行业股票A 为例，该基金同时在支付宝与京东金融持有）：

| # | 现象 | 根因 |
|---|------|------|
| 1 | 京东平台估值 3.12%，支付宝平台估值 1.92% | `get_overview` 用 `db::position_id_map()`（fund_code → positions.id）取持仓 id，同基金多平台时两行折叠成同一个 id → `position_daily` 只落一行（jd 胜出），支付宝行缺当日数据，休市回显取到上一交易日的旧估算 |
| 2 | 从支付宝持仓点进基金详情，仍显示京东平台 | `compute_fund_detail(code)` 的 platform 取自 `funds.platform`。funds 表以 `fund_code` 为主键、无平台维度，该列只是导入期残留（008923 唯一行记为 jd_finance），故任何平台入口都显示京东 |

### 证据（真实库 `~/Library/Application Support/com.fundlens.app/fundlens.db`）

```
positions: id=243 (008923, alipay, shares 2000,   cost 2420.92)
           id=419 (008923, jd_finance, shares 1227.99, cost 1600)

position_daily 2026-09-14:
  419 | 2026-09-14 | est_nav 1.601111 | day_pnl_pct_est 0.031245  (3.12%)
  243 | —— 缺 09-14 行，最新为 09-11 (-1.77%)
```

当日 202 条持仓只落了 195 行 position_daily（7 个重复 fund_code 被折叠）。

---

## 2. 修复

### 2.1 持仓 id 一律取真实 `positions.id`

- `db::HoldingRow` 新增 `position_id: i64`；`list_holdings` / `get_holding` 的 SQL 增加 `p.id`。
- `get_overview` 循环内 `let pid = h.position_id;`（原为 `pos_id_map.get(&h.code)`）。
- **删除** `db::position_id_map()` 并留注释说明不可回退——该函数的前提「单账户单基金只有一条持仓」与唯一索引 `(account_id, fund_code, platform)` 直接冲突。

影响面：`position_daily` 落库与休市回显（`latest_position_daily_map`）均按 `position_id` 定位，改用真实 id 后各平台互不覆盖。

### 2.2 详情页按入口平台解析

- `get_fund_detail(code, platform: Option<String>)` → `cached_fund_detail(code, platform)`，缓存 key 改为 `code@platform`（同基金多平台各自一份快照，避免串台）。
- `compute_fund_detail(code, platform)`：持仓定位改为按 `(code, platform)` 精确命中；`FundMetaOut.platform / platform_name` 取命中持仓行的 platform，仅在未命中持仓时回退 `funds.platform`。
- 份额 / 成本 / 单位成本随之取该平台持仓行。

### 2.3 前端

- `api.ts`：`getFundDetail(code, platform?)`；新增 `fundDetailPath(code, platform)` 生成 `/fund/:code?platform=xxx`。
- `PositionTable`（窄屏 + 宽屏两处）、`StatsPage` 跳转链接带 platform。
- `FundDetailPage` 用 `useSearchParams()` 读 platform 并作为 load 依赖。

---

## 3. 变更文件

```
src-tauri/src/db.rs           HoldingRow +position_id；两处 SQL 取 p.id；删除 position_id_map()
src-tauri/src/commands.rs     overview pid=h.position_id；cached/compute/get_fund_detail 增 platform；
                              FundMetaOut.platform 取持仓行；新增回归测试
src/api.ts                    getFundDetail(code, platform?) + fundDetailPath()
src/components/PositionTable.tsx  两处跳转带 platform
src/pages/StatsPage.tsx           跳转带 platform
src/pages/FundDetailPage.tsx      读 query platform 并传入
```

---

## 4. 门禁

| 项 | 结果 |
|----|------|
| `npx tsc -b` | 通过（0 错误） |
| `npx vitest run` | 78 passed / 11 files |
| `cargo test --lib --no-default-features` | 225 passed / 0 failed / 6 ignored（新增 1） |

新增回归测试 `commands::tests::same_fund_multi_platform_keeps_distinct_position_rows`：
断言同基金两平台 position_id 不同、各自落 position_daily 后按 id 分别命中、详情页按 platform 返回对应平台名与份额。

---

## 5. 部署

- macOS：`/Applications/FundLens.app`（版本号 2.6.7，启动冒烟通过）
- Android：`FundLens-2.6.7-arm64.apk`
- 麒麟分支：同步合并（固定三处冲突处理），代码同步不出包

---

## 6. 遗留

- `transactions` 表无 platform 字段，详情页「交易记录」仍是该基金全平台流水，未按平台过滤。
- 旧版漏写的 alipay 历史 position_daily 行需 `rebuild_position_daily` 回填；本次修复后当日数据自动补写。
