# FundLens 交接说明 — 企业旗舰版 → 个人版切换接续

- 交接时间：2026-09-08 03:00（企业版积分用尽）
- 项目路径：`/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens/`（**项目本体在旧工作区，不在当前会话工作区**）
- 当前分支：`main` @ `862b38d`（已 push，与 origin 同步）
- 项目记忆：`/Users/sheng/WorkBuddy/2026-08-13-00-26-44/.workbuddy/memory/`（读 `2026-09-08.md` + `MEMORY.md`）
- 发版 skill：`~/.workbuddy/skills/fundlens-release/SKILL.md`（发版全流程，P2 出包时直接复用）

---

## 0. 接续三行自检（新会话第一件事）

```bash
git -C /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens status --porcelain   # 期望仅 3 个未跟踪文件（见 §6）
/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" /Applications/FundLens.app/Contents/Info.plist   # 期望 2.3.0
(cd /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens && npx tsc -b && npx vitest run)                       # 期望 0 错 / 47 passed
```
另可选跑 Rust：`cargo test --manifest-path /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens/src-tauri/Cargo.toml --lib --no-default-features` → 期望 129 passed。

**三个自检全绿 = 状态与本文描述一致，可直接从 §4 接着干。**

---

## 1. 状态快照

| 项 | 状态 |
|---|---|
| main | `862b38d`（= origin/main，工作区干净） |
| `feat/kylin-v10-aarch64` | `9357040`（= origin，已同步 main，**未打包**） |
| `feat/android` | `0f11c6a`（旧分支，暂不用） |
| 桌面版 | ✅ `/Applications/FundLens.app` **v2.3.0** 已部署（26 MB，plist 核验通过） |
| Android | ✅ `FundLens-2.3.0-arm64-lookthrough-p1.apk`（29.4 MB，已签名，证书 SHA-256 `787bd931…ee37c`） |
| 麒麟 | 仅代码同步，未出包（用户明确要求） |

近期提交链：`3239049`（v2.2.0 记录）→ `e97be1e`（P1 代码）→ `a7f615d`（发版记录 + Cargo 版本对齐 2.3.0）→ `1056ebe`（交付物状态补齐）→ `862b38d`（补 android/mobile schema）。

---

## 2. 本轮（企业版期间）完成的工作

1. **切入自检**：git 干净、tsc 0 错、vitest 39/39、APK 签名核验通过；发现并修复麒麟分支落后 21 个提交。
2. **基金穿透规划** `FUNDLENS-基金穿透功能规划-2026-09-08.md`（v1.1）：行业两级 + 个股穿透 + 不放大原则。
3. **P0（v2.2.0）**：`stock_profile` 表 + `lookthrough.rs` 聚合纯函数 + `lookthrough_overview` / `fetch_stock_profiles` 命令 + `LookthroughPage`（路由 `/lookthrough`，入口在「策略信号」上方）+ 当日行业贡献。
4. **P1（v2.3.0）**：基金两两重合矩阵（`lookthrough_overlap`）、单基金穿透（`lookthrough_fund`，FundDetailPage 卡片）、行业钻取、境外细分（港美股 secid 116/105）。
5. **出包与发版**：桌面 v2.3.0 部署 + Android APK 签名；发版记录 `lookthrough-p1-v2.3.0-release-2026-09-08.md`。
6. **顺手修复**：`Cargo.toml` / `Cargo.lock` 版本长期停在 2.1.1 → 对齐 2.3.0；Android 构建生成的 `android/mobile-schema.json` 补交。

---

## 3. 不可回退的决策裁定（用户已确认，勿擅自改）

- **行业两级**：L1 = 9 大类（医药医疗/主要消费/可选消费/科技TMT/金融地产/高端制造/周期资源/基建公用·交运/国防军工）；L2 = 东财行业名直出（`industry_em`，零维护）。
- **不放大原则（口径红线）**：披露权重直接用；现金/债券/未披露一律进「未穿透桶」单列；覆盖率常驻展示；**绝不归一化虚构精度**。
- **隐性重仓预警**：同一股票被 **≥3 只基金**持有且**合计穿透占比 >5%** 触发。
- **当日行业贡献**：Σ(股票穿透市值 × 当日涨跌幅)，仅交易时段展示，复用 `quotes_cache` + 节流，不新增出站请求。
- **麒麟分支**：只同步代码，**不打包**。
- **jjcc 口径**：实测 `topline` 参数无效（10 与 200 响应逐字节相同），全量持仓 ccmx 为 JS 渲染不可用 → **维持前十大口径**，结论已注释在 `data.rs`，勿重复排查。

