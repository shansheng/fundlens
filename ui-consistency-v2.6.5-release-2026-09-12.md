# FundLens v2.6.5 发版记录（2026-09-12）

## 背景：桌面/Android 四项不一致反馈

用户在 v2.6.4 实机使用后反馈 4 项问题（桌面 macOS vs Android 行为不一致）。

## 缺陷 1：估算/实际标签要求只显示日期，不要「上一交易日」前缀 ✅

- 原实现：休市时标签为「上一交易日 MM-DD」（OverviewPage/PositionTable/FundDetailPage 三处）。
- 修复：统一改纯日期口径——
  - 总览宽屏 tile：`估算收益 09-11` / `实际收益 09-11`（日期取 lastNavDate，无日期回退无后缀）。
  - 窄屏汇总条角标：`09-11`（原「上一交易日 09-11」）。
  - 持仓表「当日」列：`08-25`（无日期回退「实际」）；「估算收益」列标签同理。
  - 基金详情页「当日收益」角标、估算 tile 标签同步。
- 盘中「当日估算/当日实际」语义不变（当天数据本来就该标当日）。

## 缺陷 2：桌面版休市日误显「今日估算」口径（Android 正确）✅ —— 根因在后端

- **根因**：`record_daily_snapshot`（commands.rs）每日首查为全部持仓 upsert `nav_date=今天` 的 position_daily 行，**休市日也写**。周六写入的 192 行实际是周五净值（official_nav 走 nav_on_or_before 回退），但日期标成了今天 → `last_nav_date`（众数口径）= 今天 → `showLastBadge=false` → 前端把周五数据当「当日」展示（PositionTable 全部持仓 dayIsToday=true → 标「当日实际」）。
- **为何 Android「正确」**：Android 今天尚未打开 App、未触发当日写入，其 position_daily 末条仍是 09-11，标签正确。两端代码一致，纯数据时点差异。
- 修复（双管齐下）：
  1. `record_daily_snapshot`：非交易日**跳过**持仓行写入（组合 snapshots 快照行为不变）。
  2. 新增 `db::purge_nontrading_position_daily`：清理历史已写入的非交易日行（幂等，按 is_trading_day_cached 逐日判断；周末静态判定、法定节假日走日历缓存），随总览加载自动执行。实测库中 2026-09-12 的 192 行将被清理。
- 附带测试：`purge_nontrading_position_daily_removes_weekend_rows`（周五行保留/周六行清除/幂等）。

## 缺陷 3：买入/卖出红绿标志桌面丢失 + 流水时间列重复日期 ✅

- 3a 根因：FundDetailPage 的 `TxnTag` 是灰色中性徽标（`bg-border/60`），与 LedgerPage 的红绿 `TxnBadge` 不一致。修复：统一红绿语义（买入=红/流出，卖出=绿/流入）， FundDetailPage 交易记录表与 LedgerPage 流水表配色一致。
- 3b 根因：真实库 `transactions.txn_time` 存完整时间戳（如 `2026-09-02 22:19:22`，3850 行），流水表「时间」列原样显示导致与「日期」列重复。修复：显示层 `timeOnly()` 只取 HH:MM，不动数据。

## 缺陷 4：其他页面按首页移动端优化原则适配 ✅

- 8 页 sweep（提交 `269cd86`，工程师 sweep + lead 补完 Lookthrough/Sync/Import 三页留白）：
  - **LedgerPage**：流水表窄屏切 5 列精简表（日期含时间下沉 / 类型 / 基金 / 金额 / 操作），份额·批次·备注隐藏；OCR 预览表 min-w 820→680（sm: 恢复）；删除按钮触控目标加大。
  - **FundDetailPage**：估值拆解/交易记录表 min-w 420（sm:520）+ 窄屏 text-xs；净值走势图高度 300→220（窄屏）；页留白 p-4 sm:p-6。
  - **StatsPage / StrategyPage / ReportsPage**：页留白 p-4 sm:p-6。
  - **LookthroughPage / SyncPage / ImportPage**：页留白统一 p-4 sm:p-6（三页已有 overflow-x 容器，窄屏横滚可控）。
  - 桌面 ≥md 零回归（全部用响应式前缀/narrow hook 分支）。

## 门禁与产物

| 项 | 结果 |
|---|---|
| tsc | 0 错 |
| vitest | 77/77（含标签口径断言更新） |
| cargo test | 224 passed / 6 ignored（共 230，含新增 purge 测试） |
| 版本 | 五处同步 2.6.5（package.json / package-lock / tauri.conf.json / Cargo.toml / Cargo.lock） |
| macOS | 构建部署 /Applications + 冒烟（见发版流程） |
| Android | FundLens-2.6.5-arm64.apk |
| commit | main：cefcb4b（fix）→ efabab0（升位）→ 4923e43（gitignore）→ 269cd86（sweep） |

## 事故记录

- 版本升位提交时误将 `fl-build-android.sh`（含 keystore 密码，规定永久不入库）带入提交，发现后立即 `git rm --cached` + `--amend`（amend 发生在 push 前，远端从未收到），并补 `.gitignore` 条目防复发。

## 口径提醒

- position_daily 恒只含交易日行（含回填口径一致）；货基周六发布的万份收益不进该表，与其「不参与当日盈亏」的既有口径一致。
- 验证建议：周一开盘后开桌面 App，当日估算应正常显示；周六打开时显示 09-11 日期标签。
