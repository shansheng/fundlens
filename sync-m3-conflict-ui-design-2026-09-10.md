# FundLens M3 — 多设备冲突「对比与裁决」设计记录

- 日期：2026-09-10
- 分支：`main`
- 状态：✅ 已完成并验证（Rust 197 passed / 0 failed / 6 ignored；前端 tsc 干净 + 60 passed）
- 关联：`sync-m2-cloudbase-design-2026-09-10.md`（M2 云通道）、`sync.rs`（M1 同步内核）

---

## 1. 问题

M1 内核做按行 LWW 合并：远端变更若比本地行的 `updated_at` **更旧**，就不覆盖本地，转而往
`sync_conflicts` 记一条（`tbl / row_key / device / payload / resolved`）。

M2 把「云端推送/拉取」打通后，冲突开始真实产生，但 UI 只能列出一张**只读表**：
用户看得到「哪张表、哪条主键、来自哪台设备」，却看不到**到底哪个字段不一样**，
也无从裁决——冲突只会越积越多。

M3 就是把这块从「只读列表」补成「**可对比、可裁决**」。

---

## 2. 语义契约（M3 的核心，改动前先读）

| 远端载荷 | `op` | 含义 |
|---|---|---|
| 非空 JSON 对象 | `upsert` | 远端要**修改**这行 |
| 空串 | `delete` | 远端要**删除**这行 |
| 无法解析 | `corrupt` | 载荷损坏，**不得**当成删除 |

> ⚠️ 「空」与「解析失败」必须分开处理。若把损坏载荷当空串，裁决时就会**误删本地行**。
> 这是 M3 实现里专门防的一条。

裁决两个方向：

- **保留本地**（`choice = "local"`）：丢弃远端那一版，只把 `resolved` 置 1，**不动任何数据**。
- **采用远端**（`choice = "remote"`）：把远端内容写回本地，并作为**新版本**传播出去。

### 2.1 「采用远端」为什么不能用 `INSERT OR REPLACE`

写回时**刻意剔除载荷里的 `updated_at`**，交由触发器盖新戳。原因：

`au` 触发器的守卫是 `WHEN OLD.updated_at = NEW.updated_at`。若把远端那个旧时间戳原样写进去，
触发器会认为「本次 UPDATE 没改 updated_at」→ **条件不成立 → 不记 sync_log**。后果有两个：

1. 本次裁决**传不出去**，别的设备永远不知道用户已经做了决定；
2. 本地行继续带着旧时间戳，下次同步依然输给对端，**冲突反复出现**。

所以写回用 `UPDATE`（行存在）/ `INSERT`（行不存在），而不是 `INSERT OR REPLACE`：

- 剔除 `updated_at` → 触发器正常记账 + 盖新戳；
- 不用 REPLACE 也避免了它的「先删后插」语义（见 §5 的已知风险）。

### 2.2 载荷不可信

载荷来自其它设备的快照，**键不可信**。写回时以「表自身的合法列」（`PRAGMA table_info`）为基准
去载荷里取值，而不是拿载荷的键去拼 SQL —— 反向过滤让未知列天然进不了语句。

---

## 3. 实现

### 3.1 后端 `src-tauri/src/sync.rs`

| 符号 | 作用 |
|---|---|
| `table_label(tbl)` | 13 张参与同步表的中文标签（放后端做单一事实源，列表与详情共用） |
| `ConflictField` / `ConflictDetail` | 字段级差异与详情；`op` / `localExists` / `identical` / `payloadError` |
| `conflict_detail(conn, id)` | 本地当前行 vs 远端被拒变更，逐字段比对 |
| `force_apply_remote` | 采用远端：UPDATE 或 INSERT，返回 `Ok(1)` = 写回一行 |
| `describe_writeback_error` | 把唯一键冲突翻译成用户能照做的说明 |
| `resolve_conflict(conn, id, adopt_remote)` | 裁决一条，返回 `(是否找到, 写回行数)` |
| `resolve_all_conflicts(conn, adopt_remote)` | 批量，返回 `(已解, 写回, 失败)`，**单条坏数据不中断整批** |
| `user_error` | 把面向用户的说明包成 rusqlite 错误（见 §4） |

差异表**排除主键列**（单独展示）与 `updated_at`（同步内部戳，两边必然不同，列进去只是噪音）。

`identical` 的口径（采用远端与保留本地结果相同）：

- `delete`：本地已无该行；
- `upsert`：本地**有**该行且各字段一致；
- `corrupt`：一律 false（无从判断，让用户看到操作入口）。

> 注意「本地已无该行 + 远端要改」的情形：采用远端会把行**插回来**，这是实质变化，不能判成「无差异」。

### 3.2 命令与 ACL（三步，缺一不可）

`sync_conflict_detail` / `sync_conflict_resolve` / `sync_conflicts_resolve_all`

1. `commands.rs` 写 `#[tauri::command]`；
2. `lib.rs` `generate_handler!` 注册；
3. `permissions/fundlens.toml` 新增权限组 **`fl-sync-conflict`** + `capabilities/default.json` 引用。

