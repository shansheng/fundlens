# 慢操作异步化改造 v2.6.3（2026-09-11）

## 背景与目标

用户要求「看一下应用还有哪些是比较慢的操作，改成异步」。全量排查 70 个 tauri 命令后，锁定穿透页（LookthroughPage）三个逐条网络请求的批量补拉命令：每个请求叠加 800ms 节流 / 连续失败 3s 退避，全量耗时 2~5 分钟且**同步占死主线程**，期间 UI 完全冻结。

## 改造内容

三个命令由同步阻塞迁移到既有 `FetchTaskState` 后台任务模式（与 `disclosure_fetch` / `nav_refresh` 同款）：`start` 立即返回 + Rust `std::thread` 后台执行 + 前端 0.8s 轮询 `_progress` + 协作式 `_cancel`（当前一只跑完即停）。幂等：任务在跑时重复 start 直接返回当前进度。

| 旧命令（已删） | 新命令三件套 | 说明 |
|---|---|---|
| `fetch_stock_profiles` | `stock_profiles_start/_progress/_cancel` | 行业画像补拉（本批首个改造，前一轮完成） |
| `refresh_stock_style` | `stock_style_start/_progress/_cancel` | 风格估值补拉（A 股 6 位码） |
| `refresh_index_constituents` | `index_constituents_start/_progress/_cancel` | 指数成分表补拉（compose+replace） |

## 改动文件

- `src-tauri/src/commands.rs`：删 `FetchStockStyleOut` / `RefreshIndexConstituentsOut` 结构体与同步函数；新增 `static STOCK_STYLE` / `static INDEX_CONSTITUENTS` 任务态 + 六个 `#[tauri::command]`（profiles 三件套在上一轮已加）。
- `src-tauri/src/lib.rs`：`generate_handler!` 换注册 9 个新命令（旧 3 个移除）。
- `src-tauri/permissions/fundlens.toml`：`fl-fetch-stock-profiles` / `fl-refresh-stock-style` / `fl-refresh-index-constituents` 三组改为 allow 新命令（组名不变）。
- `src/api.ts`：删 3 个旧函数与结果类型；新增 9 个 `*Start/*Progress/*Cancel` 函数（返回 `FetchTaskProgress`）。
- `src/hooks/useFetchTask.ts`：`FetchTaskKind` 扩为 5 种，`API_BY_KIND` 增 3 组。
- `src/pages/LookthroughPage.tsx`：三个按钮接 `useFetchTask`——运行中显示 `n/total` 进度 + 独立「取消」按钮；完成回调 `load()` / `loadStyle()` 回读并按 ok/failed/cancelled 分支 alert。
- `src/pages/LookthroughPage.test.tsx`：mock 改 `stockProfilesStart` / `indexConstituentsStart`（返回 `running:false, total:0` 的 idleDone，走完成回调路径）。

## 门禁与验证

- `npx tsc -b`：0 错误。
- `npx vitest run`：71 passed / 0 failed。
- `cargo test --lib --no-default-features`：219 passed / 0 failed / 6 ignored。
- macOS 包构建 + 部署 `/Applications` + 启动冒烟：见发版记录尾部实测数据。

## 口径不变声明

三个补拉的业务口径（收集范围、门禁、节流、退避）与原同步版**逐行一致**，仅执行位置从主线程移到后台线程；`FetchTaskProgress` 的 total = 本次缺失待补条数（风格任务不再暴露「全部股票数」字段，前端以 0/0 显式提示「无需补拉」）。
