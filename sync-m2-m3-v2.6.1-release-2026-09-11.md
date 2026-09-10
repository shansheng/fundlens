# FundLens v2.6.1 发版记录 —— 多设备同步 M2 云通道 + M3 冲突裁决 + 备份闭环

- **版本**：2.6.0 → **2.6.1**
- **发版提交**：`24e5612`（版本升位）；功能区间 `d4eed72..24e5612`（19 个功能/修复提交）
- **日期**：2026-09-11
- **分支**：`main`（Tauri 2）
- **构建**：`zsh fl-build-desktop.sh`，耗时 **3 分 53 秒**
- **交付物**：`/Applications/FundLens.app`（v2.6.1，部署于 2026-09-11 01:05:33）

## 一、交付内容

本次发布把「多设备同步」从 M1 内核（v2.6.0 已含）推进到**可用的完整闭环**，共三块：

### 1. M2 云通道（后端落地）

- **架构**：`SyncTransport` 抽象四实现 —— `DirTransport`（本地目录）/ `PgRestTransport`（CloudBase PG REST 直连，`MODE_PG`）/ `HttpTransport`（自定 relay，`MODE_CLOUD`，本地跑 `relay/server.js` 验证）/ `MemTransport`（测试）。
- **选型结论**：零服务端。不依赖云函数、不依赖自建 relay；`MODE_PG` 走 CloudBase 内置 PostgreSQL 的 REST 网关直连，service_role API Key 即唯一密钥（表开启 RLS 且无 policy = deny-all，非 service_role 一律不可读写）。
- **环境**：CloudBase 环境 `sss-d3ggl1sft593f46c7` / ap-singapore / PG 实例 `pgdb-a29fpyny` / 表 `fl_sync`。建表迁移归档于 `cloudbase/migrations/20260910131035_fl_sync_store.sql`（`device + stamp` 复合主键，`kind/size/body/created_at`）。
- **命令面**：`sync_cloud_config_get` / `sync_cloud_config_set` / `sync_cloud_check` / `sync_cloud_push` / `sync_cloud_pull`。
- **UI**：同步页新增「云端同步」区块（模式选择 = 本地目录 / CloudBase(PostgreSQL 直连) / 自建 relay，基址 + 令牌配置，连通性检测）。

### 2. M3 多设备冲突对比与裁决

- **同步内核**：设备快照（父表优先导出）+ 快照 JSONL + 空 `ts` 走 LWW 规则；水位（watermark）标记已见变更，支持同一毫秒内的 tie-break。
- **冲突详情**：`sync_conflict_detail` 给出「本地行 vs 远端被拒变更」的**逐字段差异**，排除主键与 `updated_at`。
- **裁决**：`sync_conflict_resolve`（保留本地 / 采用远端）与 `sync_conflicts_resolve_all`（批量，单条坏数据不中断）。
- **⚠️ 静默丢数据高危项已闭合（本次最重要的修复）**：`positions` 的同步身份是自增 `id`，业务身份是唯一索引 `(account_id, fund_code, platform)`。跨设备各自新建同一持仓时 id 不同、自然键相同，`INSERT OR REPLACE` 撞唯一索引**不报错**，而是**先删冲突行再插**，并沿 `position_daily` 的 `ON DELETE CASCADE` 级联抹掉日线历史 —— 纯静默丢失。
  - 修法：回放前**前置检测**自然键相撞（`unique_index_columns` + `natural_key_collision`），命中即 `record_conflict` 并跳过，不复用 REPLACE；冲突详情里 `blocked_reason` 前置禁用「采用远端」并提示先合并重复记录。
  - 载荷缺列也要检出（`column_effective_defaults`）：`PRAGMA table_info` 的 `dflt_value` 是默认值**表达式原文**（`''`/`0`/`datetime('now')`），直接 `SELECT <expr>` 交给 SQLite 求值即得与真实 INSERT 一致的结果，据此补齐缺失的自然键列再比对 —— 否则跨版本同步时旧设备快照不含新列，仍是假阴性。
- **UI**：新增「数据同步」页（状态卡 / 手动导出导入 / 冲突列表 + 逐字段差异 + 裁决按钮）。

### 3. 备份闭环（原 M4 备份能力）

