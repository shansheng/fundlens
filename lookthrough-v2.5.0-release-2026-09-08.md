# FundLens v2.5.0 发版记录 — 指数成分穿透（组合覆盖率 +12.43pp）

日期：2026-09-08 ｜ 分支：main `45243bc` / 麒麟 `015224e` ｜ Android：本批次不打 APK（用户明确不需要）

## 1. 版本与交付物

| 项 | 值 |
|---|---|
| 版本 | 2.4.0 → **2.5.0**（package.json / package-lock 根两处 / tauri.conf.json / Cargo.toml / Cargo.lock 五处同步；lock 根包此前滞后 2.1.1 一并归位） |
| 桌面 | ✅ `/Applications/FundLens.app` v2.5.0 已部署（PlistBuddy=2.5.0、部署 mtime 01:00:22 ≥ 构建 mtime 01:00:00、10.8M） |
| Android | ❌ 无明确要求，未打包 |
| 麒麟 | ✅ 代码同步至 `015224e`（merge main 45243bc，保留 Tauri1 适配），cargo check --lib 通过，未打包 |
| 提交 | main `9d33bd6`+`6b7c3bf`+`45243bc`+release 文档；kylin `015224e`；均已推送 origin，tag `v2.5.0` |

## 2. 背景：72% 未穿透的构成诊断（2026-09-08）

用户报「行业穿透 72% 未穿透，占比太高」。逐桶拆解后确认已近制度饱和：

| 未穿透构成 | 占比 | 性质 |
|---|---|---|
| 货基/理财（002/005） | 8.2% | 设计内不穿透 |
| 主动基金披露低覆盖（前十大 Σpct≈25%） | 56.7% | 制度天花板（jjcc 全量持仓不可得，铁证已注释） |
| **被动指数/ETF 仅按披露前十大** | 32% | **口径低估——本次 v2.5 修复** |
| QDII/新基金无披露 | 13.4% | 数据源无 |

方案裁定：指数成分穿透（用户 AskUserQuestion 选定「指数成分穿透（推荐）」），放弃程序补画像/召唤专家补披露（F10 仅公开源，专家无从补）。

## 3. 功能：指数成分穿透（v2.5，纯被动指数基金）

### 数据链（本地成分表 + 联网刷新）
- `index_constituent(index_code, stock_code, stock_name, weight, as_of)` 新表（PK 双列），幂等建表
- 抓取：东财数据中心 `RPT_INDEX_CONSTITUENT`（成分代码+名称+样本日，URL 引号须 %22 编码，否则 HTTP 400）分页拉全名单
- 权重合成：`fetch_float_mv_batch`（腾讯 gtimg parts[44] 流通市值，批次 ≤60）→ **流通市值加权近似** `w = ffmv/Σffmv`
  （东财/中证仅公开 top10 权重且 curl 受限，全权重不可得；`INDEX_EQUITY_FACTOR=0.95` 作为成分全名单的覆盖系数）
- `refresh_index_constituents` 命令：遍历持仓 → 门禁（纯被动 + 非排除 token + 解析出非 hk/105/116 指数码）→ 只拉缺失 → replace；与 refresh_stock_style 同款节流/退避
- 命令 + ACL：`fl-refresh-index-constituents`（commands.rs + lib.rs + fundlens.toml + capabilities 四处）

### 引擎分支（lookthrough.rs `aggregate`，不放大红线保持）
- 门禁 `index_constituent_code`：纯被动指数基金 && 跟踪指数非空非港/非 105/116 && 名称不含排除 token
  （黄金/金etf/原油/期货/豆粕/白银/商品/reit/纳斯达克/纳指/标普/美国/全球/海外/香港/港股/恒生/沪港深/红利/低波）&& 成分表非空
- 成分路径：`contributed = fund_mv × 0.95 × w`；`coverage = min(max(0.95×Σw,0),1)`；
  未覆盖部分 `fund_mv×(1−coverage)` 进未穿透桶；Σ个股 = 0.95×fund_mv（不放大）
- `FundInfoRow.penetration_source`：`"index_constituent"` / `"disclosure_top10"`（前端据此标注口径）
- 披露前十路径逐字节未动；L1 映射 EXACT2 49 项同批补入（未分类 141→1）

### 前端标注（穿透口径透明化）
- 穿透页顶部「刷新指数成分」按钮（confirm + refreshed/failed 小结）
- 「基金穿透明细」新增「穿透口径」列：指数成分 chip（tooltip 注明流通市值近似×0.95 非官方披露）/ 披露前十大
- 口径条改写双来源表述；`FundInfoRow`/`FundLookthroughResult` 补 penetrationSource 契约

## 4. 端到端对抗验证（真实库副本，无头，非本会话提交）

方式：复制真实库至 `/tmp/v25verify/`，临时 `#[ignore]` 测试直调 `refresh_index_constituents()`（与 App 内按钮同源），baseline→refresh→re-aggregate，跑完删除测试并清理，真实库全程未触碰。

| 指标 | before | after | Δ |
|---|---|---|---|
| 组合穿透覆盖率 | 36.96% | **49.40%** | **+12.43pp** |
| 未穿透市值 | ¥359,798 | ¥288,839 | **−¥70,959** |

- 目标指数 12/12 全部刷新成功，failed_codes=[]（000933/399975/399986/000905/000300/399995/980017/399997/399808/000932/399395/399006，成分数 17~500）
- **32 只基金**从披露前十翻转为指数成分穿透，新 coverage 恒 =0.95（纯被动全成分覆盖，符合设计）
- 代表：012043 鹏华酒C 0.75→0.95、012857 汇添富消费联接C 0.018→0.95、017516 易方达北证50 0.42→0.95、012895 天弘科创创业50联接C 0.016→0.95
- 不变量全绿：ΣL1_pct=1.000000、无 coverage 越界、个股暴露≤基金MV、未穿透桶语义保持、门禁无绕过（指数增强/商品/港股/红利/低波均正确回退披露路径）

## 5. 质量门槛

| 检查 | 结果 |
|---|---|
| cargo test --no-default-features（main） | 141 passed（+7 v2.5） |
| tsc -b（main） | 0 错误 |
| vitest run（main） | 48 passed（+1 前端新增） |
| npm run build（main） | 成功 |
| 真实库端到端 | ✅ 覆盖率提升 +12.43pp，12/12 刷新 |
| cargo check --lib（kylin 合并后） | 通过（1 既有 warning：commands.rs:5 unused import Manager） |
| 麒麟冲突 | capabilities/permissions 整删 ×2、tauri.conf 保 v1 改 2.5.0；api.ts 自动合并（kylin v1 特征保留 + main 三处改动全落地） |

## 6. 测试指引（App 内人工验收）

1. 基金穿透页 → 点「刷新指数成分」→ 等待小结（目标 12 指数，应全部成功）
2. 「行业」Tab：覆盖率应从 ~37% 升至 ~49%；持有指数基金的行业分布明显变满
3. 「基金穿透明细」：指数基金行「穿透口径」列显示「指数成分」chip，悬停有近似权重说明；主动基金仍为「披露前十大」
4. 桌面 v2.5.0 已部署（未签名 ad-hoc，macOS 首次打开需右键→打开）