生成物 `gen/schemas/*` 已随构建重跑并入库——已核验 `acl-manifests.json` 含 `fl-sync-conflict`
及三个命令名（避免运行时「命令不可用」）。

### 3.3 前端 `src/pages/SyncPage.tsx`

冲突区从「表格」改成「可展开列表」：

- 行头：表标签 · 主键（`rowKey` 是 JSON 数组文本，解析后展示）· 未解/已解 · 来源设备 · 时间；
- 展开：按需拉取差异（列表**不携带 payload**，避免一次拉 200 条大字段）；
  差异表三列「字段 / 本地（当前保留）/ 远端（被拒）」，`null` 与空串分别显示 `—` 与 `(空)`；
- 操作：`保留本地` / `采用远端`（后者**二次确认**，因为会改写数据）；
- 批量：`全部保留本地` / `全部采用远端`（带未解条数确认）；
- `op = corrupt` 时禁用「采用远端」并说明原因。

---

## 4. 一处实现细节：错误信息不要带内部前缀

`db::with_conn` 的闭包必须返回 `SqlResult`，所以面向用户的说明要包成一个 rusqlite 错误。
**不要用 `InvalidParameterName`** —— 它的 Display 会给用户看到 `Invalid parameter name: ...`。
改用 `SqliteFailure(_, Some(msg))`，其 Display 恰好就是 msg 本身，与 db.rs「数据库未初始化」的既有约定一致。

---

## 5. 已知风险：唯一键冲突（本次验证发现，**尚未修复**）

`positions` 的跨设备同步主键是自增 `id`，但它的**业务身份**其实是
`(account_id, fund_code, platform)`（唯一索引 `uq_positions_account_fund_platform`，`db.rs:480`）。

两台设备各自新建「逻辑上同一条」持仓 → **id 不同、自然键相同**。此时：

- **M3「采用远端」**（本次已处理）：按 id 找不到本地行 → 走 INSERT → 撞唯一索引。
  现在会给出可操作的说明并**保持冲突未解**，不自动合并——合并两条持仓是会影响用户资产的
  语义决策，必须由用户确认，且不留下半截数据。
- **常规拉取路径**（`apply_one_upsert` 的 `INSERT OR REPLACE`，M1 内核，**本次未改**）：
  在真实库副本上实测（`PRAGMA foreign_keys=1`），REPLACE 会**静默删掉本地旧持仓行**，
  并通过 `position_daily.position_id REFERENCES positions(id) ON DELETE CASCADE`
  （`db.rs:117`）**级联抹掉该持仓的 position_daily 历史**，然后插入新行——全程无报错。

  当前真实库 `position_daily` 为 0 行，所以暂时无数据可丢；但该表参与同步、会被填充，
  风险是真实存在的。

**待定决策**：是否把「无 PK 命中但撞自然键」的回放改成记冲突（复用本页 UI）而不是 REPLACE；
或把 positions 的同步身份从 `id` 改为自然键。两者都会改变同步语义，需用户确认后再动。

---

## 6. 验证

### 6.1 单元测试（`sync.rs`，最小 schema）

| 测试 | 覆盖 |
|---|---|
| `conflict_detail_lists_only_changed_fields` | 只列真正不同的字段；主键与 updated_at 不列；不存在的 id → None |
| `resolve_keep_local_leaves_data_untouched` | 保留本地不动数据、只清标记 |
| `resolve_adopt_remote_overwrites_row_and_logs_new_version` | 覆盖本地 + 记为新版本（sync_log +1）+ **不写回旧时间戳** |
| `empty_payload_means_remote_delete_intent` | 空载荷 = 删行意图；采用即删 |
| `corrupt_payload_errors_and_never_deletes_row` | 损坏载荷报错、不删行、保持未解 |
| `resolve_all_adopt_remote_skips_corrupt_entries` | 批量 `(resolved, applied, failed)`，坏数据不中断整批 |
| `every_synced_table_has_a_label` | 13 表标签无遗漏 |
| `upsert_conflict_without_local_row_is_not_identical` | 本地无该行 + 远端改行 → 非「无差异」，采用会插回 |
| `upsert_conflict_with_identical_content_is_flagged_identical` | 内容全同 → 标记无差异 |
| `adopt_remote_unique_collision_gives_actionable_error` | 唯一键冲突给出可操作说明、无副作用、保持未解 |

### 6.2 真实库端到端（独立 worker，真实库副本）

用真实库副本（positions=198 / funds=355）跑完整命令层，6 项断言全过：
本地覆盖与传播、损坏载荷不删数据、批量裁决计数、错误口径。**唯一键冲突项由该验证发现**，
已按 §5 处理。

### 6.3 前端

`npx tsc -b` 干净；`npx vitest run` 60 passed（SyncPage 12 条，含差异渲染、空值占位、
保留本地不二次确认、采用远端必须确认、corrupt 禁用采用远端、批量裁决与失败计数、无冲突时不显示批量入口）。
