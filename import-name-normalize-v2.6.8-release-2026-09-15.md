# v2.6.8 发版记录 — 导入流水名称规范化（代码为唯一身份）

> 日期：2026-09-15
> 分支：main → feat/kylin-v10-aarch64
> 类型：缺陷修复（P0 数据正确性）

---

## 1. 问题

用户上报：导入流水后，基础持仓出现重复项，且入库了大量不规范的基金名称。

### 排查结论（真实库实证）

把「昨天导入的 44 条流水」涉及的 34 个代码逐个联网核验，**代码全部正确**（如 015789 = 永赢高端装备智选混合发起A）。
问题不在代码解析，而在**名称直接采用 OCR 原文入库**：

| 代码 | 入库（脏） | 规范名称 |
|------|-----------|---------|
| 001594 | `基金丨天弘中证银行ETF联接账` | 天弘中证银行ETF联接A |
| 011839 | `基基金丨天弘中证人工智能主题ETF联接C` | 天弘中证人工智能主题ETF发起联接A |
| 012324 | `基基金丨兴全恒惠30天持有期超短债债券A` | 兴全恒惠30天持有超短债A |
| 012452 | `基金丨国泰利优30天滚动持有短债债券C` | 国泰利优30天滚动持有短债A |
| 012722 | `基金平安中证光伏产业指数` | 平安中证光伏产业指数A |
| 014424 | `基基金丨博时恒生医疗保健ETF联接(QDII)A` | 博时恒生医疗保健ETF发起式联接(QDII)A |
| 015789 | `基永赢高端装备智选混合` | 永赢高端装备智选混合发起A |
| 018691 | `基基金丨兴全恒盛90天持有期债券A` | 兴全恒盛90天持有债券A |
| 021399 | `基基金丨广发中证红利ETF联接` | 广发中证红利ETF发起式联接A |
| 520920 | `基金丨天弘恒生科技ETF联接` | 天弘恒生科技ETF |
| 023895 | `天弘上证科创板综合指数增`（截断） | 天弘上证科创板综合指数增强A |

噪声特征：OCR 把界面上的「基金丨」标签栏与名称连读，形成 `基金丨…` / `基基金丨…` / `基…` 前缀，或末尾截断。
同一只基金在不同截图里噪声写法还不一样 → 一旦名称参与身份判定，就会拆出多条持仓。

### 代码层缺陷

1. `db::import_transactions` 的 funds 入库直接用 `it.fund_name`（OCR 原文），未按代码反查规范名。
2. 流水导入预览（`import_txn_screenshots`）解析不到代码时**静默放行**（持仓截图导入路径有 `continue` 跳过，流水路径没有），空/脏代码会一路带到入库。
3. `parse_fund_search` 兜底「评分全 0 时取接口首条」——名称噪声大时会把流水挂到不相干的基金上。

> 说明：`apply_txn_to_position_conn` 本身只用 `(account_id, fund_code, platform)` 定位持仓，**未**带入名称，这一条符合预期，本次未改。

---

## 2. 修复

### 2.1 代码是唯一身份，名称不参与判定
- `commands::import_transactions` 增加**代码门禁**：`fund_code` 非 6 位数字直接返回错误，列出是第几条、叫什么，要求补全代码后再导入。
- `db::import_transactions` 增加同款门禁（双保险，防御其它直调路径）：非法代码整条 `continue`，不建 funds、不建持仓。
- 前端流水预览提交前同样校验，代码列对未解析行标红并提示「需补代码」。

### 2.2 入库名称一律「代码 → 规范名」
- 新增 `data::fetch_fund_name(code)`：复用东方财富 fundsuggest（key=代码），取该代码的 NAME。
- 预览阶段：代码一旦确认为真实 6 位，**立即用规范名覆盖 OCR 原文**，用户在预览表看到的就是规范名（新增 `codeResolved` / `nameNormalized` 两个标记位）。
- 导入阶段：对本地 funds 中尚不存在的代码联网反查规范名；已存在的代码 db 层不改写名称（保持 ON CONFLICT 只补 platform）。

### 2.3 收紧搜索兜底
`parse_fund_search`：多条候选却全部评分为 0 时**不再取首条**，返回 None（仅唯一候选才采信）。避免噪声名称被随机挂到不相干基金。

---

## 3. 历史数据清理

已备份 `~/fundlens-backup/fundlens-2026-09-15-before-name-cleanup.db`（33MB），随后按「代码 → 规范名」修正 11 行 `funds.name`，清理后噪声名称残留为 0。
（仅改名称，不触碰 positions / transactions；持仓按 `(code, platform)` 定位，不受影响。）

---

## 4. 变更文件

```
src-tauri/src/data.rs        新增 fetch_fund_name；parse_fund_search 兜底收紧；新增 1 测试
src-tauri/src/db.rs          import_transactions 代码门禁 + 注释；新增 1 测试
src-tauri/src/commands.rs    import_txn_screenshots 规范名覆盖 + 两个标记位；
                             import_transactions 代码门禁 + 规范名补查
src/api.ts                   ImportTxnOut 增 codeResolved / nameNormalized
src/pages/LedgerPage.tsx     代码列标红提示 + 提交前代码校验
```

---

## 5. 门禁

| 项 | 结果 |
|----|------|
| `npx tsc -b` | 通过（0 错误） |
| `npx vitest run` | 78 passed / 11 files |
| `cargo test --lib --no-default-features` | 227 passed / 0 failed / 6 ignored（新增 2） |

新增测试：
- `db::tests::import_txn_identity_is_code_plus_platform_ignoring_dirty_names`：同一基金用 4 种噪声写法导入，持仓恒为 1 行且份额累加到 400；名称顶替代码的行被整条跳过。
- `data::tests::parse_fund_search_rejects_ambiguous_when_no_score`：多条候选全 0 分时不猜，返回 None。

---

## 6. 部署

- macOS：`/Applications/FundLens.app` 2.6.8
- Android：`FundLens-2.6.8-arm64.apk`
- 麒麟分支：同步代码

---

## 7. 遗留

- 名称清理只覆盖本次识别出的噪声特征（前缀污染 / 截断）。若后续发现其它形态的脏名称，可用同一思路按代码批量重刷。
- 导入预览新增的「需补代码」提示未做 i18n（项目本身无多语言需求）。
