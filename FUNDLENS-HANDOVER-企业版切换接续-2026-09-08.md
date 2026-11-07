# FundLens 开发接续交接说明
> 场景：WorkBuddy 从「个人版」切换到「企业旗舰版」后，在新会话里继续 FundLens 开发。
> 本文件自包含，可在新会话中直接作为上下文喂给助手，或按「第 0 节自检」逐项核对。
> 整理时间：2026-09-08 00:50（GMT+8）· 仓库远端：github.com/shansheng/fundlens（git 协议，ed25519 无密码密钥）

---

## 0. 新会话第一步：三行自检（30 秒确认环境可用）

```bash
cd /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens
git status --short && git log --oneline -1        # 期望：干净 / 2c97901(或更新)
npx tsc -b && npx vitest run 2>&1 | tail -5       # 期望：0 错误 / 39 passed
ls /Applications/FundLens.app/Contents/MacOS/fundlens   # 期望：桌面版已部署
```

三项都过 → 直接看「3. 当前状态快照」往下做；任一失败 → 按「4. 关键路径与环境」排查。

---

## 1. 切换账号（一次性动作）

- **为什么同手机号是两个账户**：WorkBuddy 登录页「个人 / 企业」两个 Tab 是**互相独立的账号体系**。个人版=微信/手机号注册（与 CodeBuddy 同体系、同积分）；企业旗舰版=公司管理员分配的企业账号（密码/SSO）。个人号无法自助升级为企业号。
- **切换入口（两种）**：
  1. 左下角**头像 → 用户菜单 → 「企业版 / 进入企业智能体」**（已登录企业账号时最快）；
  2. 或 头像 → **退出登录** → 登录页顶部选**「企业」Tab** → 用公司分配的企业账号登录。
- ⚠️ 退出登录会**中断正在执行的任务**、且**云端会话历史不互通**（本会话内容切过去看不到）。请确认没有后台任务在跑再切。
- 切换后如文件沙箱变化，重新授权本机工作目录：
  `/Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens`（以及常用目录）。

## 2. 切换后不会丢的东西（放心的依据）

| 资产 | 位置 | 是否随账号 |
|---|---|---|
| Git 仓库/代码/commit 历史 | GitHub `shansheng/fundlens`（远端） | ❌ 不随账号，随时 clone/pull |
| 本地工作副本 | `~/WorkBuddy/2026-08-13-00-26-44/fundlens` | ❌ 随机器，不随账号 |
| 项目记忆（含业务口径裁定） | 项目下 `.workbuddy/memory/MEMORY.md` + 每日日志 | ❌ 随机器，新会话打开同一目录会自动注入 |
| 用户级 Skills / Commands / MCP | `~/.codebuddy`、`~/.workbuddy/skills` | ❌ 随机器 |
| 应用真实数据库 | `~/Library/Application Support/com.fundlens.app/fundlens.db` | ❌ 随机器 |
| Android 签名 keystore | `~/.fundlens-android.keystore`（alias `fundlens`） | ❌ 随机器 |
| 云端会话历史 / 本会话上下文 | WorkBuddy 云 | ✅ 随账号，**切过去即不可见** → 靠本文件 + commit 接续 |

> 结论：切换账号对 FundLens **无实质影响**，唯一成本是"新会话没有旧对话上下文"，用本文件补齐即可。

---

## 3. 当前状态快照（截至 2c97901 / 2026-09-08）

### 3.1 技术栈与架构
- **桌面主线 main**：Tauri 2 + Rust（`src-tauri/`，SQLite 本地库）+ React 18 + Vite + TS + Tailwind + recharts。
- **移动端 Android**：同构前端，`tauri android`（aarch64 产物；gen 工程在 `src-tauri/gen/android/`）。
- **麒麟分支** `feat/kylin-v10-aarch64`：Tauri 1 适配（webkit2gtk-4.0 / glibc 2.31），与 main **永久分叉**，功能需双端同步，**每轮收尾需把 main 改动同步过去并重打包**（见 §5 同步提醒）。

### 3.2 最近一轮已完成（本轮交付，勿重复做）
1. **基金明细双图合一**（FundDetailPage）：删除独立「持仓成本走势」卡片 → 单张净值图 =
   单位净值实线 + 累计净值虚线（仅分红/拆分时 `hasAccNav` 才叠加）+ **持仓成本琥珀虚线参考线**
   （`costPoints[0].unitCost`，v9 当前均价，线上标签「成本 x.xxxx」）+ 买入▲红/卖出▼绿/分红◆琥珀；
   Y 轴手动 domain 纳入成本线并留 8% 呼吸区；移除 recharts `<Legend>`，自绘 `KeySwatch`
   图例形符与图中图形同源。commit `2c97901`。
2. **Reports 盈亏日历 P0 修复**：硬编码 `rgba(220,38,38)/rgba(22,163,74)` → `chartTheme.withAlpha(color, alpha)`
   （新增，解析 rgb()/hex→rgba），gain/loss 令牌随主题切换。StatsPage 饼图本就令牌化，未动。
