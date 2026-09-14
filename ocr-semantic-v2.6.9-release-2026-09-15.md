# v2.6.9 发版记录：OCR 语义层降噪（层1/层2/层3 + 方案1/方案2）

- 版本：2.6.9（上一版 2.6.8）
- 日期：2026-09-15
- 分支：main → 同步 `feat/kylin-v10-aarch64`
- 性质：OCR 识别质量专项（无 UI 变更、无数据库结构变更）

---

## 1. 背景与问题定性

用户反馈「OCR 识别噪音比较大」，但明确指出：**文字识别本身问题不大，噪音集中在语义层**——
各平台持仓/流水字段的「按表格换行」与「乱加前缀」。因此本次不动「识别」之外的架构，
按三层 + 两个修复方案落地：

| 编号 | 内容 | 目标症状 |
|------|------|----------|
| 层 1 | OCR 参数调优 | 小字漏检、长名称被检测框切尾、细体字断行、水印噪音 |
| 层 2 | 模型升级 v4 → v5 | 折行长名称 / 中英混排（A500ETF联接C）识别率 |
| 层 3 | 后处理纠错 | 名称残缺/形近字导致「按名称查代码」打偏、离谱数值入库 |
| 方案 1 | 标签前缀剥离 | `基金丨天弘中证银行ETF联接A` 这类前缀污染 |
| 方案 2 | 折行回收改形状规则 | 名单外后缀（增强A/优选A/一年持有…）被当垃圾丢弃 |

> 方案 3（列对齐分析：`reconstruct_rows` 先按 x 聚成列）**未做**——待有真实样本后验证效果再实施。

---

## 2. 方案 1：标签前缀剥离

**根因**：`丨`（U+4E28）与 `丶`（U+4E36）落在 CJK 区 0x4E00–0x9FFF 内，
而 `clean_name` 的过滤规则是「保留 CJK + 字母数字 + 空白」，于是视觉分隔符被当作合法汉字保留，
产出「基金丨XXX」「基基金丨XXX」这类带前缀的名称 → 入库后同一基金出现多种写法（v2.6.8 已按代码止血，本次从源头消除）。

**改动**（`src-tauri/src/ocr.rs`）：

1. 新增 `is_visual_separator(c)`：剔除 `丨 丶 亅 | ｜ │ ┃ ▍ ▎ ▏`；`clean_name` 的 CJK 过滤改为
   `is_cjk_char(c) && !is_visual_separator(c)`。
2. 新增 `strip_label_prefix(s)`：按 `基金名称 / 基金简称 / 产品名称 / 基金 / 基` 顺序剥离，
   **每个前缀带最小剩余 CJK 字数门槛**（前四个 ≥2、裸「基」≥6），
   防止误伤以「基」开头的真实基金名（`基建工程指数A` 剥离后剩 5 字 < 6 → 不剥离，安全）。
3. `clean_name` 末尾依次执行：字符纠错 → 前缀剥离。

## 3. 方案 2：折行回收改形状规则

**原实现缺陷**：`merge_name_groups` 阶段 2 用**关键词白名单**判断碎片是否可回收
（必须以 A/B/C 结尾，或含「联接/混合/发起式/股票」），名单外的后缀（`增强A`/`优选A`/`智选A`/`一年持有`）一律不保护。

**新实现**（形状规则，与后缀内容无关）：短碎片（≤8 字符）+ y 间距 ≤170 + 左缘偏差 ≤50
+ **两组之间不存在任何数值行** → 回收进前一组。

> 「中间无数值行」是关键判据：真正换卡片时中间必然夹着金额/收益行，
> 而折行碎片与上一行之间没有。这条补足了纯形状规则在「两张卡片间距 < 170px」时会误合并的漏洞。

新增常量：`ORPHAN_MAX_FRAG_CHARS = 8`、`ORPHAN_X_TOL = 50`。

## 4. 层 3：后处理纠错

1. **本地规范名模糊匹配**：新增 `db::known_fund_names()`（只取在持仓中出现过的基金名作语料），
   `ocr::correct_name_against_known()` 依次判定：完全一致不处理 → 形近字归一化后一致 → Dice 二元组
   相似度 ≥0.86 且长度比 ≥0.6 → 替换为**唯一最优**规范名。已接入
   `import_screenshots` 与 `import_txn_screenshots`（**先纠正再按名称解析代码**，降低搜索打偏概率）。