- 整库滚动备份模块 `backup.rs`：每日首次 + 写库前触发，保留 N 份（`sync_set_backup_keep`）。
- 命令：`sync_create_backup` / `sync_list_backups` / `sync_set_backup_keep` / `sync_restore_backup` / `sync_delete_backup`。
- **恢复安全网**：恢复前先自动落一份 `before-restore` 备份，再做整库在线覆盖（`sqlite3_backup`），最后失效所有缓存。
- **恢复前显式校验**：对被选文件先以只读方式打开并读 `PRAGMA schema_version`，非 SQLite / 已损坏直接报「所选文件不是有效的数据库备份（已损坏或非 SQLite 文件）」，把原先依赖事务语义的**隐式安全**变成**显式可读**。
- **路径穿越防护**：`resolve_backup_file` 五重校验（空 / 含 `/` `\` `..` / 非 `.db` / 父目录不等于备份目录 / 非普通文件）。

## 二、变更清单（文件级）

| 文件 | 变更 | 说明 |
|---|---|---|
| `src-tauri/src/cloud.rs` | **新增 1934 行** | 云通道传输抽象 + 四实现 + 推送/拉取编排 |
| `src-tauri/src/sync.rs` | **+1650 行** | 快照内核、LWW 回放、冲突记录/裁决、自然键相撞前置检测 |
| `src-tauri/src/backup.rs` | **新增 307 行** | 整库滚动备份 + 保留策略 |
| `src-tauri/src/commands.rs` | +1066 行 | 同步/云/备份命令面 + `RestoreBackupOut` + `resolve_backup_file` |
| `src-tauri/src/lib.rs` | +25 行 | 模块声明 + `generate_handler!` 注册 |
| `src-tauri/permissions/fundlens.toml` | +20 行 | `fl-sync-cloud` / `fl-sync-conflict` / `fl-sync-backup` 权限组 |
| `src-tauri/capabilities/default.json` | 6 行 | 能力挂载 |
| `src-tauri/gen/schemas/*` | 24×2 + 2×2 行 | 声明式 ACL 产物（构建期重生成，随源码提交） |
| `src/pages/SyncPage.tsx` | **新增 1050 行** | 同步页：状态卡 / 云端区块 / 备份区块 / 冲突列表 |
| `src/pages/SyncPage.test.tsx` | **新增 540 行** | 20 个前端单测 |
| `src/api.ts` | +341 行 | 17 个同步命令封装 + 非 Tauri mock 兜底 |
| `src/App.tsx` | 5 行 | 路由挂载 |
| `relay/server.js` | **新增 266 行** | 自定 relay 协议参考实现（仅用于本地端到端验证） |
| `cloudbase/migrations/20260910131035_fl_sync_store.sql` | **新增** | `fl_sync` 建表 |
| 设计文档 | 新增 3 份 | `sync-m2-cloudbase-design` / `sync-m3-conflict-ui-design` / `perf-cloudbase-v2.6.0-design` |

合计 **26 个文件、+7780 / −54 行**。

## 三、质量验证（本次发版实测）

| 门禁 | 命令 | 结果 |
|---|---|---|
| 前端类型检查 | `npx tsc -b` | ✅ 通过 |
| 前端单测 | `npx vitest run` | ✅ **68 passed / 9 files**（4.17s） |
| Rust 单测 | `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features` | ✅ **207 passed / 0 failed / 6 ignored**（7.49s） |
| 版本升位 | 五处回读 | ✅ 全部 `2.6.1` |
| 改动范围 | `git diff --stat` | ✅ 恰好 5 文件 / 6 行，无越界 |
| 包内命令 | ACL 清单 `acl-manifests.json` | ✅ 17 个 `sync_*` 命令全在 |

**包内命令权威清单（本次构建产物实际内容）**：
`sync_cloud_check`、`sync_cloud_config_get`、`sync_cloud_config_set`、`sync_cloud_pull`、`sync_cloud_push`、`sync_conflict_detail`、`sync_conflict_resolve`、`sync_conflicts_resolve_all`、`sync_create_backup`、`sync_delete_backup`、`sync_export_snapshot`、`sync_import_snapshot`、`sync_list_backups`、`sync_list_conflicts`、`sync_restore_backup`、`sync_set_backup_keep`、`sync_status`

**部署核验**：
- `CFBundleShortVersionString` = `2.6.1` ✅
- 构建产物二进制 mtime `Sep 11 01:05:21` ≤ 部署二进制 mtime `Sep 11 01:05:33` ✅（确认落地）
- 启动冒烟：`open -a` 后 6 秒 `pgrep` 得到 `pid=33810` ✅，随后已关闭

**已知未覆盖（显式说明）**：
- 本轮**未**做真实数据库副本上的端到端同步演练（跨设备冲突→裁决→再传播的完整链路此前已在独立验证轮次覆盖 14 项断言，本次发版未重跑）。
- `MODE_PG` 的线上连通性未在本次发版中复测（依赖 CloudBase API Key，凭据在仓库外 `~/.workbuddy/fundlens-cloudbase-apikey.txt`）。
- 麒麟 aarch64 分支**未**同步本次变更（见「下一步」）。

## 四、交付物状态

| 平台 | 产物 | 状态 |
|---|---|---|
| macOS | `/Applications/FundLens.app` v2.6.1 | ✅ 已部署并冒烟通过 |
| macOS（构建缓存） | `src-tauri/target/release/bundle/macos/FundLens.app` | ✅ |
| Android | APK | ⏸ 未打包（按要求以用户明示为准） |
| 麒麟 aarch64 | deb / AppImage | ⏸ 未同步（需 Docker `fl-build` + 分支合并） |

## 五、使用说明

1. 打开「数据同步」页，顶部状态条显示当前设备与同步水位。
2. **本地目录模式**：选一个两端都能访问的目录（如 iCloud/网盘同步目录），一端「导出快照」，另一端「导入快照」。
3. **CloudBase 模式**：模式选 `CloudBase（PostgreSQL 直连）`，基址填 REST 基址 `https://sss-d3ggl1sft593f46c7.api.tcloudbasegateway.com/v1/rdb/rest`，令牌填 service_role API Key，点检测 → 推送/拉取。
4. 出现冲突时在冲突列表点开，看逐字段差异后选「保留本地」或「采用远端」。
5. 若提示该行自然键相撞（`blocked_reason`），需先在持仓页合并重复记录，再回来裁决。
6. 备份区可「立即备份」、调整保留份数、恢复或删除某一份；恢复前会自动再落一份 `恢复前自动` 备份。

## 六、已知限制

- CloudBase PG 通道**仅传文本载荷**（快照 JSONL），二进制附件不走此通道。
- PG 直连使用 service_role API Key，等同于服务端密钥，**必须**存放于仓库外，不可入库。
- 自然键相撞采用「记冲突 + 阻断采用远端」策略，**不做自动合并**，需人工裁决。
- 冲突记录按 `(table, row_key, device)` 去重，但**不同来源设备各自保留**一条。

## 七、回滚方案

```bash
# 1) 回退本地源码
git -C /Users/sheng/WorkBuddy/2026-08-13-00-26-44/fundlens revert --no-commit 24e5612
# 2) 如需回到旧客户端：重新构建 d4eed72 后覆盖部署
git -C <PROJ> checkout d4eed72 && zsh <PROJ>/fl-build-desktop.sh
rm -rf /Applications/FundLens.app && cp -R <PROJ>/src-tauri/target/release/bundle/macos/FundLens.app /Applications/
```
注意：v2.6.1 引入了新的同步表与 ACL，回退到 v2.6.0 **不影响本地持仓数据**（同步层只读本地表 + 独立 `fl_sync`/`sync_conflicts` 表），回退后同步数据会留在库中不参与计算。

## 八、下一步

1. **麒麟分支同步**：`feat/kylin-v10-aarch64` 合并 main 至本提交（三处固定冲突处理：`capabilities/` + `permissions/` 取删、`tauri.conf.json` 保留 v1 schema + 版本升位），随后走 Docker `fl-build` 出 deb/AppImage。
2. **Android**：如需 APK，走 `fl-build-android.sh`（keystore 脚本保持未跟踪）。
3. **M3 冲突 UI 收敛**：`fl-sync-conflict` 权限组下的裁决入口在同步页与持仓页尚无二级联动。
4. **真实库副本端到端复测**：建议在正式对外前，用 `FUNDLENS_DATA_DIR` 指向真实库副本，跑一遍「导出 → 另一设备导入 → 制造自然键冲突 → 裁决 → 再传播」全链路。