3. **桌面 macOS 已部署**：`/Applications/FundLens.app`（mtime 00:45:01 晚于产物与源码，已核验无漂移），已启动。
4. **Android APK 构建中**：构建命令/签名步骤见 §5「发版与出包」。

### 3.3 功能面（区别于竞品的关键口径，已裁定、勿回退）
- **v9 持仓模型**：`positions` 为权威，`transactions` 纯账本；录/删流水=增量更新/反向撤销；截图导入、改份额/成本**直写 positions 不产生流水**（ACL：`fl-update-position` / `fl-update-position-cost`）。
- **三分支统一口径**（总览/明细共用）：货基/理财(002/005)=份额×官方净值；6位码基金→`compute_position_metrics`；金额兜底。收益率分母恒=昨收市值，成本分母=eff_cost（阈值 1e-9）。
- **当日实际=官方**：`day_pnl_act` 恒=份额×(official_nav−baseline)，绝不 stale 回退估算；昨收基准单源 `funds.prev_nav`>0 优先官方昨收否则 `prev_nav_from_history`。
- 红涨绿跌（`--color-gain`红/`--color-loss`绿）；图表/UI 颜色一律走 CSS 令牌（`readColorVar` 渲染期解析），**除 #fff/#000 外禁止源码硬编码色值**（P0）。
- ⚠️ 空平台持仓回填会撞唯一键 → `init_db` 已有 rowid 子查询防御，新增/导入持仓必须带 platform。
- ⚠️ **新增 Tauri 命令三步缺一不可**：`commands.rs` 写 → `lib.rs` `generate_handler!` 注册 → `permissions/fundlens.toml` + `capabilities/default.json` 双处补 ACL（否则前端 invoke 报权限错）。

---

## 4. 关键路径与环境

| 项 | 路径 / 命令 |
|---|---|
| 前端源码 | `src/pages/`、`src/components/`、`src/api.ts`、`src/chartTheme.ts` |
| 前端类型检查 | `npx tsc -b` |
| 前端单测 | `npx vitest run`（39 passed） |
| 生产构建 | `npm run build`（tsc -b && vite build） |
| 桌面构建 | `cd fundlens && SDK=$(xcrun --show-sdk-path) && export CXXFLAGS="-I$SDK/usr/include/c++/v1" CFLAGS="-I$SDK/usr/include" MNN_LIB_DIR="$PWD/src-tauri/vendor/mnn/prebuilt" && npm run tauri build` |
| 桌面产物 | `src-tauri/target/release/bundle/macos/FundLens.app` → 部署 `rm -rf /Applications/FundLens.app && cp -R <产物> /Applications/`（先 `pkill -f ".../MacOS/fundlens"`） |
| 部署核验 | `stat -f "%Sm"` 比对 /Applications 二进制 mtime 是否晚于构建产物与源码（strip 后 strings 不可靠） |
| Rust 侧测试 | `cargo test --lib --no-default-features`（macOS 撞 ARG_MAX，勿直接 cargo test） |
| Android 出包 | `npx tauri android build -t aarch64 --apk` → 产物 `src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`，签名：`zipalign -f -p 4` + `apksigner sign`（keystore `~/.fundlens-android.keystore`，alias/pass 见项目记忆，勿外发）→ 重命名 `FundLens-<版本>-arm64-<说明>.apk` |
| 应用真实 DB（复现用） | `~/Library/Application Support/com.fundlens.app/fundlens.db` |

---

## 5. 收尾/发版流程（每轮必做，含下一轮提醒）

1. **代码收尾**：`git add/commit` → `git push origin main` → 报 commit hash。
2. **麒麟同步（下一轮必做）**：main 本轮含多处 UI/口径改动，需按「同步 main→kylin」要点合并到
   `feat/kylin-v10-aarch64` 并进 fl-build 容器重打包（`api.ts`、`lib.rs`(无 dialog plugin)、`db.rs`(path_resolver 返回 Option) 三文件不能整覆盖；前端其余可整覆盖）。
3. **发版版本号四处同步**：`package.json`、`package-lock.json`(root 两处)、`src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml`(+ Cargo.lock fundlens 条目)；`git tag -a vX.Y.Z` 后 push main+tag 触发 windows-build workflow（注意其 create_release 用 `vars.APP_VERSION` 且需手动 dispatch，MCP 无该工具，发布页需 GitHub UI 手动）。
4. **Android APK 已交付（2026-09-08 00:53）**：`fundlens/FundLens-2.1.1-arm64-charts-unified.apk`（29M），
   apksigner 签名证书 SHA-256 `787bd931...` 与项目 keystore 一致，已核验。**无需再出包**，除非有新改动。

---

## 6. 给新会话的建议开场白（可直接粘贴）

> 我是 FundLens 开发者，刚把 WorkBuddy 从个人版切到企业旗舰版，请先读交接说明
> `fundlens/FUNDLENS-HANDOVER-企业版切换接续-2026-09-08.md` 并按第 0 节做三行自检；
> 然后请核对 3.2 与 5.4（Android APK 是否已签名交付、麒麟分支是否已同步），
> 若都完成则继续做「（此处填你下一个需求）」。