---

## 4. 待办与下一步（按优先级）

1. **P2（未开工，用户未发令）** — 备选池见 `lookthrough-p1-v2.3.0-release-2026-09-08.md` §9：
   - 风格箱（大/小盘 × 价值/成长九宫格，需市值与估值数据源）
   - 口径开关（中报全量 vs 前十大，待东财全量链路可用）
   - 重合矩阵钻取（点击单元格展开两基金共同持仓明细）
2. **可选清理**：根目录三个历史 APK（2.1.1 / 2.2.0 / 2.3.0，共 ~88 MB）已 gitignore，按需保留最新即可。
3. **境外画像**：仓库 DB 中港美股画像仍为 0 条，需在 App 内点「补行业画像」拉取后境外细分才生效（功能已就绪，只是数据未拉）。

---

## 5. 环境与命令速查

| 用途 | 命令 |
|---|---|
| 前端类型/测试 | `(cd <PROJ> && npx tsc -b; npx vitest run)` |
| Rust 测试 | `cargo test --manifest-path <PROJ>/src-tauri/Cargo.toml --lib --no-default-features` |
| 桌面出包 | `zsh <PROJ>/fl-build-desktop.sh`（约 7 min，含 CLT21 C++ 头修复）→ 拷 `src-tauri/target/release/bundle/macos/FundLens.app` 到 `/Applications/` |
| Android 出包 | `zsh <PROJ>/fl-build-android.sh`（约 10 min，zipalign + apksigner；keystore 密码经 `FUNDLENS_KS_PASS` 或 `~/.gradle/gradle.properties` 注入，**脚本本身不含密码**） |
| 升版本 | **三处**：`package.json`、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` + `Cargo.lock`（历史坑：Cargo 侧漏改） |

**已踩过的坑（务必遵守）**
- Shell 每条命令 cwd 会重置、`cd` 前缀易丢失 → 用绝对路径 / `git -C` / `--manifest-path`；长命令写成脚本执行。
- Edit 工具偶发「报成功但不落盘」→ 关键编辑用 python 补丁并回读验证。
- 合并提交**禁止 `git add -A`**（曾把 29M APK 误提交，靠重做合并 + force push 清理）；显式列路径。
- **构建期间不要切 git 分支**（麒麟是 Tauri 1 代码，会污染正在编译的源码）→ 先出包，再同步麒麟。
- Android 构建会改写 `src-tauri/gen/schemas/android-schema.json` 与 `mobile-schema.json`（新增权限项），它们是已跟踪产物 → 出包后检查 `git status` 并补交。
- 麒麟五处 Tauri 1 适配合并后必须保留：api.ts v1 invoke / lib.rs 无 dialog plugin / db.rs `path_resolver` / commands.rs `app_cache_dir` path_resolver / tauri.conf v1 schema；**不存在** `capabilities/`、`permissions/` 目录。

---

## 6. 未跟踪文件（**已于 2026-11-07 处理，本节降级为留档**）

| 文件 | 2026-09-08 当时状态 | 2026-11-07 起 |
|---|---|---|
| `fl-build-android.sh` | 含明文 keystore 密码，不入库 | 密码已外部化，**脚本已脱敏入库**（`d908762`） |
| `FUNDLENS-HANDOVER-企业版切换接续-2026-09-08.md` | 本地留存 | 已入库 |
| `overview-v2.1.0-release-2026-09-06.md` | 本地留存 | 已入库 |

> ⛔ keystore 本体（`~/.fundlens-android.keystore`）**仍然不入库**，`.gitignore` 已含 `*.keystore`、`*.jks`。

`.gitignore` 已含：`*.apk`、`*.idsig`、`*.app.tar.gz`、`dist*/`、`target/`、`*.db`。

---

## 7. 关键文档索引

| 文档 | 路径 |
|---|---|
| 基金穿透规划 v1.1 | `fundlens/FUNDLENS-基金穿透功能规划-2026-09-08.md` |
| v2.3.0 发版记录 | `fundlens/lookthrough-p1-v2.3.0-release-2026-09-08.md` |
| v2.2.0 发版记录 | `fundlens/lookthrough-v2.2.0-release-2026-09-08.md` |
| 发版 skill | `~/.workbuddy/skills/fundlens-release/SKILL.md` |
| 项目记忆 | `/Users/sheng/WorkBuddy/2026-08-13-00-26-44/.workbuddy/memory/2026-09-08.md` |

> 免责声明：本文涉及的持仓结构分析基于公开数据与量化方法，仅供参考，不构成投资建议。市场有风险，投资需谨慎。
