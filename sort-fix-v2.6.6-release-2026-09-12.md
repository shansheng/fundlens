# FundLens v2.6.6 发版记录（2026-09-12）

## 缺陷：持仓表「估算收益 / 估算收益率」排序错乱 ✅

- **现象**：首页持仓列表按「估算收益」「估算收益率」点表头排序，顺序与显示值对不上（休市/盘后时段尤其明显）。
- **根因**（与此前「当日」列同类错位）：排序比较器 `sortValue` 对这两列直接取原始字段 `p.dayPnlEst / p.dayPnlPctEst`，而**显示**口径自 v2.6.4 起在非盘中时段回填上一交易日估算 `lastDayPnlEst`。休市日原始字段恒 0 → 排序按全 0 稳定序（=传入序），显示却是回填值 → 「显示 A 序、排 B 序」。
- **修法**：排序与显示逐字段对齐——
  - 盘中：按 `dayPnlEst / dayPnlPctEst`（不变）。
  - 非盘中：按 `lastDayPnlEst`（null → NaN 沉底）；收益率基数 = `marketValue − dayPnlAct`，与显示侧 `estPct` 完全一致。
  - `useMemo` 依赖补 `marketSession`。
- **回归测试**：`「估算收益/估算收益率」排序按单元格实际展示口径`（升/降序 + 收益率列，非盘中回填值驱动排序）。

## 门禁

| 项 | 结果 |
|---|---|
| tsc | 0 错 |
| vitest | 78/78 |
| 版本 | 五处同步 2.6.6 |
| commit | main：9c30665（fix）→ 升位提交 |

## 产物

- macOS：2.6.6 部署 `/Applications` + 冒烟
- Android：`FundLens-2.6.6-arm64.apk`
- 麒麟：main 同步 + cargo check（不打包）
