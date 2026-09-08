# FundLens v2.4.0 发版记录 — 穿透 P2（重合钻取 + 风格箱）+ 数据源修复 + 境外画像补拉

日期：2026-09-08 ｜ 分支：main `e4ed731` / 麒麟 `bd70b68`（只同步未打包）｜ Android：本批次不打 APK

## 1. 版本与交付物

| 项 | 值 |
|---|---|
| 版本 | 2.3.0 → **2.4.0**（package.json / package-lock / tauri.conf.json / Cargo.toml / Cargo.lock 五处同步） |
| 桌面 | ✅ `/Applications/FundLens.app` v2.4.0 已部署（mtime/大小/版本三重核验 + 启动冒烟通过） |
| Android | ❌ 无明确要求，未打包（用户 2026-09-08 指示） |
| 麒麟 | ✅ 代码同步至 `bd70b68`（merge main e4ed731，保留 Tauri1 五处适配），未打包 |
| 提交 | main `e4ed731`（18 文件 +1224/−74）；kylin `bd70b68`；均已推送 origin |

## 2. 功能（P2，用户"做1、2"中的 ①钻取+风格箱 与 ②境外补拉）

### 重合矩阵钻取
- 后端 `lookthrough_overlap_detail(code_a, code_b)`：`holding_vector`（每股取最大披露权重）→ `OverlapDetail`
  （weight_overlap / jaccard / common_count / common: Vec<CommonHolding{code,name,weight_a,weight_b}> / as_of）
- 前端：重合矩阵单元格与窄列表行可点击 → 钻取卡片列出两基金共同持仓明细（股票/代码/在A权重/在B权重/重合贡献=min）
- 命令 + ACL：`fl-lookthrough-overlap-detail`

### 风格箱九宫格（价值/核心/成长 × 大/中/小）
- 后端 `style_box()`：市值分档 大 ≥1e11 / 中 ≥1e10 / 小；风格按市值加权中位 PE 锚定，0.8×=价值、1.25×=成长；
  PE≤0/缺失 → 无估值行；`stock_style` 新表 + `upsert/missing/list`；境外市值单独一行
- `refresh_stock_style()`：只拉 A 股 6 位（555 只），走全局节流 + 失败退避（与 fetch_stock_profiles 同款）
- `lookthrough_style(platform)`：只读聚合，毫秒级无网络
- 前端：风格箱 Tab（懒加载）3×3 + 境外/无估值补充行 + 「补风格快照」按钮
- 命令 + ACL：`fl-lookthrough-style`、`fl-refresh-stock-style`

### 架构去重
- 抽 `lookthrough_funds(platform)` 统一加载器，消除 lookthrough_overlap / lookthrough_fund / overlap_detail 三重重复

## 3. 关键修复：画像/风格数据源切 push2delay（阻断级，实测发现）

- 症状：真实库补拉 623 只仅 4 成功；curl push2.eastmoney.com → 000/exit 52（连接被重置）
- 根因：`push2.eastmoney.com` DNS 解析到 **198.18.0.11**（本机代理 fake-ip 保留段 198.18.0.0/15），该域名规则缺失 → 黑洞；
  其余东财 host（fundgz / pingzhongdata / api.fund / fundsuggest）与 push2delay 均 200 正常
- 影响：push2 仅被 P0/P1/P2 画像/风格拉取使用（App 既有行情走腾讯/新浪，从不经过 push2）→ **App 内「补行业画像/补风格快照」在当前网络同样必失败**，属首个触达该 host 的功能
- 修复：data.rs 两处 `push2.eastmoney.com` → `push2delay.eastmoney.com`（东财官方延迟行情镜像；
  行业/风格为准静态数据，延迟无影响；已注释根因防回退）。实测 A股/港股全字段一致（f57/f58/f116/f127/f162/f167）

## 4. T60 境外画像补拉（真实库，无头执行）

方式：临时 `#[ignore]` 测试复用 `fetch_stock_profiles` / `refresh_stock_style`（与 App 内按钮完全同源），
FUNDLENS_DATA_DIR 指向 `~/Library/Application Support/com.fundlens.app`，跑完即删（未进提交）。
库已备份：`/tmp/fundlens-pre-t60-20260908.db`（7.9M）。

| 数据 | 待拉 | 成功 | 失败 |
|---|---|---|---|
| 行业画像（A股555 + 港股68 + 美股0） | 623 | **622**（A554 + HK68） | 1（005930） |
| 风格估值（A股） | 555 | **553** | 2（005930、000660） |

- **港股 68 只行业画像全部落库**（例：中国移动→电讯、华虹宏力→半导体、新华保险→保险→金融地产）
- 风格样例：恒瑞医药 3034 亿 / PE34.0 / PB4.71；百济神州 4011 亿 / PE61.3；荣昌生物 693 亿 / PE7.4

## 5. 已知边界（非回归，记录备查）

- **韩股**：QDII 披露中三星 005930 / SK海力士 000660 以原生代码入库：
  - 005930 无 KR secid 支持 → 拉取失败，正确落未分类兜底；
  - 000660 与 A 股 *ST南华 代码碰撞 → 被误归 A 股（行业 "-"→未分类，披露侧名称仍显示 SK海力士，实际影响仅"未分类"归属）
- **L1 未分类 141/622**：东财行业（电讯/工业工程/一般金属等）尚无 `sector_l1_of` 映射 → 待补映射表 + 重派生（后续项，P0 映射表覆盖度优化）
- **口径开关未做**：jjcc topline 实测无效，全量持仓不可得（铁证已注释 data.rs），维持前十大口径
- **kylin 本机 tsc**：因本机 node_modules 为 main 的 api v2（漂移），kylin 的 v1 子模块导入报 TS2307；
  合并树依赖正确（api ^1.6.0），Docker 权威构建按分支 lockfile 不受影响；kylin cargo check ✅ / vitest 33 ✅

## 6. 质量门槛（合并前全绿）

| 检查 | 结果 |
|---|---|
| tsc -b（main） | 0 错误 |
| vitest run（main） | 47 passed |
| cargo test --no-default-features（main） | 134 passed（+5 P2 新增） |
| cargo check --lib（kylin 合并后） | 通过（1 外观性 warning） |
| vitest（kylin 合并后） | 33 passed |
| ACL codegen | acl-manifests.json 含三项 fl-lookthrough-* |

## 7. 测试指引（App 内人工验收）

1. 打开基金穿透（策略信号上方入口）→「重合」Tab：点击任意高重合度单元格/行 → 应出现共同持仓钻取卡片
2. 「风格」Tab：首次展示全为境外/无估值是正常的 → 点「补风格快照」→ 九宫格出现 A 股大/中/小 × 价值/核心/成长分布
3. 「行业」Tab：港股基金（如 QDII）应能细分出港股行业（此前境外细分恒为 0，现已有 68 只画像）
4. 桌面 v2.4.0 已在 /Applications（未签名 ad-hoc，macOS 首次打开需右键→打开）
