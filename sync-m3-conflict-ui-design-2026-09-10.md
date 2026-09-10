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

## 5. 唯一键冲突：从「已知风险」到「已修复」（含一次假阴性回归）

`positions` 的跨设备同步主键是自增 `id`，但它的**业务身份**其实是
`(account_id, fund_code, platform)`（唯一索引 `uq_positions_account_fund_platform`，`db.rs:480`）。

两台设备各自新建「逻辑上同一条」持仓 → **id 不同、自然键相同**。此时：

- **M3「采用远端」**（已处理）：按 id 找不到本地行 → 走 INSERT → 撞唯一索引。
  给出可操作的说明并**保持冲突未解**，不自动合并——合并两条持仓是会影响用户资产的
  语义决策，必须由用户确认，且不留下半截数据。
- **常规拉取路径**（`apply_one_upsert` 的 `INSERT OR REPLACE`，M1 内核）：在真实库副本上实测
  （`PRAGMA foreign_keys=1`），REPLACE 会**静默删掉本地旧持仓行**，并通过
  `position_daily.position_id REFERENCES positions(id) ON DELETE CASCADE`（`db.rs:117`）
  **级联抹掉该持仓的 position_daily 历史**，然后插入新行——全程无报错。

### 5.1 决策：选 A

两条路——(A) 把「无 PK 命中但撞自然键」的回放改成记冲突（复用本页 UI）；(B) 把 `positions`
的同步身份从 `id` 改成自然键。(B) 会动 M1 内核的跨设备身份定义、影响面大；(A) 只改回放分支、
复用已有裁决通道。**取 A**（commit `456b959`）。

实现要点：

- `unique_index_columns`——读 `PRAGMA index_list` / `index_info` 拿唯一索引列组合，
  跳过 partial 索引（语义不完整）与表达式索引（`index_info.name` 为 NULL）。
- `natural_key_collision`——载荷主键在本地不存在、但其自然键命中**另一条**本地行时，
  返回被撞行主键。**这是「REPLACE 会删哪一行」的精确复刻**，不是启发式。
- 命中即 `record_conflict(...) + continue`，不再落 REPLACE。
- `ConflictDetail.blocked_reason`——同类相撞时**前置**告诉 UI「采用远端会覆盖并删除本地那一条，
  请先合并重复记录」，按钮直接禁用，而不是让用户点完再吃一个 UNIQUE 报错。

### 5.2 独立验证发现的假阴性（已修）

独立验证者在真实库副本上构造场景，命题 1-6、8 通过，但**命题 7 不通过**：

> 载荷**省略**某个自然键列时，早期实现的 `if !cols.iter().all(|c| map.contains_key(c)) { continue; }`
> 会**跳过整个索引**→ 返回 None → 不记冲突 → 仍走 REPLACE。复现：本地
> `(account_id=3, fund_code='MIS', platform='')` + 3 条子记录；远端载荷同自然键、不同 id、
> **不含 `platform`** → `applied=1, conflicts=0`，本地行与 3 条子记录**全部消失**。

根因：`INSERT OR REPLACE` 只绑载荷带的列，其余列取**列默认值**。载荷缺 `platform` 时新行以
默认 `''` 落库，照样撞唯一索引。原实现因为「列不全」而放弃比对，等于把最危险的情况放行了。

修正：`column_effective_defaults`——`PRAGMA table_info` 的 `dflt_value` 是默认值**表达式原文**
（实测 `''` / `0` / `datetime('now')`），交给 SQLite 自己求值（`SELECT <expr>`）即得与真实 INSERT
完全一致的结果，无需解析 SQL 字面量。把载荷缺的列补成该默认值后再比对，假阴性消失，
且不引入假阳性。

补充：**触发面比看上去广**——跨版本同步时，旧版本设备（或其快照文件）本就不含新版本才加入的列，
缺列是正常现象；而快照文件是用户可见、可手工编辑的。故这不是纯理论缺口。

### 5.3 顺带处理：重复冲突去重（命题 8）

`sync_conflicts` 无 `(tbl, row_key)` 唯一约束，同一变更重复回放（水位未推进、手工重复导入）
会堆出多条一样的待裁决项。`record_conflict` 现在先删同 `(tbl, row_key, device)` 的**未解**行再插，
只保留最新一次远端意图（LWW 下更旧的按定义不是胜者，本地行另行保留，无损）。来源设备不同的
冲突各自保留。

### 5.4 数据不丢的论证（调用方视角）

两处调用（`cloud.rs:452` 云拉取、`commands.rs:2007` 文件导入）都在事务内执行 `apply_changeset_lww`，
`record_conflict` 的 INSERT 随 `COMMIT` 一并提交；水位（`mark_seen` / `META_LAST_IMPORT`）在 COMMIT
**之后**且只写元信息，不触碰 `sync_conflicts`。故被拒载荷安全留存于 `sync_conflicts.payload`，
「记冲突 + 推进水位」不会丢数据。若回放返回 Err 则整体 ROLLBACK、水位不推进，下次可重放。

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
| `colliding_upsert_records_conflict_and_preserves_local_row` | 选 A 核心：撞自然键 → 记冲突，本地行不被静默删改 |
| `self_update_is_not_flagged_as_collision` | 按自身主键更新不得误判（假阳性守卫） |
| `brand_new_row_is_applied_not_flagged` | 全新记录正常插入、不记冲突（假阳性守卫） |
| `collision_is_detected_even_when_payload_omits_a_natural_key_column` | §5.2 假阴性修复：载荷缺 `platform`（默认 `''`）仍必须检出；带上且不同则不得误报 |
| `colliding_upsert_with_omitted_key_column_preserves_local_row_and_children` | 端到端：缺列相撞时本地行 + 3 条 `position_daily` 子记录零丢失 |
| `repeated_rejected_change_keeps_one_unresolved_conflict_with_latest_payload` | §5.3 去重：重复回放只留 1 条未解冲突且保留最新远端意图 |

### 6.2 真实库端到端（独立 worker，真实库副本）

用真实库副本（positions=198 / funds=355）跑完整命令层，6 项断言全过：
本地覆盖与传播、损坏载荷不删数据、批量裁决计数、错误口径。**唯一键冲突项由该验证发现**，
已按 §5 处理。

### 6.3 前端

`npx tsc -b` 干净；`npx vitest run` 60 passed（SyncPage 12 条，含差异渲染、空值占位、
保留本地不二次确认、采用远端必须确认、corrupt 禁用采用远端、批量裁决与失败计数、无冲突时不显示批量入口）。
