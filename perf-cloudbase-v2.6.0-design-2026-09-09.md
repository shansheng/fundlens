# FundLens v2.6.0 设计方案：总览/报表性能优化 + CloudBase 多设备同步

- 日期：2026-09-09
- 状态：待评审（性能部分推荐直接开工）
- 范围：P-A/P-B/P-C 本地性能（本次实现）；Phase-2 CloudBase 同步（架构定稿，另排期）
- 依据：用户反馈「总览/持仓页刷新转圈」「周报月报卡」；真实库副本实测取证

---

## 0. 取证结论（已实测，勿回退）

| 观察 | 证据 | 结论 |
|---|---|---|
| SQLite 查询本身不慢 | positions 198 / funds 355 / disclosures 2622 / nav_history 68k / snapshots 仅 4；全表统计 51ms、disclosures 全扫 <1ms | 不是 SQL 瓶颈 |
| 报表页并发风暴 | `ReportsPage.loadAll` Promise.all 并发 6 命令；`build_period_report`(commands.rs:2963) 内部对**每个**周期再整跑一次 `get_overview` | 打开报表页 ≈ 同刻 5~6 份「839 符号行情网络批抓取 + ~200 持仓逐只 value_fund 重算」 |
| 总览每次刷新全量重做 | `get_overview`：全量 disclosures 扫描 → Rust 筛最新期 → 指数/个股分批行情抓取 → 逐持仓重算；est_cache 只写不读 | 无快照复用，高频刷新（含 UI 定时器/切页）重复付网络 RTT |
| 写阻塞读风险 | `with_conn` 全局单连接 + 互斥串行；busy_timeout=0 | 行情/净值批量写期间 UI 读可能撞锁报错 |
| nav_history 索引缺口 | 68k 行仅 `idx_nav_history_date`，无 (fund_code, nav_date) | prev_nav_from_history 逐基金扫描 |

---

## 1. P-A：总览/明细短 TTL 快照缓存（治「转圈」根因）

### 1.1 设计
进程内缓存（非 DB），单文件 `src-tauri/src/commands.rs` 内新增模块级静态（或独立 `cache.rs`）：

```
static OVERVIEW_CACHE: Mutex<Option<(Instant, String /*key*/, OverviewOut)>>
static OVERVIEW_COMPUTE: Mutex<()>        // single-flight：并发首miss时只算一次
TTL: 交易时段 10s；盘前/盘后/休市 60s（用 data::market_phase() 区分）
```

- `get_overview(platform)` 命令体改为走 `cached_overview(platform)`：先查缓存（key=platform、未过期）→ 命中直接 clone 返回；未命中则持 COMPUTE 锁后**二次检查**（single-flight 防并发 6 份同时算），计算后写缓存再返回。
- `build_period_report` 的 best/worst（:2963）改为复用同一 `cached_overview(None)`——报表与总览页数值**恒一致**，且不再产生独立的行情风暴。
- `get_fund_detail` 单只路径：同样接短 TTL（key=code），保证与总览同窗口口径一致（延续 001551 一致性原则）；detail 与 overview 缓存各自独立。
- **失效**：任何改持仓/交易/基金/净值/快照的写命令成功后调 `invalidate_overview_cache()`（置 None）。给出清单式接入点：`grep '#[tauri::command]' commands.rs` 中调用了 db 层 positions/transactions/funds/nav_history/snapshots 增删改的函数，逐个在成功返回前失效。
- `record_daily_snapshot` 的「当日首查落盘」副作用保持原语义（幂等按日），不因缓存改变记账行为；缓存只拦「同窗口重复计算」。

### 1.2 为什么不用 est_cache
est_cache 只写不读是历史遗留，但它跨会话持久化，交易时段接读有新鲜度风险。本次用**进程内短 TTL**（内存、自动过期、写即失效），不动估值口径、不动表结构，风险最小、收益最大。est_cache 读路径列入后续（见 §4）。

### 1.3 验收
- 单测：缓存逻辑抽可测 seam（如 `cached_overview_with(compute, ttl, key)`），断言同 key 窗口内 compute 只执行一次、过期后重算、写命令后失效重算。
- 回归：`cargo test --lib --no-default-features` 全绿；临时 #[ignore] 测试对真实库副本连续两次 `get_overview`，第二次耗时应 <5ms 且数值与首查一致。

---

## 2. P-B：报表页 6 并发命令收敛（治「周报/月报」卡）

根因 = 每报表内嵌 get_overview；P-A 落地后 6 个并发命令在 single-flight 下**共享一次**行情抓取与计算，报表端无需改前端即可消除风暴。
前端不改动（loadAll 的 Promise.all 保留，命中缓存后开销趋近于 0）。可选后续：日/周/月/年四报表同窗口结果做前端缓存，切 tab 不再发命令——P-A 已覆盖，暂不做。

