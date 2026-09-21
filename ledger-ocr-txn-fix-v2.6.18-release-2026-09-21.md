# FundLens v2.6.18 发版记录 — 记账截图导入「日期静默错位」与「京东红包 = 买入」

- 版本：**2.6.18**（上一版 2.6.17）
- 日期：2026-09-21
- 分支：`main`（Tauri 2）
- 提交：`606089b`（交易块切分行底部）、`994457c`（红包 = 买入）
- 交付物：macOS `FundLens.app`(2.6.18) + `FundLens-2.6.18-arm64.apk`

---

## 1. 交付内容（为什么必须发这一版）

修的是**静默数据错误**——解析成功率不降、界面不报错，但交易日期/类型是错的，
会直接污染持仓成本与收益：

| 缺陷 | 表现 | 影响 |
|---|---|---|
| 交易块切分「行底部」被低估 | 卡片内的日期行被切成独立块 → **首笔日期为空**（前端兜底成「今天」）、**后续笔沿用上一笔日期/分钟** | 按错误交易日确认净值 → 份额/成本错 |
| 京东「红包-」未识别 | 类型落空、名称带「红包」前缀 | 该笔无法正确归类（主人裁定 = 买入） |

### 1.1 病灶（实测证死）

`extract_txn_rows` 块切分用 `r.y + max(c.h)` 估算重建行底部，该式**只在「行内所有 cell 同 y」时成立**。
而 `reconstruct_rows` 会把同一卡片的名称/金额/状态/**折行尾段**聚成一个重建行，各 cell 的 y 相差 40~60px → **低估行底部**。

京东首笔实测：行内最低 cell 是折行尾段 `合A`@y490 h61 → **真实底部 551**，旧式算得 `437+61=498`（**低估 53px**）
→ 与日期行(@y558) 的真实间隙由 **7px 被放大成 60px > TXN_GAP(45)** → 日期行自成一块；
该块只有日期、无金额/名称 → **整块被丢弃**；而 `ctx_date` **只向后传递** → 两面症状。

### 1.2 修复

| 文件 | 改动 |
|---|---|
| `src-tauri/src/ocr.rs` | ① 块切分 `row_bottom` → `max(c.y + c.h)`（**主循环 + 兜底循环两处**）；② 名称片段带坐标收集后按 `(y, x)` 排序拼接（折行尾段左缘比首行靠左 1px，原按 x 会拼出「合A中欧红利优享灵活配置混」）；③ `detect_txn_type` 置顶加 `contains("红包") → buy`；④ `txn_type_raw` 保留原文「红包」；⑤ `strip_type_kw` 前缀表加 `红包-`、`LABEL_PREFIXES` 加 `("红包", 4)` |
| `package.json` / `package-lock.json`(×2) / `src-tauri/tauri.conf.json` / `src-tauri/Cargo.toml` / `src-tauri/Cargo.lock` | 版本 2.6.17 → 2.6.18（⑤处同步） |

⛔ 关键约束：`txn_type_raw` **不可**把「红包」归一成「买入」——三平台全靠该字段反推
（京东 `转入/转出/分红/红包`、腾讯 `买入/取出`、支付宝 `买入/卖出`），归一后京东与腾讯/支付宝撞车，平台判定失效。

---

## 2. 真实数据对账（同一批真机截图，OCR 结果完全相同，纯抽取层差异）

修复前后用 `git stash push -- src-tauri/src/ocr.rs` 取得基线，对照同一张图：

### 京东「账户明细」8 笔

| # | 卡片 | 修复前 | 修复后 |
|---|---|---|---|
| 1 | 转入-中欧红利优享灵活配置混合A / 09-18 14:58:12 / 200.00 | `date=` 空、`time=` 空、名称乱序 `合A中欧红利优享灵活配置混` | `2026-09-18 14:58`、`中欧红利优享灵活配置混合A` |
| 2 | 红包-博时黄金ETF联接C / 09-17 18:48:37 / 0.11 | `raw=` 空、名称 `红包博时黄金ETF联接C` | `raw=红包`、`博时黄金ETF联接C` |
| 3 | 转入-天弘纳斯达克100指数发起(QDII)C / 09-17 18:47:56 | 分钟错成 `18:48` | `18:47` |
| 4 | 转入-银华海外数字经济量化选股混合发起式(QDII)C / 09-17 18:47:20 | ✅ | ✅ |
| 5 | 转入-华夏有色金属ETF联接A / 09-17 14:55:59 | ✅ | ✅ |
| 6 | 转入-大成高鑫股票A / 09-16 23:02:50 | ✅ | ✅ |
| 7 | 转入-南方中证A500ETF联接C / 09-16 22:59:53 | ✅ | ✅ |
| 8 | 分红-嘉实沪深300红利低波动ETF联接A / 09-15 15:55:03 | 日期错成 `09-16 22:59` | `2026-09-15 15:55` |

→ **修复前 3 处异常 → 修复后 8/8 与截图逐字一致**（日期/时间/金额/类型/名称）。

### 腾讯「交易记录」8 笔

| 表现 | 修复前 | 修复后 |
|---|---|---|
| 时间链 | 第 3~6 笔**整体错位一位**（23:11/23:09/23:07/23:06，实际 23:09/23:07/23:06/23:05） | 逐笔正确 |
| 末笔卖出 | 兴全恒惠30天持有期超短债债券A 记成 `09-16 00:45` | `09-15 16:18` |

→ **修复前 5 处异常 → 修复后 8/8 一致**。
⚠️ **腾讯受害比京东重**（5/8 vs 3/8）：腾讯日期同样在卡片下方，且卡片更高、间距更整齐，一旦误切就整体错位。
**教训：「某平台看着没问题」不能作为结论，必须做修复前/后对照实验。**

---

## 3. 质量验证

| 门禁 | 结果 |
|---|---|
| `npx tsc -b` | **0 错误** |
| `npx vitest run` | **110 passed**（16 文件） |
| `cargo test --lib --no-default-features` | **286 passed / 0 failed**（+2 新增） |
| `cargo test --lib --features ocr` | **286 passed / 0 failed** |

新增回归测试（纯几何，不依赖 OCR 模型，门禁档即可覆盖）：
- `extract_txn_rows_keeps_date_of_wrapped_name_card`（京东首笔日期不得丢/不得串值 + 名称顺序）
- `extract_txn_rows_last_dividend_keeps_own_date`（末笔分红不得沿用上一笔日期）
- `extract_txn_rows_treats_jd_hongbao_as_buy`（红包 = buy + raw 保留「红包」+ 名称剥净）
- `hongbao_prefix_not_stripped_when_remainder_too_short`（粘连写法 + `min_cjk` 守卫）

`Cargo.lock` 未跑偏：`tauri = 2.11.5`（main 口径），仅 `name = "fundlens"` 的 version 改动。

---

## 4. 交付物状态

### 4.1 macOS

| 项 | 值 |
|---|---|
| 产物 | `src-tauri/target/release/bundle/macos/FundLens.app` |
| 部署 | `/Applications/FundLens.app`（版本 **2.6.18**） |
| sha256（cargo 产物 = bundle = 部署，三方一致） | `8af479fd507bbede0d934bd450691c0e1f32c34d7c4943c65b3223006f04fec6` |
| 旧包 sha256（对照） | `544d87c1b4b6f20db9537f00ffaca5836eefd3165f29feb6bf23a78e64d23898` |
| 构建耗时 | 10m 14s |
| 启动冒烟 | ✅ 进程存活（PID 7593），近 5 分钟无崩溃日志 |
| 回滚副本 | `/tmp/FundLens.app.prev-20260921-181455` |

**二进制指纹（决定性：证明「部署的 app 就是这份源码构建的」）** —— 本轮改的 Rust 字面量旧版不存在：

| 探针 | 新版 | 旧版 | 判定 |
|---|---|---|---|
| `红包`（本轮新增） | **2** | **0** | ✅ 决定性 |
| `红包-`（本轮新增） | **1** | **0** | ✅ 决定性 |
| `LedgerPage-DNtqrPwg.js`（前端 asset 名） | **1** | **0** | ✅ 决定性 |
| `转入` / `转出-` / `分红` / `买入`（共有对照） | 2/1/5/9 | 2/1/5/9 | ✅ 对照有效（证明搜索方法不是恒 0） |

> 用 `python3` 做**字节**搜索。⛔ 别用 `strings`——只认 ASCII，中文恒不命中，新旧都 0 极易误判。

### 4.2 Android APK

| 项 | 值 |
|---|---|
| 文件 | `FundLens-2.6.18-arm64.apk`（项目根，`.gitignore` 已覆盖 `*.apk`） |
| 大小 / sha256 | 22,696,620 字节 / `0832ae91b1583d8031864cce4b2843651498956dc332669b83c8600ac495e8df` |
| 包内版本 | `versionName='2.6.18'` / `versionCode='2006018'`（`com.fundlens.app`） |
| 签名证书 SHA-256 | `787bd931068340f4b699fe669e2ecd74269c4d6b4dbeefc97af32f5d908ee37c` → **同签名，可直接覆盖安装** |
| OCR 三重校验 | ✅ `ocr check ok: model=ppocrv6-tiny`（det/rec sha256 与 `resources/ocr` 一致，且 APK 内 `assets/ocr/` 与源一致） |
| 构建耗时 | 6m 04s |
| 旧包 | `FundLens-2.6.17-arm64.apk` 保留在项目根（未跟踪，不影响仓库） |

**APK 指纹（独立复核，解包取 `lib/arm64-v8a/libfundlens_lib.so`）**：

| 探针 | 2.6.18 | 2.6.17 | 判定 |
|---|---|---|---|
| `红包` / `红包-` | **2 / 1** | **0 / 0** | ✅ 决定性 |
| `转入` / `转出-` / `分红` / `买入` | 2/1/5/9 | 2/1/5/9 | ✅ 对照有效 |

本次构建**未**改写 `src-tauri/gen/schemas/`（无新增 `#[tauri::command]`），无需补交 schema。

---

## 5. 使用说明

- macOS：`/Applications/FundLens.app` 已就位，直接启动。历史数据在
  `~/Library/Application Support/com.fundlens.app/fundlens.db`，未触碰。
- Android：`adb install -r FundLens-2.6.18-arm64.apk`（同签名，可直接覆盖）。
- **本版对已有错误数据不自愈**：此前用截图导入过的交易，其日期/类型仍是导入当时的错误值，
  如需修正请在记账页手工校对，或删掉该批后重新导入（本版会解析正确）。

---

## 6. 已知限制（勿当「已修」）

1. **腾讯折行尾段丢尾**：如「易方达亚洲精选股票(QDII)」抽成「…股票Q」——末行纯 ASCII（`DII)`）
   被 `is_name_candidate`（要求至少 1 个 CJK）拒绝。不影响名称解析命中，未修。
2. **类份额守卫对「X类」后缀与尾部截断名失效**；OCR ASCII 形近混淆 `(QDII)A` → `QDIlA` 仍为诚实失败。
3. **编译环境**：本机 CLT 的 `usr/include/c++/v1` 是空壳目录，带 `--features ocr` 的 cargo
   手跑必须 `export CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`
   （两个出包脚本已内置）。**clean build 仍会挂**，根治需重装 Command Line Tools。
4. 构建残留一条既有告警 `src/data.rs:886 unused variable: status`（非本版引入，不影响正确性）。

---

## 7. 回滚方案

macOS：
```bash
pkill -f "FundLens.app/Contents/MacOS/fundlens"
mv /Applications/FundLens.app /tmp/FundLens.app.bad
mv /tmp/FundLens.app.prev-20260921-181455 /Applications/FundLens.app   # 回到 2.6.17
```
Android：`adb install -r FundLens-2.6.17-arm64.apk`（同签名，可降级覆盖）。
代码：`git revert 994457c 606089b`（两个提交互相独立，可分别回退）。
数据库无需回滚（本版未改 schema）。

---

## 8. 未做 / 下一步

- **未同步麒麟分支** `feat/kylin-v10-aarch64`（主人本轮只要求出 macOS + APK 包）。按约定同步须在**所有出包完成后**做，避免构建期切分支污染源码；两个包出完后已在后续提交中执行。
- ✅ **已入库** `src-tauri/tests/ocr_txn_diag.rs`（5 个真机诊断：`diag_txn_code_resolution` / `diag_platform_probe` / `diag_dump_lines` / `diag_block_split` / `diag_platform_names`）。
  本版两次定位（块切分错位、红包笔）都靠它。**无 `TEST_TXN_IMGS` 时全部自动跳过（5 passed / 0.00s）**，不影响 CI 与门禁；运行需 `--features ocr` + 本机 CXXFLAGS（见 §6.3）。
- 未在本机做麒麟打包（Docker Desktop 4.71.0 起不来，约定改在麒麟机器 `git pull` 自打包）。