2. **形近字表** `confusable_key()`：`O/o/〇/Q→0`、`l/I/i→1`、`S/s→5`、`帐→账`。
   ⚠️ **只用于比对，绝不写回**——基金名里的数字有语义（A500 / 沪深300），直接替换会破坏真实名称。
3. **数值范围校验**：`sanitize_fund_numbers` / `sanitize_txn_numbers`
   （净值 0.05–50、金额/份额 (0, 1e9)、收益率 |r| ≤ 500、|收益| ≤ 10×市值），
   已在 `extract_fund_rows` / `extract_txn_rows` 内置调用；交易行若金额被判离谱清零则整行丢弃。

## 5. 层 1 + 层 2：模型与参数

- **层 2**：`RustOConfig::ppv4` → `ppv5`；`download_ocr_models.sh` 改拉
  `mnn/PP-OCRv5/{det,rec}/ch_PP-OCRv5_*_mobile.mnn`（det 4.5MB、rec 16.6MB，较 v4 +6.6MB）。
  字典沿用 v4 的 `ppocr_keys_v1.txt`（RapidOCR 仓库无 v5 专属中文字典，已实测 v5 路径 404）。
  脚本新增 `MODEL_VERSION` 标记文件：版本不符或无标记但有权重 → 删除旧权重再下载，
  避免 `fetch` 跳过已存在文件而静默留下 v4 权重。
- **层 1**（在 v5 preset 之上覆盖）：

| 参数 | v4 旧值 | v5 preset | 本次取值 | 理由 |
|------|---------|-----------|----------|------|
| `det.limit_type` | max | **min** | **max** | v5 预设的 min/736 会把 1170×2532 截图压到 736×1590，小字全丢 |
| `det.limit_side_len` | 960 | 736 | **1536** | 长边限 1536，相较 v4 的 960 像素量约 2.5 倍 |
| `det.unclip_ratio` | 1.5 | 2.0 | **2.0** | 外扩文本框，减少长名称尾部被吞 |
| `det.use_dilation` | false | true | **true** | 细体字不再断成两行 |
| `global.text_score` | 0.5 | 0.5 | **0.6** | 滤掉图标/水印低置信噪音 |

- 新增 `ocr::warmup_engine()` + `#[ignore]` 自检测试 `engine_loads_ppocrv5_model`
  （`cargo test --lib -- --ignored`）：权重与预设不匹配时在加载阶段即失败，而不是识别时静默返回空。

---

## 6. 验证

| 门禁 | 结果 |
|------|------|
| `cargo test --lib --no-default-features` | **239 passed / 0 failed / 6 ignored** |
| `cargo test --lib engine_loads_ppocrv5_model -- --ignored` | **1 passed**（PP-OCRv5 权重加载成功） |
| `npx tsc -b` | 通过 |
| `npx vitest run` | **78 passed**（11 文件） |

新增单测 11 个（方案1 ×2、方案2 ×4、层3 ×4、模型自检 ×1），其中：

- `clean_name_strips_visual_separators_and_label_prefixes`：覆盖 `丨`/`▍`/`│` 与「基金名称/基金」前缀
- `clean_name_keeps_real_names_starting_with_ji`：防误伤 `基建工程指数A` / `基建工程ETF`
- `orphan_recovery_works_for_unknown_suffix`：名单外后缀「增强A」仍被回收
- `orphan_recovery_blocked_when_numbers_in_between`：中间有数值行 → 不合并
- `correct_name_against_known_fixes_lookalike_and_truncation`：`5G→SG`、尾部截断 `联接→联接C`

## 7. 影响与回滚

- 模型权重为 git 跟踪文件，本次提交包含 v5 的 `det.mnn` / `rec.mnn`（共 +6.6MB）。
- 回滚：切回 `RustOConfig::ppv4` + 运行旧版 `download_ocr_models.sh`（会按 MODEL_VERSION 自动换回 v4 权重）；
  语义层改动（方案 1/2、层 3）彼此独立，可单独 revert。

## 8. 未做 / 待样本验证

- **方案 3（列对齐分析）**：`reconstruct_rows` 先按 x 聚成列、同列 y 相邻合并——本版未实施。
- 全部三层改动均在**无真实样本**前提下按代码逻辑推演完成，实际增益需用户用真实截图验证后微调阈值
  （`ORPHAN_X_TOL` / `NAME_MATCH_MIN_SIM` / `det.limit_side_len`）。