---

## 3. P-C：DB 加固（低风险，随 P-A 一起）

1. `init_db` 幂等补索引：
   - `CREATE INDEX IF NOT EXISTS idx_nav_history_fund_date ON nav_history(fund_code, nav_date)`（68k 行 prev-nav 回退查询从逐基金全扫→索引定位）
2. `init_db` 单连接 PRAGMA 补齐（仅影响本进程连接，落库文件不受影响）：
   - `cache_size = -65536`（64MB 页缓存，现 2000 页=2MB 偏小）
   - `mmap_size = 268435456`（256MB 只读 mmap）
   - `busy_timeout = 5000`（写批期间读等待 5s 而非立刻 SQLITE_BUSY）
3. 不引入连接池/多连接（保持 with_conn 单连接串行语义，避免 SQLITE_BUSY 面扩大）。

---

## 4. Phase-2：CloudBase 多设备同步（架构定稿，加密取消）

> 用户裁定：数据无保密必要 → **取消客户端加密层**；CloudBase 侧仍保持账号私有权限（私有桶/私有云库 ACL），Tencent 侧默认权限即隐私边界。

### 4.1 定位
- 本地 SQLite 仍是**唯一事实源**，离线优先、App 完全不依赖云可运行。
- CloudBase = 增量同步中枢 + 自动备份：`~/.workbuddy/skills` 已有 cloudbase 连接器（对象存储 + 云函数 + 云数据库），可复用。

### 4.2 架构（设备 A↔CloudBase↔设备 B）
```
本地 SQLite ──(1)写操作→ sync_log(本地表: 表名,row_id,op,updated_at)
                     │ (2)按 watermark 增量导出 changeset(JSONL)
                     ▼
             CloudBase 云存储私有桶 /sync/{device}/{yyyy-MM-dd-HHmmss}.jsonl
                     │ (3)各设备拉取他设备 changeset
                     ▼
(4)按表回放：行级 last-write-wins（updated_at）+ 删除 tombstone（deleted_at 标记）
                     │ (5)自动备份：整库 .backup 产物上传 /backup/fundlens-<date>.db
```
- 关键表：positions / funds / transactions / snapshots / position_daily / settings / grid_*（用户态数据全量参与）；`nav_history`、`disclosures`、`quotes_cache`、`est_cache`、`stock_*` 等**派生/缓存数据不参与**（各设备自行从官方源重拉，天然一致，避免同步风暴）。
- 变更跟踪：本地加 `sync_log` + 每业务表补 `updated_at` 列（migration），避免全表 diff；首启做一次性全量 baseline 上传。
- 冲突：同表同行异地修改 → LWW（updated_at 大者胜），冲突行单独落 `sync_conflicts` 表并在 UI 提示。
- 导入幂等：replay 按 (表,row_id) upsert + tombstone，天然可重放。

### 4.3 里程碑（另排期，不在本次）
- M1 同步内核：sync_log/updated_at migration + changeset 导出导入纯函数 + 单测（无云可测）
- M2 CloudBase 通道：私有桶上传/下载 + watermark 推进 + 云函数拉取清单
- M3 UI：手动「立即同步」+ 自动（启动/网络恢复）+ 冲突提示；麒麟/Android 共用
- M4 自动备份：写库前整库 backup 上传，保留 N 份

---

## 5. 开发计划（本次实现范围 = P-A + P-B + P-C）

| # | 任务 | 产出 |
|---|---|---|
| 1 | 建分支 feat/perf-v2.6.0 | 自 main（7de5fdd） |
| 2 | P-C：db.rs 索引 + PRAGMA | init_db 幂等补丁 |
| 3 | P-A：commands.rs 缓存模块 + single-flight + 失效接入 + detail TTL | cached_overview / invalidate / 写命令失效点清单 |
| 4 | P-B：build_period_report 复用缓存 | 报表不再触发行情网络 |
| 5 | 单测（缓存 seam + 失效）+ 全量回归 | cargo test --no-default-features / tsc / vitest |
| 6 | 独立验证（新 worker） | 语义核对 + 真库副本临时 #[ignore] 计时断言 |
| 7 | 合 main + 发版记录（v2.6.0 走既有流程） | commit hash / 桌面部署按需 |

### 风险与边界
- 缓存数值一致性：总览↔明细同窗口同快照（利大于弊，符合 001551 一致性诉求）；写命令漏失效是最大风险 → 失效点清单由实现者 grep 枚举并在单测覆盖主链路（录交易/改持仓/导入/记快照）。
- 不动估值口径、不动持仓模型、不改 DB 文件格式（P-C 仅加索引与连接 PRAGMA，均可逆）。
- Phase-2 CloudBase 不加密属用户明示；本阶段不含任何云代码。
