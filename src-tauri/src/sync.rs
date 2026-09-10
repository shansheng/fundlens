// FundLens · Phase-2 CloudBase 同步 M1 内核（纯本地、无云、无 UI、无新命令）。
//
// 设计要点（详见仓库根 perf-cloudbase-v2.6.0-design-2026-09-09.md §4）：
// - 本地 SQLite 仍是唯一事实源；变更由 db.rs 中的 SQLite 触发器在 DB 层自动跟踪，
//   业务写代码零侵入（见 db::init_sync_schema）。
// - 本模块是「纯函数内核」：所有函数都接收 &Connection，不依赖 Tauri / 全局 DB 单例，
//   因此可在内存库上无副作用单测，未来也可在云拉取/回放路径直接复用。
// - 表名一律走白名单（SYNCED_TABLES），列名一律来自行 json 自身而非外部输入，杜绝 SQL 注入。

use rusqlite::types::Value as RusqliteValue;
use rusqlite::{params_from_iter, Connection, Result as SqlResult};
use serde_json::Value;

/// 参与同步的用户态表（白名单）。派生/缓存表（nav_history、disclosures、quotes_cache、
/// est_cache、stock_profile、stock_style、index_constituent、ocr_jobs、quote_jobs、
/// import_sessions、trading_calendar、migrations、sync_* 等）不在此列——各设备自行从官方源重拉。
///
/// 这是「参与同步的表」的唯一事实源，db::init_sync_schema 也引用它。
pub const SYNCED_TABLES: &[&str] = &[
    "positions",
    "funds",
    "transactions",
    "snapshots",
    "position_daily",
    "settings",
    "grid_funds",
    "grid_signal",
    "grid_signal_history",
    "grid_pending_rebuy",
    "grid_settings",
    "accounts",
    "platform_templates",
];

fn is_synced_table(t: &str) -> bool {
    SYNCED_TABLES.contains(&t)
}

/// 快照/基线的**导出与回放顺序**：父表先于子表（外键安全）。
///
/// db.rs 的真实外键链（决定顺序，改动前先核对 DDL）：
///   funds(code) ← positions.fund_code / snapshots.fund_code / transactions.fund_code
///   positions(id) ← position_daily.position_id
///   transactions(id) ← transactions.related_tx_id（自引用，配对流水）
/// 因此 funds 必须最先、positions 先于 transactions/position_daily。其余表无外键依赖，成组置于中段。
/// 注意：M3 导入侧仍会在事务内开启 `PRAGMA defer_foreign_keys`（见 commands.rs）以兜底「被 LWW 跳过的父行」
/// 等极端情形；本顺序是双保险，也让导出文件本身在外部工具中可顺序重放。
pub const SNAPSHOT_TABLE_ORDER: &[&str] = &[
    "funds",
    "accounts",
    "platform_templates",
    "settings",
    "grid_settings",
    "grid_funds",
    "positions",
    "transactions",
    "snapshots",
    "position_daily",
    "grid_signal",
    "grid_signal_history",
    "grid_pending_rebuy",
];

/// 参与同步表 → 业务主键列清单（表驱动，单一事实源）。
/// D2：跨设备稳定身份必须走业务主键，而非内部 rowid。各表主键逐一核对（取自 db.rs 建表 DDL）：
/// - positions / transactions / snapshots / grid_signal / grid_signal_history / grid_pending_rebuy /
///   accounts / platform_templates：自增整型主键 `id`
/// - funds：`code`；settings：`key`；grid_funds：`fund_code`；grid_settings：`k`（均为 TEXT 主键）
/// - position_daily：复合主键 `(position_id, nav_date)`
///
/// 本表同时被 db.rs（生成触发器记录 json_array(<pk>)）与 sync.rs（DELETE/LWW 按 pk 定位）引用，
/// 保证「记日志」与「按主键回放」口径一致。必须与 SYNCED_TABLES 完全对应（13 张）。
pub const PK_COLUMNS: &[(&str, &[&str])] = &[
    ("positions", &["id"]),
    ("funds", &["code"]),
    ("transactions", &["id"]),
    ("snapshots", &["id"]),
    ("position_daily", &["position_id", "nav_date"]),
    ("settings", &["key"]),
    ("grid_funds", &["fund_code"]),
    ("grid_signal", &["id"]),
    ("grid_signal_history", &["id"]),
    ("grid_pending_rebuy", &["id"]),
    ("grid_settings", &["k"]),
    ("accounts", &["id"]),
    ("platform_templates", &["id"]),
];

/// 取表业务主键列清单；非白名单表返回 None。
pub fn pk_columns(tbl: &str) -> Option<&'static [&'static str]> {
    PK_COLUMNS.iter().find(|(t, _)| *t == tbl).map(|(_, c)| *c)
}

/// 一条变更记录。
/// - `op` = "upsert"（行当前存在）或 "delete"（墓碑，行已被删）。
/// - `ts` = 该变更在源端发生的时刻（来自 sync_log.ts）。LWW 回放（apply_changeset_lww）
///   据此与目标行 updated_at 比较，决定应用或记冲突。注意：设计文档的 Change 仅列
///   {tbl, row_key, op, payload}，此处额外携带 ts 是 LWW 必需的，且 collect_changeset
///   本就从 sync_log 取到该值，不引入额外来源。
/// - `row_key` = 业务主键的 JSON 数组文本（如 `["000001"]`、`["000001","2026-01-01"]`），
///   D2 起取代旧 row_id；sync.rs 端解析后按 pk 列定位目标行（跨设备稳定）。
/// - `payload` = upsert 时该行的当前快照（serde_json::Value::Object）；delete 时为 None。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Change {
    pub tbl: String,
    pub row_key: String,
    pub op: String,
    pub ts: String,
    pub payload: Option<Value>,
}

/// 解析 row_key JSON 数组文本为 Value 数组（pk 值列表）。
fn parse_row_key(s: &str) -> SqlResult<Vec<Value>> {
    serde_json::from_str::<Vec<Value>>(s)
        .map_err(|e| rusqlite::Error::InvalidParameterName(format!("row_key: {e}")))
}

/// 按业务主键读取某行当前快照，序列化为 serde_json::Value::Object。行不存在返回 None。
/// 列名来自 PRAGMA table_info（表自身元信息，非外部输入）→ 安全；WHERE 由 pk 列与绑定参数构成，
/// 列名来自白名单常量 PK_COLUMNS，值来自行自身数据，无外部拼接 → 无注入。
fn select_row_by_pk(conn: &Connection, tbl: &str, pk: &[Value]) -> SqlResult<Option<Value>> {
    let pk_cols = match pk_columns(tbl) {
        Some(c) => c,
        None => return Ok(None),
    };
    if pk.len() != pk_cols.len() {
        return Ok(None);
    }
    let cols = synced_columns(conn, tbl)?;
    if cols.is_empty() {
        return Ok(None);
    }
    let where_clause = pk_cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c}=?{}", i + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let col_list = cols.join(",");
    let sql = format!("SELECT {col_list} FROM {tbl} WHERE {where_clause}");
    let boxes: Vec<Box<dyn rusqlite::ToSql>> = pk.iter().map(json_to_boxed_sql).collect();
    let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params_from_iter(refs.iter().copied()))?;
    match rows.next()? {
        Some(r) => {
            let mut map = serde_json::Map::new();
            for (i, c) in cols.iter().enumerate() {
                let v: RusqliteValue = r.get(i)?;
                map.insert(c.clone(), sql_value_to_json(v));
            }
            Ok(Some(Value::Object(map)))
        }
        None => Ok(None),
    }
}

/// 取表的列名（按定义顺序）。
fn synced_columns(conn: &Connection, tbl: &str) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info('{tbl}')"))?;
    let mut rows = stmt.query([])?;
    let mut cols = Vec::new();
    while let Some(r) = rows.next()? {
        cols.push(r.get::<_, String>(1)?); // pragma_table_info 第 1 列 = name
    }
    Ok(cols)
}

fn sql_value_to_json(v: RusqliteValue) -> Value {
    match v {
        RusqliteValue::Null => Value::Null,
        RusqliteValue::Integer(i) => Value::Number(i.into()),
        RusqliteValue::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        RusqliteValue::Text(s) => Value::String(s),
        // blob 在业务表中不存在；用 UTF-8 lossy 兜底，保证不 panic。
        RusqliteValue::Blob(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
    }
}

/// json 值 → 可绑定参数。列名来自 json 自身（行原有列），绝不拼接外部列名。
fn json_to_boxed_sql(v: &Value) -> Box<dyn rusqlite::ToSql> {
    match v {
        Value::Null => Box::new(Option::<String>::None),
        Value::Bool(b) => Box::new(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Box::new(i)
            } else if let Some(u) = n.as_u64() {
                Box::new(u as i64)
            } else {
                Box::new(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => Box::new(s.clone()),
        // 数组/对象按 TEXT 存储其 JSON 文本（业务列均为标量/JSON 文本，口径一致）。
        Value::Array(a) => Box::new(serde_json::to_string(a).unwrap_or_default()),
        Value::Object(o) => Box::new(serde_json::to_string(o).unwrap_or_default()),
    }
}

/// 幂等回放单条 upsert：按 payload 的列清单 INSERT OR REPLACE。
///
/// D4（列白名单校验）：列名必须属于目标表真实列（PRAGMA table_info 取），杜绝任意列名拼接注入；
/// - 未知列：丢弃该列、其余正常写入（返回 0 = 已应用）。
/// - 主键列缺失：整条跳过（返回 1 = 错误计数），因为无主键无法 INSERT OR REPLACE 定位。
/// - 无任何合法列：整条跳过（返回 1）。
/// 调用方累加返回值得到 (applied, errors)。
fn apply_one_upsert(conn: &Connection, ch: &Change) -> SqlResult<usize> {
    let map = match &ch.payload {
        Some(Value::Object(m)) if !m.is_empty() => m,
        _ => return Ok(0), // 无有效 payload 则不应用
    };
    // 合法列集（来自表自身元信息，非外部输入）
    let valid_cols = synced_columns(conn, &ch.tbl)?;
    let valid_set: std::collections::HashSet<&str> = valid_cols.iter().map(|s| s.as_str()).collect();
    // 主键列必须齐全，否则无法定位 → 整条跳过
    let pks = match pk_columns(&ch.tbl) {
        Some(c) => c,
        None => return Ok(1), // 非白名单表（is_synced_table 已前置拦截，理论不可达）
    };
    for pk in pks {
        if !map.contains_key(*pk) {
            return Ok(1); // 主键缺失 → 错误计数 1
        }
    }
    // 仅保留合法列（未知列丢弃）
    let cols: Vec<&String> = map.keys().filter(|k| valid_set.contains(k.as_str())).collect();
    if cols.is_empty() {
        return Ok(1); // 无任何合法列 → 跳过
    }
    let col_list = cols.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
    let placeholders = vec!["?"; cols.len()].join(",");
    let sql = format!(
        "INSERT OR REPLACE INTO {} ({}) VALUES ({})",
        ch.tbl, col_list, placeholders
    );
    let boxes: Vec<Box<dyn rusqlite::ToSql>> = cols
        .iter()
        .map(|c| json_to_boxed_sql(map.get(*c).unwrap_or(&Value::Null)))
        .collect();
    let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, params_from_iter(refs.iter().copied()))?;
    Ok(0)
}

/// 收集 after_ts/after_id 之后的变更集（按 (ts, id) 复合升序）。
///
/// D3：复合水位 (after_ts, after_id) 解决同毫秒 ts 丢数据——`ts > ?1 OR (ts = ?1 AND id > ?2)`，
/// 排序 `ORDER BY ts, id`。仅以标量 ts 为水位在同毫秒多条变更时会漏掉后续条目。
///
/// - 白名单过滤：sync_log.tbl 不在 SYNCED_TABLES 的一律跳过（防 sync_log 被篡改导致注入）。
/// - op=upsert：按 row_key（业务主键）读取该行当前快照进 payload；若行已被删（如 upsert 后又 delete），
///   则降级为 delete 墓碑（行已不在，无法再取 payload）。
/// - op=delete：保持墓碑，payload=None。
pub fn collect_changeset(conn: &Connection, after_ts: &str, after_id: i64) -> SqlResult<Vec<Change>> {
    let mut stmt = conn.prepare(
        "SELECT id, tbl, row_key, op, ts FROM sync_log \
         WHERE (ts > ?1) OR (ts = ?1 AND id > ?2) ORDER BY ts ASC, id ASC",
    )?;
    let mut log_rows = stmt.query(rusqlite::params![after_ts, after_id])?;
    let mut entries: Vec<(String, String, String, String)> = Vec::new(); // (tbl, row_key, op, ts)
    while let Some(r) = log_rows.next()? {
        let _id: i64 = r.get(0)?; // id 仅用于排序/水位，丢弃
        let tbl: String = r.get(1)?;
        let row_key: String = r.get(2)?;
        let op: String = r.get(3)?;
        let ts: String = r.get(4)?;
        if !is_synced_table(&tbl) {
            continue;
        }
        entries.push((tbl, row_key, op, ts));
    }

    let mut out = Vec::new();
    for (tbl, row_key, op, ts) in entries {
        let pk = match parse_row_key(&row_key) {
            Ok(p) => p,
            Err(_) => continue, // row_key 损坏则跳过该条
        };
        match op.as_str() {
            "upsert" => match select_row_by_pk(conn, &tbl, &pk)? {
                Some(payload) => out.push(Change {
                    tbl,
                    row_key,
                    op: "upsert".into(),
                    ts,
                    payload: Some(payload),
                }),
                None => out.push(Change {
                    tbl,
                    row_key,
                    op: "delete".into(),
                    ts,
                    payload: None,
                }),
            },
            "delete" => out.push(Change {
                tbl,
                row_key,
                op: "delete".into(),
                ts,
                payload: None,
            }),
            _ => {}
        }
    }
    Ok(out)
}

/// 回放期间暂停全部同步触发器的 RAII 守卫。
///
/// D1（P0 回环修复）：旧实现用 `PRAGMA triggers=OFF` 是错药——该 pragma 不存在于 SQLite，
/// 未知 pragma 会被**静默忽略**（已 CLI 实证：置 OFF 后 AFTER INSERT 触发器照常触发），
/// 且 `recursive_triggers` 只拦「触发器体内再点燃其它触发器」，不拦顶层 INSERT/DELETE 点燃本表
/// 触发器。回放若不拦：写 sync_log + ai 回写把源端 updated_at 覆盖成回放时刻 → 双向无限同步。
///
/// 正确做法：**sync_meta 暂停标记**。db.rs 中每个触发器体的副作用（回写 updated_at / 记 sync_log）
/// 都带 `NOT EXISTS(sync_meta.sync_pause='1')` 守卫（见 init_sync_schema）；本守卫负责在回放前
/// 置标记 '1'、结束后（含出错/Drop 兜底）清除，使回放窗口内所有触发器静默。
/// 安全性前提：本应用为全局单连接（with_conn 串行），暂停窗口内无其它写路径，标记法可靠。
struct SyncPauseGuard<'a> {
    conn: &'a Connection,
    active: bool,
}
impl<'a> SyncPauseGuard<'a> {
    fn new(conn: &'a Connection) -> SqlResult<Self> {
        conn.execute(
            "INSERT INTO sync_meta(key, value) VALUES('sync_pause', '1') \
             ON CONFLICT(key) DO UPDATE SET value = '1'",
            [],
        )?;
        Ok(Self { conn, active: true })
    }
    fn done(mut self) -> SqlResult<()> {
        self.active = false;
        self.conn
            .execute("DELETE FROM sync_meta WHERE key = 'sync_pause'", [])?;
        Ok(())
    }
}
impl<'a> Drop for SyncPauseGuard<'a> {
    fn drop(&mut self) {
        if self.active {
            let _ = self
                .conn
                .execute("DELETE FROM sync_meta WHERE key = 'sync_pause'", []);
        }
    }
}

/// 按 row_key（业务主键）删除目标行。pk 列来自白名单常量 PK_COLUMNS，值绑定（非拼接）→ 无注入。
fn delete_by_pk(conn: &Connection, ch: &Change) -> SqlResult<()> {
    let pk = parse_row_key(&ch.row_key)?;
    let pk_cols = pk_columns(&ch.tbl).ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    if pk.len() != pk_cols.len() {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    let where_clause = pk_cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c}=?{}", i + 1))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!("DELETE FROM {} WHERE {}", ch.tbl, where_clause);
    let boxes: Vec<Box<dyn rusqlite::ToSql>> = pk.iter().map(json_to_boxed_sql).collect();
    let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, params_from_iter(refs.iter().copied()))?;
    Ok(())
}

/// 幂等回放变更集（无冲突处理，直接覆盖）。
/// - upsert：按 payload 列清单 INSERT OR REPLACE（D4 列白名单校验）。
/// - delete：按 row_key（业务主键）DELETE（D2，跨设备稳定定位；行不存在则无操作，仍计入 applied）。
///
/// D1：全程 `SyncPauseGuard`（sync_meta 暂停标记，RAII）确保回放不点燃触发器，
/// 不产生 sync_log、不回写 updated_at → 不会与源端形成双向无限同步（回环）。
/// 返回 (applied, errors)：applied = 成功应用的条数；errors = 因主键缺失/无合法列被跳过的条数（D4）。
pub fn apply_changeset(conn: &Connection, changes: &[Change]) -> SqlResult<(usize, usize)> {
    let _guard = SyncPauseGuard::new(conn)?;
    let mut applied = 0usize;
    let mut errors = 0usize;
    for ch in changes {
        if !is_synced_table(&ch.tbl) {
            continue;
        }
        match ch.op.as_str() {
            "upsert" => match apply_one_upsert(conn, ch)? {
                0 => applied += 1,
                e => errors += e,
            },
            "delete" => match delete_by_pk(conn, ch) {
                Ok(()) => applied += 1,
                Err(_) => errors += 1,
            },
            _ => {}
        }
    }
    _guard.done()?;
    Ok((applied, errors))
}

/// LWW（Last-Write-Wins）变体回放。
///
/// 应用前按 row_key（业务主键，D2）比较目标行 updated_at 与变更 ts：
/// - 目标行 updated_at 比变更 ts 新（远端是更旧的变更）→ 跳过，并向 sync_conflicts 记一条
///   （resolved=0, device=来源设备, row_key=主键, payload=变更快照），返回计数 (applied, conflicts)。
/// - 否则正常应用（upsert / delete）。
///
/// D1：全程 `SyncPauseGuard`（sync_meta 暂停标记）守卫，回放不点燃触发器。
/// 注意：本函数只实现单设备对单变更的 LWW 判定；多设备汇合、冲突人工/自动解算在后续阶段处理。
pub fn apply_changeset_lww(
    conn: &Connection,
    changes: &[Change],
    device: &str,
) -> SqlResult<(usize, usize)> {
    let _guard = SyncPauseGuard::new(conn)?;
    let mut applied = 0usize;
    let mut conflicts = 0usize;
    for ch in changes {
        if !is_synced_table(&ch.tbl) {
            continue;
        }
        // 目标当前 updated_at（按业务主键 row_key 定位，D2）
        let target_ts: Option<String> = match parse_row_key(&ch.row_key) {
            Ok(pk) => {
                let pk_cols = match pk_columns(&ch.tbl) {
                    Some(c) => c,
                    None => continue,
                };
                if pk.len() != pk_cols.len() {
                    continue;
                }
                let where_clause = pk_cols
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{c}=?{}", i + 1))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let sql = format!("SELECT updated_at FROM {} WHERE {}", ch.tbl, where_clause);
                let boxes: Vec<Box<dyn rusqlite::ToSql>> = pk.iter().map(json_to_boxed_sql).collect();
                let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
                conn.query_row(&sql, params_from_iter(refs.iter().copied()), |r| r.get(0))
                    .ok()
            }
            Err(_) => continue,
        };
        // ts 为空 = 变更源未携带时间信息（M1 迁移前存量行的快照）→ 不能据此制造冲突，
        // 也不能覆盖对端已有 updated_at 的行：静默跳过（本地已知时间的版本获胜）。
        if ch.ts.is_empty() && matches!(target_ts.as_deref(), Some(t) if !t.is_empty()) {
            continue;
        }
        let target_newer = match target_ts {
            Some(ref t) if !t.is_empty() => *t > ch.ts, // 同格式（YYYY-MM-DD HH:MM:SS.fff）字典序可比
            _ => false,                                 // 目标无行 / updated_at 为空 → 不冲突
        };
        if target_newer {
            let payload_str = ch.payload.as_ref().map(|v| v.to_string()).unwrap_or_default();
            conn.execute(
                "INSERT INTO sync_conflicts(tbl, row_key, device, payload, resolved, created_at) \
                 VALUES(?1, ?2, ?3, ?4, 0, strftime('%Y-%m-%d %H:%M:%f','now'))",
                rusqlite::params![ch.tbl, ch.row_key, device, payload_str],
            )?;
            conflicts += 1;
        } else {
            match ch.op.as_str() {
                "upsert" => match apply_one_upsert(conn, ch)? {
                    0 => applied += 1,
                    _ => {} // 主键缺失等跳过，LWW 不单列 error
                },
                "delete" => {
                    if delete_by_pk(conn, ch).is_ok() {
                        applied += 1;
                    }
                }
                _ => {}
            }
        }
    }
    _guard.done()?;
    Ok((applied, conflicts))
}

// ---------------------------------------------------------------------------
// M3：冲突详情与解算
//
// 背景：LWW 回放遇到「远端变更比本地行更旧」时不覆盖本地，而是往 sync_conflicts 记一条
// （tbl / row_key / device / payload / resolved）。M1 只落库、M2 只读列表，用户看不到
// 「到底哪几个字段不一样」，也无法裁决。本节补齐两件事：
//   ① conflict_detail —— 把本地当前行与远端被拒变更逐字段对比，产出可直接渲染的差异表；
//   ② resolve_conflict / resolve_all_conflicts —— 用户裁决「保留本地」或「采用远端」。
// ---------------------------------------------------------------------------

/// 参与同步表的中文标签（UI 展示用；放后端做单一事实源，避免前后端各维护一份）。
pub fn table_label(tbl: &str) -> &'static str {
    match tbl {
        "positions" => "持仓",
        "funds" => "基金",
        "transactions" => "交易流水",
        "snapshots" => "净值快照",
        "position_daily" => "持仓日线",
        "settings" => "设置",
        "grid_funds" => "网格基金",
        "grid_signal" => "网格信号",
        "grid_signal_history" => "网格信号历史",
        "grid_pending_rebuy" => "网格待回补",
        "grid_settings" => "网格设置",
        "accounts" => "账户",
        "platform_templates" => "平台模板",
        _ => "未知表",
    }
}

/// 冲突中单个字段的本地值 vs 远端值（值可能为 null）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictField {
    pub col: String,
    pub local: Option<Value>,
    pub remote: Option<Value>,
}

/// 一条冲突的完整详情，供 UI 做字段级对比与裁决。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDetail {
    pub id: i64,
    pub tbl: String,
    pub table_label: String,
    pub row_key: String,
    pub device: String,
    pub created_at: String,
    pub resolved: bool,
    /// 远端意图：`upsert` 改行 / `delete` 删行 / `corrupt` 载荷无法解析。
    pub op: String,
    /// 本地当前是否还有这一行（false = 本地已删或从未有）。
    pub local_exists: bool,
    /// 逐字段差异；主键列与 updated_at 不列入（主键单独展示、updated_at 属同步内部戳）。
    pub fields: Vec<ConflictField>,
    /// 无实质差异（采用远端与保留本地结果相同）——UI 可只提供「保留本地」。
    pub identical: bool,
    /// op == "corrupt" 时的解析错误说明。
    pub payload_error: Option<String>,
}

/// sync_conflicts 的原始行（M3 内部读取用；对外经 ConflictDetail 暴露）。
struct RawConflict {
    id: i64,
    tbl: String,
    row_key: String,
    device: String,
    payload: String,
    resolved: i64,
    created_at: String,
}

const RAW_CONFLICT_COLS: &str = "id, tbl, row_key, device, payload, resolved, created_at";

fn row_to_raw_conflict(r: &rusqlite::Row<'_>) -> SqlResult<RawConflict> {
    Ok(RawConflict {
        id: r.get(0)?,
        tbl: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        row_key: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        device: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        payload: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
        resolved: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
        created_at: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
    })
}

fn read_raw_conflict(conn: &Connection, id: i64) -> SqlResult<Option<RawConflict>> {
    let sql = format!("SELECT {RAW_CONFLICT_COLS} FROM sync_conflicts WHERE id = ?1");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([id])?;
    match rows.next()? {
        Some(r) => Ok(Some(row_to_raw_conflict(r)?)),
        None => Ok(None),
    }
}

/// 全部未解冲突，按 id 升序（保证批量「采用远端」的回放顺序与逐条裁决一致）。
fn list_unresolved_conflicts(conn: &Connection) -> SqlResult<Vec<RawConflict>> {
    let sql =
        format!("SELECT {RAW_CONFLICT_COLS} FROM sync_conflicts WHERE resolved = 0 ORDER BY id ASC");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        out.push(row_to_raw_conflict(r)?);
    }
    Ok(out)
}

/// 远端载荷语义：`Ok(None)` = 远端意图删行（payload 为空）；`Ok(Some)` = 改行内容。
/// 空串与「解析失败」必须区别对待——否则损坏的载荷会被误判成 delete，裁决时错删本地行。
fn remote_payload(c: &RawConflict) -> Result<Option<Value>, String> {
    let s = c.payload.trim();
    if s.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Object(m)) if !m.is_empty() => Ok(Some(Value::Object(m))),
        Ok(_) => Err("冲突载荷不是非空 JSON 对象".to_string()),
        Err(e) => Err(format!("冲突载荷解析失败: {e}")),
    }
}

/// 把冲突载荷转成一条可回放的远端变更。载荷损坏时返回错误（绝不退化成 delete）。
fn change_from_conflict(c: &RawConflict) -> Result<Change, String> {
    let payload = remote_payload(c)?;
    let op = if payload.is_some() { "upsert" } else { "delete" };
    Ok(Change {
        tbl: c.tbl.clone(),
        row_key: c.row_key.clone(),
        op: op.to_string(),
        ts: String::new(),
        payload,
    })
}

/// 按 row_key 读本地当前行快照；row_key / 表不合法时返回 None（不报错，详情降级展示）。
fn local_row_of(conn: &Connection, c: &RawConflict) -> SqlResult<Option<Value>> {
    let pk_cols = match pk_columns(&c.tbl) {
        Some(c) => c,
        None => return Ok(None),
    };
    let pk = match serde_json::from_str::<Vec<Value>>(&c.row_key) {
        Ok(p) if p.len() == pk_cols.len() => p,
        _ => return Ok(None),
    };
    select_row_by_pk(conn, &c.tbl, &pk)
}

/// 读取一条冲突的详情：本地当前行 vs 远端被拒变更，逐字段列出差异。
pub fn conflict_detail(conn: &Connection, id: i64) -> SqlResult<Option<ConflictDetail>> {
    let c = match read_raw_conflict(conn, id)? {
        Some(c) => c,
        None => return Ok(None),
    };
    let local = local_row_of(conn, &c)?;
    let parsed = remote_payload(&c);
    let (op, remote, payload_error) = match &parsed {
        Ok(Some(v)) => ("upsert", Some(v.clone()), None),
        Ok(None) => ("delete", None, None),
        Err(e) => ("corrupt", None, Some(e.clone())),
    };

    // 逐字段差异：主键单独展示；updated_at 是同步内部戳（两边必然不同），列入只会制造噪音。
    let pk_cols = pk_columns(&c.tbl).unwrap_or(&[]);
    let mut fields = Vec::new();
    if let (Some(Value::Object(l)), Some(Value::Object(r))) = (local.as_ref(), remote.as_ref()) {
        let mut keys: Vec<&String> = l.keys().chain(r.keys()).collect();
        keys.sort();
        keys.dedup();
        for k in keys {
            if k == "updated_at" || pk_cols.contains(&k.as_str()) {
                continue;
            }
            let lv = l.get(k).cloned().unwrap_or(Value::Null);
            let rv = r.get(k).cloned().unwrap_or(Value::Null);
            if lv != rv {
                fields.push(ConflictField {
                    col: k.clone(),
                    local: Some(lv),
                    remote: Some(rv),
                });
            }
        }
    }
    let local_exists = local.is_some();
    // 无实质差异 = 采用远端与保留本地结果相同：
    // - 删行意图：本地已经没有这行了；
    // - 改行意图：本地有这行且各字段一致（若本地已无该行，采用远端会重新插回来，是有实质变化的）；
    // - 载荷损坏：无从判断，一律给 false，让用户看到操作入口。
    let identical = match op {
        "delete" => !local_exists,
        "upsert" => local_exists && fields.is_empty(),
        _ => false,
    };

    Ok(Some(ConflictDetail {
        id: c.id,
        tbl: c.tbl.clone(),
        table_label: table_label(&c.tbl).to_string(),
        row_key: c.row_key.clone(),
        device: c.device.clone(),
        created_at: c.created_at.clone(),
        resolved: c.resolved != 0,
        op: op.to_string(),
        local_exists,
        fields,
        identical,
        payload_error,
    }))
}

/// 强制把远端变更写回本地（「采用远端」裁决）。
///
/// 与回放（`apply_one_upsert`）的两点关键差别：
/// 1. **不设 `sync_pause`** —— 触发器照常记 sync_log，使本次裁决作为「新版本」向其它设备传播；
/// 2. **剔除 payload 里的 `updated_at` 再写** —— 交由触发器盖为 now。若原样写入远端旧时间戳，
///    `au` 触发器会因 `OLD.updated_at != NEW.updated_at` 而不记账（本次裁决丢失），
///    且本地会继续带着旧时间戳输给对端，冲突反复出现。
///
/// 行存在则 UPDATE（只碰载荷提供的列，未提供的列保留本地值），不存在则 INSERT。
///
/// 返回 `Ok(1)` = 写回了一行。载荷结构不合法（非对象 / 缺主键 / 无可写列）一律返回 `Err`——
/// 用户既然选了「采用远端」，静默不生效是最坏的结果；报错可让该条冲突保持未解并提示用户。
fn force_apply_remote(conn: &Connection, ch: &Change) -> Result<usize, String> {
    let map = match &ch.payload {
        Some(Value::Object(m)) if !m.is_empty() => m,
        _ => return Err("远端载荷为空或不是 JSON 对象".to_string()),
    };
    let valid_cols = synced_columns(conn, &ch.tbl).map_err(|e| format!("读取表结构失败: {e}"))?;
    let pks = match pk_columns(&ch.tbl) {
        Some(c) => c,
        None => return Err(format!("表不在同步白名单内: {}", ch.tbl)),
    };
    let missing: Vec<&str> = pks
        .iter()
        .copied()
        .filter(|p| !map.contains_key(*p))
        .collect();
    if !missing.is_empty() {
        return Err(format!("远端载荷缺少主键列: {}", missing.join(", ")));
    }
    // 注意迭代方向：以「表自身的合法列」为基准去 payload 里取值，而非以 payload 的键为基准
    // 去拼 SQL —— 载荷来自其它设备的快照，键不可信；反向过滤可保证未知列天然进不了语句。
    let mutable: Vec<&str> = valid_cols
        .iter()
        .map(|s| s.as_str())
        .filter(|c| *c != "updated_at" && map.contains_key(*c))
        .collect();
    if mutable.is_empty() {
        return Err("远端载荷不含任何可写列".to_string());
    }
    let pk_vals: Vec<Value> = pks
        .iter()
        .map(|p| map.get(*p).cloned().unwrap_or(Value::Null))
        .collect();
    let exists = select_row_by_pk(conn, &ch.tbl, &pk_vals)
        .map_err(|e| format!("定位本地行失败: {e}"))?
        .is_some();

    let mut boxes: Vec<Box<dyn rusqlite::ToSql>> = mutable
        .iter()
        .map(|c| json_to_boxed_sql(map.get(*c).unwrap_or(&Value::Null)))
        .collect();
    let sql = if exists {
        let set_clause = mutable
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let where_clause = pks
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", mutable.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        boxes.extend(pk_vals.iter().map(json_to_boxed_sql));
        format!("UPDATE {} SET {} WHERE {}", ch.tbl, set_clause, where_clause)
    } else {
        let placeholders = vec!["?"; mutable.len()].join(",");
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            ch.tbl,
            mutable.join(","),
            placeholders
        )
    };
    let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, params_from_iter(refs.iter().copied()))
        .map_err(|e| describe_writeback_error(&ch.tbl, &e))?;
    Ok(1)
}

/// 把写回失败转成可操作的说明。
///
/// 唯一索引冲突是最需要解释的一种：跨设备各自新建了「逻辑上同一条」业务记录（如 positions 以
/// 自增 `id` 为同步主键，但业务身份其实是 `account_id + fund_code + platform`），两边 id 不同、
/// 自然键相同。此时强行 Insert 会撞 `uq_positions_account_fund_platform`。
/// 这里**不自动合并**——合并两条持仓是会影响用户资产的语义决策，必须由用户确认；
/// 因此仅把原始 SQL 错误翻译成用户能看懂、能照做的提示，冲突保持未解。
fn describe_writeback_error(tbl: &str, e: &rusqlite::Error) -> String {
    let raw = e.to_string();
    if let Some((_, cols)) = raw.split_once("UNIQUE constraint failed:") {
        return format!(
            "远端这条记录与本地另一条记录指向同一条业务记录（唯一键冲突：{}），\
             无法直接采用。请先保留本地，并在对应页面合并这两条重复记录后重新同步。",
            cols.trim()
        );
    }
    format!("写回本地失败（{tbl}）: {raw}")
}

/// 按远端意图把一条冲突写回本地。载荷损坏 → 报错，绝不退化成删除。
fn apply_remote_conflict(conn: &Connection, c: &RawConflict) -> Result<usize, String> {
    let ch = change_from_conflict(c)?;
    if ch.op == "delete" {
        delete_by_pk(conn, &ch).map_err(|e| format!("删除本地行失败: {e}"))?;
        Ok(1)
    } else {
        force_apply_remote(conn, &ch)
    }
}

fn mark_resolved(conn: &Connection, id: i64) -> SqlResult<()> {
    conn.execute(
        "UPDATE sync_conflicts SET resolved = 1 WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// 把面向用户的说明包成 rusqlite 错误。
///
/// `with_conn` 的闭包必须返回 `SqlResult`，而 `SqliteFailure(_, Some(msg))` 的 Display 恰好就是
/// msg 本身（不带内部前缀），与 db.rs「数据库未初始化」的既有约定一致。
/// 不要改用 `InvalidParameterName`——它的 Display 会给用户看到 "Invalid parameter name: ..."。
fn user_error(msg: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(msg.into()),
    )
}

/// 解算一条冲突。`adopt_remote = true` → 采用远端（覆盖/删除本地行）；false → 保留本地（仅清标记）。
/// 返回 `(是否找到该冲突, 实际写回行数)`。
pub fn resolve_conflict(
    conn: &Connection,
    id: i64,
    adopt_remote: bool,
) -> SqlResult<(bool, usize)> {
    let c = match read_raw_conflict(conn, id)? {
        Some(c) => c,
        None => return Ok((false, 0)),
    };
    let mut applied = 0usize;
    if adopt_remote {
        // 写回失败（载荷损坏 / 唯一键冲突）时不置 resolved，让该条继续留在列表里由用户处理。
        applied = apply_remote_conflict(conn, &c).map_err(user_error)?;
    }
    mark_resolved(conn, id)?;
    Ok((true, applied))
}

/// 批量解算全部未解冲突。返回 `(已解条数, 写回行数, 失败条数)`。
///
/// 单条失败（载荷损坏）不中断整批：跳过并计入 failed，该条保持未解，用户在列表里仍能看到。
pub fn resolve_all_conflicts(
    conn: &Connection,
    adopt_remote: bool,
) -> SqlResult<(usize, usize, usize)> {
    let list = list_unresolved_conflicts(conn)?;
    let mut applied = 0usize;
    let mut failed = 0usize;
    let mut resolved = 0usize;
    for c in &list {
        if adopt_remote {
            match apply_remote_conflict(conn, c) {
                Ok(n) => applied += n,
                Err(_) => {
                    failed += 1;
                    continue;
                }
            }
        }
        mark_resolved(conn, c.id)?;
        resolved += 1;
    }
    Ok((resolved, applied, failed))
}

/// 全量导出（首次同步基线）：逐参与表按业务主键 SELECT 全行，输出 upsert Change（ts 留空）。
///
/// M1 语义：ts 留空 + `apply_changeset`（严格回放）用于一次性把源端状态铺到空库。
/// M3 的 LWW 合并请改用 `full_device_snapshot`（ts 带真实 updated_at）。
pub fn baseline_export(conn: &Connection) -> SqlResult<Vec<Change>> {
    live_rows(conn, false)
}

/// 导出全部存活行为 upsert Change；`with_ts=true` 时以该行 `updated_at` 作为 ts（LWW 可比）。
/// 遍历顺序 = SNAPSHOT_TABLE_ORDER（父表优先，外键安全）。
fn live_rows(conn: &Connection, with_ts: bool) -> SqlResult<Vec<Change>> {
    let mut out = Vec::new();
    for tbl in SNAPSHOT_TABLE_ORDER {
        let pks = match pk_columns(tbl) {
            Some(c) => c,
            None => continue,
        };
        let pk_list = pks.join(",");
        let mut stmt = conn.prepare(&format!("SELECT {pk_list} FROM {tbl}"))?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let mut pk_vals: Vec<Value> = Vec::with_capacity(pks.len());
            for i in 0..pks.len() {
                let v: RusqliteValue = r.get(i)?;
                pk_vals.push(sql_value_to_json(v));
            }
            if let Some(payload) = select_row_by_pk(conn, tbl, &pk_vals)? {
                let row_key = serde_json::to_string(&pk_vals).unwrap_or_default();
                // 存量行（M1 迁移前）updated_at 可能为空串 → ts 留空，由 LWW 的「无信息」规则处理。
                let ts = if with_ts {
                    payload
                        .get("updated_at")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    String::new()
                };
                out.push(Change {
                    tbl: (*tbl).to_string(),
                    row_key,
                    op: "upsert".into(),
                    ts,
                    payload: Some(payload),
                });
            }
        }
    }
    Ok(out)
}

/// 设备快照（M3 文件通道的载荷语义）= 源端「当前逻辑状态」的可重放表示。
///
/// 组成：
/// 1) 删除墓碑：`sync_log` 中 op='delete' 的 (tbl,row_key) 去重取最新 ts；**仅对当前已不存在的行发出**
///    （防「删除后同主键重建」被墓碑误删）。
/// 2) 存活行 upsert：`live_rows(conn, true)`，ts = 该行 updated_at。
///
/// 与 `collect_changeset`（增量、payload 取当前行快照）的关键差异：本函数**不含历史 upsert 条目**，
/// 因此不会出现「旧 ts 携带新 payload」而在对端制造假冲突。重放幂等（LWW + upsert/delete），
/// 可反复导入并向多设备收敛；真正的增量/水位推进由 M2 云通道承担。
pub fn full_device_snapshot(conn: &Connection) -> SqlResult<Vec<Change>> {
    let mut out = Vec::new();

    // 1) 删除墓碑（去重取最新 ts），跳过当前仍存在的行。
    let mut stmt = conn.prepare(
        "SELECT tbl, row_key, MAX(ts) FROM sync_log WHERE op='delete' GROUP BY tbl, row_key",
    )?;
    let mut rows = stmt.query([])?;
    let mut tombs: Vec<(String, String, String)> = Vec::new();
    while let Some(r) = rows.next()? {
        let tbl: String = r.get(0)?;
        let row_key: String = r.get(1)?;
        let ts: Option<String> = r.get(2)?;
        if !is_synced_table(&tbl) {
            continue;
        }
        tombs.push((tbl, row_key, ts.unwrap_or_default()));
    }
    for (tbl, row_key, ts) in tombs {
        let pk = match parse_row_key(&row_key) {
            Ok(p) => p,
            Err(_) => continue, // row_key 损坏则跳过
        };
        // 当前已存在（如删除后同主键重建）→ 不发明碑，交给存活行 upsert。
        if select_row_by_pk(conn, &tbl, &pk)?.is_some() {
            continue;
        }
        out.push(Change {
            tbl,
            row_key,
            op: "delete".into(),
            ts,
            payload: None,
        });
    }

    // 2) 存活行（ts 取行 updated_at）。
    out.extend(live_rows(conn, true)?);
    Ok(out)
}

/// 读取本设备同步水位（watermark）。
///
/// D3：watermark 存于 sync_meta(key='watermark')，value 为 JSON `{"ts": "...", "id": <i64>}`。
/// 返回 None 表示尚无水位（首次同步）。key 不存在或 JSON 损坏时返回 None（容错）。
pub fn read_watermark(conn: &Connection) -> SqlResult<Option<(String, i64)>> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM sync_meta WHERE key='watermark'",
            [],
            |r| r.get(0),
        )
        .ok();
    match v {
        Some(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(o) if o.is_object() => {
                let ts = o
                    .get("ts")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = o.get("id").and_then(|x| x.as_i64()).unwrap_or(0);
                Ok(Some((ts, id)))
            }
            _ => Ok(None),
        },
        None => Ok(None),
    }
}

/// 写入/推进本设备同步水位（watermark）。已存在则覆盖（sync_meta.key 主键 ON CONFLICT）。
pub fn write_watermark(conn: &Connection, ts: &str, id: i64) -> SqlResult<()> {
    let val = serde_json::json!({ "ts": ts, "id": id }).to_string();
    conn.execute(
        "INSERT INTO sync_meta(key, value) VALUES('watermark', ?1) \
         ON CONFLICT(key) DO UPDATE SET value = ?1",
        [val],
    )?;
    Ok(())
}

/// 写入/更新 sync_meta 任意键值（如最近导出/导入时间），已存在则覆盖。
/// key 属内部固定常量（非外部输入），参数化绑定无注入风险。
pub fn write_meta(conn: &Connection, key: &str, value: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO sync_meta(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// 读取 sync_meta 任意键值；key 不存在返回 None（容错）。
pub fn read_meta(conn: &Connection, key: &str) -> SqlResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM sync_meta WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .ok())
}

// ---- M3：设备快照文件格式（JSONL：首行头 + 每行一条 Change）----

/// 快照头行版本号（解析时校验；未知版本仍尽力解析）。
pub const SNAPSHOT_FORMAT: i64 = 1;
/// sync_meta 键：本设备标识（首次使用时生成并持久化）。
pub const META_DEVICE_ID: &str = "device_id";
/// sync_meta 键：最近一次导出/导入时间（ISO8601 本地时间）。
pub const META_LAST_EXPORT: &str = "last_file_export";
pub const META_LAST_IMPORT: &str = "last_file_import";

/// 快照文件头（首行 JSON）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SnapshotHeader {
    /// 格式版本，恒为 SNAPSHOT_FORMAT
    pub fl_sync: i64,
    /// 源设备标识
    pub device: String,
    /// 导出时间（源端本地时间字符串）
    pub exported_at: String,
    /// 变更条数（不含头行）
    pub count: usize,
}

/// 序列化设备快照为 JSONL 文本：首行头 + 每条 Change 一行 JSON。
/// 纯函数（不碰 IO），便于单测与 M2 云通道复用（云通道上传的也是同一载荷）。
pub fn snapshot_to_jsonl(changes: &[Change], device: &str, exported_at: &str) -> String {
    let header = SnapshotHeader {
        fl_sync: SNAPSHOT_FORMAT,
        device: device.to_string(),
        exported_at: exported_at.to_string(),
        count: changes.len(),
    };
    let mut s = String::with_capacity(changes.len() * 256 + 128);
    s.push_str(&serde_json::to_string(&header).unwrap_or_default());
    s.push('\n');
    for c in changes {
        if let Ok(line) = serde_json::to_string(c) {
            s.push_str(&line);
            s.push('\n');
        }
    }
    s
}

/// 解析设备快照 JSONL 文本 → (头, 变更集)。
///
/// 容错：空行跳过；首行若不含 `fl_sync` 字段则视为无头文件（头为 None，全部行按 Change 解析）。
/// 任一 Change 行坏掉 → 返回 Err（含行号），**不做部分解析**，由调用方在落库前整体拒绝。
pub fn parse_snapshot(text: &str) -> SqlResult<(Option<SnapshotHeader>, Vec<Change>)> {
    let mut header: Option<SnapshotHeader> = None;
    let mut out: Vec<Change> = Vec::new();
    let mut first_content = true;
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if first_content {
            first_content = false;
            // 头行：能解析为对象且含 fl_sync 字段 → 记头并继续；否则按普通 Change 行处理。
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if v.get("fl_sync").is_some() {
                    header = serde_json::from_value::<SnapshotHeader>(v).ok();
                    continue;
                }
            }
        }
        match serde_json::from_str::<Change>(line) {
            Ok(c) => out.push(c),
            Err(e) => {
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "第 {} 行解析失败: {e}",
                    idx + 1
                )));
            }
        }
    }
    Ok((header, out))
}

/// 取本设备标识：sync_meta(device_id) 不存在时生成（时间戳 + 进程号，足够本地唯一）并落库。
pub fn device_id(conn: &Connection) -> SqlResult<String> {
    if let Some(id) = read_meta(conn, META_DEVICE_ID)? {
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("dev-{:x}-{:x}", nanos, std::process::id());
    write_meta(conn, META_DEVICE_ID, &id)?;
    Ok(id)
}

/// 当前 sync_log 末端的复合游标 (ts, id)；空日志返回 ("", 0)。
/// 用于记录「某次导出时的日志位置」，以便统计其后新增的变更条数（UI 的待同步计数）。
pub fn latest_log_cursor(conn: &Connection) -> SqlResult<(String, i64)> {
    let mut stmt = conn.prepare("SELECT ts, id FROM sync_log ORDER BY ts DESC, id DESC LIMIT 1")?;
    let mut rows = stmt.query([])?;
    match rows.next()? {
        Some(r) => Ok((r.get(0)?, r.get(1)?)),
        None => Ok((String::new(), 0)),
    }
}

/// 统计 (after_ts, after_id) 之后的 sync_log 条数（不含白名单过滤：仅计数，不读内容）。
pub fn count_changes_after(conn: &Connection, after_ts: &str, after_id: i64) -> SqlResult<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM sync_log WHERE (ts > ?1) OR (ts = ?1 AND id > ?2)",
        rusqlite::params![after_ts, after_id],
        |r| r.get(0),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rusqlite::Connection;

    /// 在内存库上建最小参与表集合（列名尽量贴近真实 schema）+ 排除表 nav_history，
    /// 再跑生产迁移函数建立 updated_at 列 / 触发器 / sync_* 表。
    ///
    /// pub(crate)：cloud.rs 的编排测试（推送/拉取/事务回滚）需要与同步内核**同一套 schema**，
    /// 复用本函数可保证「触发器口径」与排障预期不分叉。
    pub(crate) fn setup(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE funds (code TEXT PRIMARY KEY, name TEXT NOT NULL, platform TEXT NOT NULL);
             CREATE TABLE positions (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL, shares REAL NOT NULL);
             CREATE TABLE transactions (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT, amount REAL NOT NULL);
             CREATE TABLE snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, account_id INTEGER NOT NULL DEFAULT 1, snapshot_date TEXT NOT NULL);
             CREATE TABLE position_daily (position_id INTEGER NOT NULL, nav_date TEXT NOT NULL, shares REAL NOT NULL, PRIMARY KEY(position_id, nav_date));
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);
             CREATE TABLE platform_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, platform TEXT NOT NULL UNIQUE, ocr_rules TEXT);
             CREATE TABLE grid_funds (fund_code TEXT PRIMARY KEY, enabled INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE grid_signal (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL, signal_date TEXT NOT NULL);
             CREATE TABLE grid_signal_history (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL);
             CREATE TABLE grid_pending_rebuy (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL);
             CREATE TABLE grid_settings (k TEXT PRIMARY KEY, v TEXT);
             CREATE TABLE nav_history (fund_code TEXT NOT NULL, nav_date TEXT NOT NULL, nav REAL NOT NULL, PRIMARY KEY(fund_code, nav_date));",
        )
        .unwrap();
        crate::db::init_sync_schema(conn).unwrap();
    }

    // ① 触发器生效：insert/update 参与表 → sync_log 出现对应 upsert（row_key 为业务主键）且 updated_at 被填；
    //    delete → delete 墓碑；排除表不产生 sync_log。
    #[test]
    fn triggers_track_changes() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);

        conn.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000001','测试','alipay')",
            [],
        )
        .unwrap();

        let ua: String = conn
            .query_row("SELECT updated_at FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert!(!ua.is_empty(), "AFTER INSERT 触发器应填充 updated_at");

        // D2：sync_log 记的是 row_key（业务主键 JSON 数组），而非 rowid
        let rk: String = conn
            .query_row(
                "SELECT row_key FROM sync_log WHERE tbl='funds' AND op='upsert' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rk, "[\"000001\"]", "upsert 应记业务主键 row_key");

        let n_upsert: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_log WHERE tbl='funds' AND op='upsert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_upsert, 1, "insert 应记 1 条 upsert");

        conn.execute("UPDATE funds SET name='改名' WHERE code='000001'", [])
            .unwrap();
        let n_upsert2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_log WHERE tbl='funds' AND op='upsert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_upsert2, 2, "update 应再记 1 条 upsert");

        conn.execute("DELETE FROM funds WHERE code='000001'", []).unwrap();
        let n_del: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_log WHERE tbl='funds' AND op='delete'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_del, 1, "delete 应记 1 条墓碑");
    }

    // ② collect_changeset 按复合水位过滤、upsert 行带完整 payload。
    #[test]
    fn collect_respects_watermark_and_payload() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);

        conn.execute("INSERT INTO positions(fund_code,shares) VALUES('000001',100)", [])
            .unwrap();
        // 间隔几毫秒，确保两条变更 ts 不同（strftime 毫秒精度），watermark 测试才可确定。
        std::thread::sleep(std::time::Duration::from_millis(3));
        conn.execute("INSERT INTO positions(fund_code,shares) VALUES('000002',200)", [])
            .unwrap();

        let all = collect_changeset(&conn, "", 0).unwrap();
        assert_eq!(all.len(), 2);
        for c in &all {
            assert_eq!(c.op, "upsert");
            let p = c.payload.as_ref().unwrap();
            // payload 必须包含该行的全部列（含自增主键与 updated_at）
            for col in ["id", "fund_code", "shares", "updated_at"] {
                assert!(p.get(col).is_some(), "payload 缺失列 {col}");
            }
            assert!(p["fund_code"].is_string(), "fund_code 应为字符串");
        }

        // 复合水位 = (最新变更 ts, 最新 id) → 之后无变更
        let (max_ts, max_id): (String, i64) = conn
            .query_row(
                "SELECT ts, id FROM sync_log ORDER BY ts DESC, id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let after_max = collect_changeset(&conn, &max_ts, max_id).unwrap();
        assert_eq!(after_max.len(), 0, "watermark 之后的变更应为空");

        // 复合水位 = (第一条变更 ts, 第一条 id) → 其自身（等于）被排除，仅剩更晚的
        let (first_ts, first_id): (String, i64) = conn
            .query_row(
                "SELECT ts, id FROM sync_log ORDER BY ts ASC, id ASC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let after_first = collect_changeset(&conn, &first_ts, first_id).unwrap();
        assert!(
            after_first.iter().all(|c| c.ts > first_ts || (c.ts == first_ts && true)),
            "watermark 之后的变更 ts 必须严格大于 watermark（同毫秒按 id 严格大于）"
        );
        assert_eq!(after_first.len(), 1);
    }

    // ③ apply_changeset 幂等：同一 changeset 应用两遍结果一致（行数/内容）；无 error。
    #[test]
    fn apply_is_idempotent() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO settings(key,value) VALUES('a','1')", [])
            .unwrap();
        src.execute("INSERT INTO settings(key,value) VALUES('b','2')", [])
            .unwrap();
        let changes = collect_changeset(&src, "", 0).unwrap();

        let dump = |c: &Connection| -> Vec<(String, String)> {
            let mut stmt = c.prepare("SELECT key, value FROM settings").unwrap();
            let mut rows = stmt.query([]).unwrap();
            let mut v = Vec::new();
            while let Some(r) = rows.next().unwrap() {
                v.push((r.get(0).unwrap(), r.get(1).unwrap()));
            }
            v.sort();
            v
        };

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (n1, e1) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n1, 2);
        assert_eq!(e1, 0);
        let snap1 = dump(&dst);

        let (n2, e2) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n2, 2, "第二遍应用条数应一致");
        assert_eq!(e2, 0);
        let snap2 = dump(&dst);
        assert_eq!(snap1, snap2, "两遍应用后数据内容必须一致");
    }

    // ④ LWW：旧变更被跳过并写 sync_conflicts（row_key 同口径）；新变更正常应用。
    #[test]
    fn lww_skips_stale_and_records_conflict() {
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        // seed 一行；其 updated_at 被插入触发器填为『现在』（2026...）
        dst.execute(
            "INSERT INTO funds(code,name,platform) VALUES('X','old','alipay')",
            [],
        )
        .unwrap();

        // 旧变更（ts 在过去）针对已存在行 X → 冲突、跳过；row_key 为业务主键 ["X"]
        let stale = Change {
            tbl: "funds".into(),
            row_key: "[\"X\"]".into(),
            op: "upsert".into(),
            ts: "2000-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "X", "name": "stale", "platform": "alipay", "updated_at": ""
            })),
        };
        // 新变更（目标无此行 Y）→ 正常应用；row_key 为业务主键 ["Y"]
        let fresh = Change {
            tbl: "funds".into(),
            row_key: "[\"Y\"]".into(),
            op: "upsert".into(),
            ts: "2000-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "Y", "name": "new", "platform": "alipay", "updated_at": ""
            })),
        };

        let (applied, conflicts) = apply_changeset_lww(&dst, &[stale, fresh], "devA").unwrap();
        assert_eq!(applied, 1);
        assert_eq!(conflicts, 1);

        let xname: String = dst
            .query_row("SELECT name FROM funds WHERE code='X'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(xname, "old", "被跳过的旧变更不应改写目标行");

        let yname: String = dst
            .query_row("SELECT name FROM funds WHERE code='Y'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(yname, "new");

        let nc: i64 = dst
            .query_row(
                "SELECT COUNT(*) FROM sync_conflicts WHERE resolved=0 AND row_key='[\"X\"]'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nc, 1, "应记录 1 条未解冲突（row_key 口径）");
    }

    // ⑤ 排除表不产生 sync_log 记录（对 nav_history 做 insert/update/delete 后 sync_log 无该表条目）。
    #[test]
    fn excluded_tables_not_tracked() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);

        conn.execute(
            "INSERT INTO nav_history(fund_code,nav_date,nav) VALUES('000001','2026-01-01',1.0)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE nav_history SET nav=2.0 WHERE fund_code='000001'",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM nav_history WHERE fund_code='000001'", [])
            .unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_log WHERE tbl='nav_history'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 0, "nav_history 为排除表，不得产生 sync_log 记录");

        // 健全性：参与表仍被正常跟踪
        conn.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000002','x','alipay')",
            [],
        )
        .unwrap();
        let nf: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_log WHERE tbl='funds'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nf, 1);
    }

    // ⑥ 墓碑：delete 导出 → apply 到另一空库 → 目标行不存在。
    #[test]
    fn tombstone_export_and_apply() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO positions(fund_code,shares) VALUES('000001',100)", [])
            .unwrap();
        src.execute("DELETE FROM positions WHERE fund_code='000001'", [])
            .unwrap();

        // collect：insert(行已删→降级 delete 墓碑) + delete(墓碑) = 2 条 delete
        let changes = collect_changeset(&src, "", 0).unwrap();
        assert_eq!(
            changes.iter().filter(|c| c.op == "delete").count(),
            2,
            "应含 2 条 delete（insert 行已删降级 + 显式 delete）"
        );

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (n, e) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n, changes.len());
        assert_eq!(e, 0);

        let cnt: i64 = dst
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 0, "应用墓碑后目标库该行必须不存在");
    }

    // 附加：baseline_export 全量导出 → apply 到空库 → 数据完整迁移。
    #[test]
    fn baseline_export_roundtrip() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('000001','A','alipay')", [])
            .unwrap();
        src.execute("INSERT INTO settings(key,value) VALUES('k','v')", [])
            .unwrap();
        src.execute("INSERT INTO accounts(name) VALUES('默认账户')", [])
            .unwrap();

        let baseline = baseline_export(&src).unwrap();
        assert!(baseline.iter().any(|c| c.tbl == "funds"));
        assert!(baseline.iter().any(|c| c.tbl == "settings"));
        assert!(baseline.iter().any(|c| c.tbl == "accounts"));

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (n, e) = apply_changeset(&dst, &baseline).unwrap();
        assert_eq!(n, baseline.len());
        assert_eq!(e, 0);

        let cnt_funds: i64 = dst
            .query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0))
            .unwrap();
        let cnt_acc: i64 = dst
            .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt_funds, 1);
        assert_eq!(cnt_acc, 1);
    }

    // ⑧ M3：设备快照往返——含删除墓碑，对端得到与源端一致的逻辑状态（删除的行不复活）。
    #[test]
    fn snapshot_roundtrip_includes_deletes() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F1','A','alipay')", [])
            .unwrap();
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F2','B','alipay')", [])
            .unwrap();
        src.execute("INSERT INTO positions(fund_code,shares) VALUES('F1',100)", [])
            .unwrap();
        src.execute("DELETE FROM funds WHERE code='F1'", []).unwrap();

        let snap = full_device_snapshot(&src).unwrap();
        // 墓碑：F1（已删）；存活：F2 / positions
        assert!(
            snap.iter().any(|c| c.tbl == "funds" && c.row_key == "[\"F1\"]" && c.op == "delete"),
            "已删行应输出 delete 墓碑"
        );
        let f2 = snap.iter().find(|c| c.row_key == "[\"F2\"]").expect("F2 应在快照中");
        assert_eq!(f2.op, "upsert");
        assert!(!f2.ts.is_empty(), "存活行 upsert 应携带该行 updated_at 作为 ts");

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (applied, conflicts) = apply_changeset_lww(&dst, &snap, "devA").unwrap();
        assert_eq!(conflicts, 0, "空库导入不应产生冲突");
        assert!(applied >= snap.len() - 1, "除墓碑外应全部应用");

        let n_funds: i64 = dst.query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0)).unwrap();
        assert_eq!(n_funds, 1, "F1 已被删除，不应复活");
        let n_pos: i64 = dst
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_pos, 1, "存活行应被带入");
    }

    // ⑨ M3：快照重复导入幂等（数据不翻倍、状态不变）。
    #[test]
    fn snapshot_import_is_idempotent() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F1','A','alipay')", [])
            .unwrap();
        src.execute("INSERT INTO settings(key,value) VALUES('k','v')", [])
            .unwrap();
        let snap = full_device_snapshot(&src).unwrap();

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        apply_changeset_lww(&dst, &snap, "devA").unwrap();
        let (_, conflicts2) = apply_changeset_lww(&dst, &snap, "devA").unwrap();

        assert_eq!(conflicts2, 0, "同一快照二次导入不应产生冲突");
        let n_funds: i64 = dst.query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0)).unwrap();
        let n_set: i64 = dst.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0)).unwrap();
        assert_eq!((n_funds, n_set), (1, 1), "幂等：行数不翻倍");
    }

    // ⑩ M3：LWW——对端该行更新更晚时，快照中的旧值被跳过并记冲突（本地新值不被回退）。
    #[test]
    fn snapshot_lww_keeps_newer_local() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F1','src','alipay')", [])
            .unwrap();
        let snap = full_device_snapshot(&src).unwrap();

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        dst.execute("INSERT INTO funds(code,name,platform) VALUES('F1','local-new','alipay')", [])
            .unwrap();
        // 手工把本地行时间推到未来（越过快照 ts）；不等 updated_at 时不触发 au 触发器改写。
        dst.execute("UPDATE funds SET updated_at='2099-01-01 00:00:00.000' WHERE code='F1'", [])
            .unwrap();

        let (_, conflicts) = apply_changeset_lww(&dst, &snap, "devA").unwrap();
        assert_eq!(conflicts, 1, "本地更新的行遇到较旧快照 → 记冲突");
        let name: String = dst
            .query_row("SELECT name FROM funds WHERE code='F1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "local-new", "本地较新值不应被回退");
    }

    // ⑪ M3：ts 为空的存量行快照 —— 不制造冲突、不覆盖对端已有 updated_at 的行。
    #[test]
    fn snapshot_empty_ts_does_not_conflict() {
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        dst.execute("INSERT INTO funds(code,name,platform) VALUES('F1','local','alipay')", [])
            .unwrap();

        let legacy = Change {
            tbl: "funds".into(),
            row_key: "[\"F1\"]".into(),
            op: "upsert".into(),
            ts: String::new(),
            payload: Some(serde_json::json!({
                "code": "F1", "name": "legacy", "platform": "alipay", "updated_at": ""
            })),
        };
        let (applied, conflicts) = apply_changeset_lww(&dst, &[legacy], "devA").unwrap();
        assert_eq!((applied, conflicts), (0, 0), "空 ts 不产生冲突也不覆盖");
        let name: String = dst
            .query_row("SELECT name FROM funds WHERE code='F1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "local");
    }

    // ⑫ M3：快照导出顺序必须覆盖且仅覆盖参与同步的表（防未来加表漏配顺序）。
    #[test]
    fn snapshot_table_order_matches_synced_tables() {
        let mut a: Vec<&str> = SYNCED_TABLES.to_vec();
        let mut b: Vec<&str> = SNAPSHOT_TABLE_ORDER.to_vec();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "SNAPSHOT_TABLE_ORDER 与 SYNCED_TABLES 必须一一对应");
    }

    // ⑬ M3：父表先于子表（funds 在 positions 之前），否则真实库回放会撞外键。
    #[test]
    fn snapshot_table_order_is_fk_safe() {
        let idx = |t: &str| SNAPSHOT_TABLE_ORDER.iter().position(|x| *x == t).unwrap();
        assert!(idx("funds") < idx("positions"), "funds 应先于 positions");
        assert!(idx("funds") < idx("transactions"), "funds 应先于 transactions");
        assert!(idx("funds") < idx("snapshots"), "funds 应先于 snapshots");
        assert!(idx("positions") < idx("position_daily"), "positions 应先于 position_daily");
    }

    // ⑭ M3：快照 JSONL 序列化/解析往返；坏行整体拒绝并报行号。
    #[test]
    fn snapshot_jsonl_roundtrip_and_bad_line() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F1','A','alipay')", [])
            .unwrap();
        let snap = full_device_snapshot(&src).unwrap();
        let text = snapshot_to_jsonl(&snap, "devA", "2026-09-10 20:00:00");

        let (header, parsed) = parse_snapshot(&text).unwrap();
        let h = header.expect("应解析出头行");
        assert_eq!(h.fl_sync, SNAPSHOT_FORMAT);
        assert_eq!(h.device, "devA");
        assert_eq!(h.count, snap.len());
        assert_eq!(parsed.len(), snap.len());
        assert_eq!(parsed[0].tbl, snap[0].tbl);

        // 无头文件（纯 Change 行）也能解析
        let headless = format!("{}\n", serde_json::to_string(&snap[0]).unwrap());
        let (h2, p2) = parse_snapshot(&headless).unwrap();
        assert!(h2.is_none(), "无头文件头为 None");
        assert_eq!(p2.len(), 1);

        // 坏行 → Err（含行号），不返回部分结果
        let bad = format!("{text}{{not-json}}\n");
        let err = parse_snapshot(&bad).unwrap_err();
        assert!(format!("{err}").contains("行解析失败"));
    }

    // ⑦ D1（P0）：回放全程禁用触发器 → 不产生 sync_log，且不覆盖源端 updated_at（防回环）。
    #[test]
    fn apply_produces_no_sync_log_and_preserves_updated_at() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('F1','A','alipay')", [])
            .unwrap();
        let src_ua: String = src
            .query_row("SELECT updated_at FROM funds WHERE code='F1'", [], |r| r.get(0))
            .unwrap();
        let changes = collect_changeset(&src, "", 0).unwrap();

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (applied, errors) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(applied, 1);
        assert_eq!(errors, 0);

        // D1：回放不得点燃触发器 → dst 不应产生任何 sync_log（否则会与源端形成双向无限同步）
        let n_log: i64 = dst
            .query_row("SELECT COUNT(*) FROM sync_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_log, 0, "回放不应产生 sync_log（否则会回环）");

        // D1：dst 目标行 updated_at 必须等于源端 ts（未被回放时刻覆盖）
        let dst_ua: String = dst
            .query_row("SELECT updated_at FROM funds WHERE code='F1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dst_ua, src_ua, "回放不应覆盖源端 updated_at");

        // 守卫释放：回放结束后普通业务写应再次被记录（sync_pause 已清除）
        dst.execute("INSERT INTO funds(code,name,platform) VALUES('F2','B','alipay')", [])
            .unwrap();
        let n_log2: i64 = dst
            .query_row("SELECT COUNT(*) FROM sync_log", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_log2, 1, "回放结束后写操作应恢复记录（守卫已释放）");
    }

    // ⑧ D2：跨设备墓碑按业务主键删对行——TEXT 主键(funds) 与复合主键(position_daily)。
    #[test]
    fn tombstone_deletes_correct_row_by_row_key() {
        // funds：TEXT 主键
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO funds(code,name,platform) VALUES('A','a','alipay')", [])
            .unwrap();
        src.execute("INSERT INTO funds(code,name,platform) VALUES('B','b','alipay')", [])
            .unwrap();
        src.execute("DELETE FROM funds WHERE code='A'", []).unwrap();
        let changes = collect_changeset(&src, "", 0).unwrap();

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        dst.execute("INSERT INTO funds(code,name,platform) VALUES('A','a','alipay')", [])
            .unwrap();
        dst.execute("INSERT INTO funds(code,name,platform) VALUES('B','b','alipay')", [])
            .unwrap();
        apply_changeset(&dst, &changes).unwrap();
        let cnt_a: i64 = dst
            .query_row("SELECT COUNT(*) FROM funds WHERE code='A'", [], |r| r.get(0))
            .unwrap();
        let cnt_b: i64 = dst
            .query_row("SELECT COUNT(*) FROM funds WHERE code='B'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt_a, 0, "A 应被墓碑删对行");
        assert_eq!(cnt_b, 1, "B 不应被误删");

        // position_daily：复合主键
        let src2 = Connection::open_in_memory().unwrap();
        setup(&src2);
        src2.execute(
            "INSERT INTO position_daily(position_id,nav_date,shares) VALUES(1,'2026-01-01',10)",
            [],
        )
        .unwrap();
        src2.execute(
            "INSERT INTO position_daily(position_id,nav_date,shares) VALUES(1,'2026-01-02',20)",
            [],
        )
        .unwrap();
        src2.execute(
            "DELETE FROM position_daily WHERE position_id=1 AND nav_date='2026-01-01'",
            [],
        )
        .unwrap();
        let changes2 = collect_changeset(&src2, "", 0).unwrap();

        let dst2 = Connection::open_in_memory().unwrap();
        setup(&dst2);
        dst2.execute(
            "INSERT INTO position_daily(position_id,nav_date,shares) VALUES(1,'2026-01-01',10)",
            [],
        )
        .unwrap();
        dst2.execute(
            "INSERT INTO position_daily(position_id,nav_date,shares) VALUES(1,'2026-01-02',20)",
            [],
        )
        .unwrap();
        apply_changeset(&dst2, &changes2).unwrap();
        let cnt_d1: i64 = dst2
            .query_row(
                "SELECT COUNT(*) FROM position_daily WHERE nav_date='2026-01-01'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let cnt_d2: i64 = dst2
            .query_row(
                "SELECT COUNT(*) FROM position_daily WHERE nav_date='2026-01-02'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cnt_d1, 0, "复合主键 2026-01-01 应被删对行");
        assert_eq!(cnt_d2, 1, "复合主键 2026-01-02 不应被误删");
    }

    // ⑨ D3：同毫秒复合水位——两条同 ts、不同 id 的 sync_log，watermark=(ts, 首行id) 仅返回第二条。
    #[test]
    fn watermark_tie_break_same_millisecond() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        // 直接插入两条同 ts、不同 id 的 sync_log（绕过触发器，精确控制）
        conn.execute(
            "INSERT INTO sync_log(tbl,row_key,op,ts) VALUES('funds',json_array('A'),'upsert','2026-01-01 00:00:00.000')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_log(tbl,row_key,op,ts) VALUES('funds',json_array('B'),'upsert','2026-01-01 00:00:00.000')",
            [],
        )
        .unwrap();
        let first_id: i64 = conn
            .query_row("SELECT id FROM sync_log ORDER BY id ASC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        // 复合水位 = (同 ts, 首行 id) → 应只返回第二行（B）
        let after = collect_changeset(&conn, "2026-01-01 00:00:00.000", first_id).unwrap();
        assert_eq!(after.len(), 1, "同毫秒复合水位应只返回 id 更大的那条");
        let rk: Vec<Value> = serde_json::from_str(&after[0].row_key).unwrap();
        assert_eq!(rk[0], serde_json::json!("B"));
    }

    // ⑩ D4：upsert 未知列丢弃、主键缺失整条跳过。
    #[test]
    fn apply_drops_unknown_cols_and_skips_missing_pk() {
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        // 未知列 'hacked' 应被丢弃，已知列正常写入
        let with_unknown = Change {
            tbl: "funds".into(),
            row_key: "[\"Z\"]".into(),
            op: "upsert".into(),
            ts: "2026-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "Z", "name": "z", "platform": "alipay",
                "updated_at": "", "hacked": "x"
            })),
        };
        let (applied, errors) = apply_changeset(&dst, &[with_unknown]).unwrap();
        assert_eq!(applied, 1);
        assert_eq!(errors, 0);
        // 未知列不应出现在表中（列白名单校验，D4）
        let hack: Result<String, _> =
            dst.query_row("SELECT hacked FROM funds WHERE code='Z'", [], |r| r.get(0));
        assert!(hack.is_err(), "未知列不应被写入");
        let zname: String = dst
            .query_row("SELECT name FROM funds WHERE code='Z'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(zname, "z");

        // 主键缺失（funds 的 code 缺失）→ 整条跳过
        let missing_pk = Change {
            tbl: "funds".into(),
            row_key: "[\"W\"]".into(),
            op: "upsert".into(),
            ts: "2026-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({ "name": "w", "platform": "alipay", "updated_at": "" })),
        };
        let (applied2, errors2) = apply_changeset(&dst, &[missing_pk]).unwrap();
        assert_eq!(applied2, 0, "主键缺失应跳过");
        assert_eq!(errors2, 1, "主键缺失应计 1 个错误");
        let wc: i64 = dst
            .query_row("SELECT COUNT(*) FROM funds WHERE code='W'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(wc, 0, "主键缺失不应写入");
    }

    // ⑪ D3：sync_meta 水位读写 helper 成对可测。
    #[test]
    fn watermark_meta_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        assert!(read_watermark(&conn).unwrap().is_none(), "初始无水位");
        write_watermark(&conn, "2026-01-01 00:00:00.000", 7).unwrap();
        assert_eq!(
            read_watermark(&conn).unwrap(),
            Some(("2026-01-01 00:00:00.000".to_string(), 7))
        );
        // 再次写入应覆盖（ON CONFLICT）
        write_watermark(&conn, "2026-02-02 00:00:00.000", 9).unwrap();
        assert_eq!(
            read_watermark(&conn).unwrap(),
            Some(("2026-02-02 00:00:00.000".to_string(), 9))
        );
    }

    // 真实库副本迁移验证（默认忽略，需 FUNDLENS_REAL_DB 指向 .backup 出的副本，勿动原库）。
    // 跑法：sqlite3 原库 ".backup '/tmp/fundlens_mig_test.db'" 后
    //   FUNDLENS_REAL_DB=/tmp/fundlens_mig_test.db \
    //   cargo test --manifest-path src-tauri/Cargo.toml --lib --no-default-features migrate_real_db_copy -- --ignored
    #[test]
    #[ignore]
    fn migrate_real_db_copy() {
        let path =
            std::env::var("FUNDLENS_REAL_DB").expect("set FUNDLENS_REAL_DB to a copy of the real db");
        let conn = Connection::open(&path).unwrap();
        // 连跑两遍，验证幂等（列已存在跳过、触发器 DROP+CREATE 不报错、旧 row_id 形状重建）
        crate::db::init_sync_schema(&conn).unwrap();
        crate::db::init_sync_schema(&conn).unwrap();

        // D2：sync_log 已采用 row_key TEXT、且旧 row_id 列已被清除
        let has_rowkey: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('sync_log') WHERE name='row_key'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(has_rowkey, "真实库 sync_log 缺少 row_key 列");
        let has_old: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('sync_log') WHERE name='row_id'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(!has_old, "真实库 sync_log 仍残留旧 row_id 列");

        for t in SYNCED_TABLES {
            let has_ua: bool = conn
                .query_row(
                    &format!(
                        "SELECT 1 FROM pragma_table_info('{t}') WHERE name='updated_at'"
                    ),
                    [],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            assert!(has_ua, "真实库 {t} 缺少 updated_at 列");
            for suf in ["_ai", "_au", "_ad"] {
                let trg: bool = conn
                    .query_row(
                        &format!(
                            "SELECT 1 FROM sqlite_master WHERE type='trigger' AND name='{t}{suf}'"
                        ),
                        [],
                        |_| Ok(true),
                    )
                    .unwrap_or(false);
                assert!(trg, "真实库 {t}{suf} 触发器缺失");
            }
        }
        let has_log: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='sync_log'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        assert!(has_log, "sync_log 表缺失");
        println!("real db migration OK on {path}");
    }

    // -----------------------------------------------------------------------
    // M3：冲突详情与解算
    // -----------------------------------------------------------------------

    /// 本地插入一行 funds（触发器会填 updated_at = now）。
    fn insert_local_fund(conn: &Connection, name: &str) {
        conn.execute(
            "INSERT INTO funds(code,name,platform) VALUES('000001',?1,'alipay')",
            rusqlite::params![name],
        )
        .unwrap();
    }

    /// 记一条「远端更旧」的冲突（同一 funds 行），返回冲突 id。
    fn record_stale_remote(conn: &Connection, device: &str, remote_name: &str) -> i64 {
        let ch = Change {
            tbl: "funds".to_string(),
            row_key: "[\"000001\"]".to_string(),
            op: "upsert".to_string(),
            ts: "2000-01-01 00:00:00.000".to_string(),
            payload: Some(serde_json::json!({
                "code": "000001",
                "name": remote_name,
                "platform": "alipay",
                "updated_at": "2000-01-01 00:00:00.000"
            })),
        };
        let (applied, conflicts) = apply_changeset_lww(conn, &[ch], device).unwrap();
        assert_eq!((applied, conflicts), (0, 1), "陈旧远端变更应被拒并记冲突");
        conn.query_row("SELECT MAX(id) FROM sync_conflicts", [], |r| r.get(0))
            .unwrap()
    }

    /// 记一条载荷损坏的冲突（模拟被截断/污染的 payload）。
    fn record_corrupt(conn: &Connection) -> i64 {
        conn.execute(
            "INSERT INTO sync_conflicts(tbl,row_key,device,payload,resolved,created_at) \
             VALUES('funds','[\"000001\"]','devX','{not json',0,'2026-09-10 00:00:00.000')",
            [],
        )
        .unwrap();
        conn.query_row("SELECT MAX(id) FROM sync_conflicts", [], |r| r.get(0))
            .unwrap()
    }

    fn conflict_resolved(conn: &Connection, id: i64) -> i64 {
        conn.query_row(
            "SELECT resolved FROM sync_conflicts WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn local_fund_name(conn: &Connection) -> String {
        conn.query_row("SELECT name FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap()
    }

    // 详情只列出真正不同的字段：相同列不列、主键不列、updated_at（同步内部戳）不列。
    #[test]
    fn conflict_detail_lists_only_changed_fields() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let id = record_stale_remote(&conn, "devB", "远端名");

        let d = conflict_detail(&conn, id).unwrap().expect("冲突应存在");
        assert_eq!(d.tbl, "funds");
        assert_eq!(d.table_label, "基金", "表名应映射为中文标签");
        assert_eq!(d.device, "devB", "来源设备应记录");
        assert_eq!(d.op, "upsert");
        assert!(d.local_exists);
        assert!(!d.identical, "name 不同 → 尚有差异");
        assert!(d.payload_error.is_none());
        assert_eq!(d.fields.len(), 1, "应只列 name 一处差异: {:?}", d.fields);
        assert_eq!(d.fields[0].col, "name");
        assert_eq!(d.fields[0].local, Some(serde_json::json!("本地名")));
        assert_eq!(d.fields[0].remote, Some(serde_json::json!("远端名")));

        assert!(conflict_detail(&conn, 999_999).unwrap().is_none(), "不存在的 id 返回 None");
    }

    // 「保留本地」只清标记，绝不动数据。
    #[test]
    fn resolve_keep_local_leaves_data_untouched() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let id = record_stale_remote(&conn, "devB", "远端名");

        let (found, applied) = resolve_conflict(&conn, id, false).unwrap();
        assert_eq!((found, applied), (true, 0), "保留本地不写回任何行");
        assert_eq!(local_fund_name(&conn), "本地名");
        assert_eq!(conflict_resolved(&conn, id), 1);

        let (resolved, applied, failed) = resolve_all_conflicts(&conn, false).unwrap();
        assert_eq!((resolved, applied, failed), (0, 0, 0), "已无未解冲突");
    }

    // 「采用远端」覆盖本地值，并作为**新版本**记入 sync_log（否则裁决传不出去）。
    #[test]
    fn resolve_adopt_remote_overwrites_row_and_logs_new_version() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let id = record_stale_remote(&conn, "devB", "远端名");
        let logs_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_log WHERE tbl='funds'", [], |r| r.get(0))
            .unwrap();

        let (found, applied) = resolve_conflict(&conn, id, true).unwrap();
        assert_eq!((found, applied), (true, 1), "采用远端应写回 1 行");
        assert_eq!(local_fund_name(&conn), "远端名", "应采用远端值覆盖本地");
        assert_eq!(conflict_resolved(&conn, id), 1);

        let logs_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM sync_log WHERE tbl='funds'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            logs_after,
            logs_before + 1,
            "裁决应作为新版本记入 sync_log，供对端拉取"
        );

        // 关键：不得把远端的旧时间戳原样写回，否则 au 触发器不记账且冲突会反复出现。
        let ts: String = conn
            .query_row("SELECT updated_at FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert!(
            ts > "2000-01-01 00:00:00.000".to_string(),
            "updated_at 应被盖为新戳（交由触发器记账），实际 {ts}"
        );
    }

    // 空载荷 = 远端意图删行：详情标为 delete，采用远端即删掉本地行。
    #[test]
    fn empty_payload_means_remote_delete_intent() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let ch = Change {
            tbl: "funds".to_string(),
            row_key: "[\"000001\"]".to_string(),
            op: "delete".to_string(),
            ts: "2000-01-01 00:00:00.000".to_string(),
            payload: None,
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (0, 1), "较旧的删除意图也应记为冲突");
        let id: i64 = conn
            .query_row("SELECT MAX(id) FROM sync_conflicts", [], |r| r.get(0))
            .unwrap();

        let d = conflict_detail(&conn, id).unwrap().unwrap();
        assert_eq!(d.op, "delete", "空载荷应判为远端删行意图");
        assert!(d.local_exists);
        assert!(!d.identical, "本地仍有该行 → 与远端意图不一致");
        assert!(d.fields.is_empty(), "删行意图无字段差异可比");

        let (found, applied) = resolve_conflict(&conn, id, true).unwrap();
        assert_eq!((found, applied), (true, 1));
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "采用远端删除意图应删掉本地行");
    }

    // 载荷损坏必须报错，绝不退化成「删除本地行」，且保持未解留给用户处理。
    #[test]
    fn corrupt_payload_errors_and_never_deletes_row() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let id = record_corrupt(&conn);

        let d = conflict_detail(&conn, id).unwrap().unwrap();
        assert_eq!(d.op, "corrupt");
        assert!(d.payload_error.is_some(), "应给出解析失败说明");
        assert!(!d.identical);

        let err = resolve_conflict(&conn, id, true).unwrap_err();
        assert!(
            err.to_string().contains("载荷"),
            "报错应指向载荷问题，实际: {err}"
        );
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "损坏的冲突不得删掉本地行");
        assert_eq!(conflict_resolved(&conn, id), 0, "处理失败应保持未解");
    }

    // 批量「采用远端」：坏条目计失败并跳过，好条目照常生效，不因一条坏数据中断整批。
    #[test]
    fn resolve_all_adopt_remote_skips_corrupt_entries() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "本地名");
        let good = record_stale_remote(&conn, "devB", "远端B");
        let bad = record_corrupt(&conn);

        let (resolved, applied, failed) = resolve_all_conflicts(&conn, true).unwrap();
        assert_eq!((resolved, applied, failed), (1, 1, 1));
        assert_eq!(local_fund_name(&conn), "远端B", "好条目应采用远端值");
        assert_eq!(conflict_resolved(&conn, good), 1);
        assert_eq!(conflict_resolved(&conn, bad), 0, "坏条目保持未解");
    }

    // 表名标签覆盖全部参与同步的表（漏配会退化成「未知表」，这里守住）。
    #[test]
    fn every_synced_table_has_a_label() {
        for t in SYNCED_TABLES {
            assert_ne!(
                table_label(t),
                "未知表",
                "参与同步的表 {t} 缺少中文标签"
            );
        }
    }

    /// 直接塞一条冲突行（用于构造单元测试不便生成的载荷）。
    fn insert_conflict_raw(conn: &Connection, tbl: &str, row_key: &str, payload: &str) -> i64 {
        conn.execute(
            "INSERT INTO sync_conflicts(tbl,row_key,device,payload,resolved,created_at) \
             VALUES(?1,?2,'devB',?3,0,'2026-09-10 00:00:00.000')",
            rusqlite::params![tbl, row_key, payload],
        )
        .unwrap();
        conn.query_row("SELECT MAX(id) FROM sync_conflicts", [], |r| r.get(0))
            .unwrap()
    }

    // 本地已无该行时，远端改行意图属于实质变化（采用远端会把行插回来），不得判为「无差异」。
    #[test]
    fn upsert_conflict_without_local_row_is_not_identical() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let payload =
            serde_json::json!({"code": "000009", "name": "远端新增", "platform": "alipay"})
                .to_string();
        let id = insert_conflict_raw(&conn, "funds", "[\"000009\"]", &payload);

        let d = conflict_detail(&conn, id).unwrap().unwrap();
        assert!(!d.local_exists, "本地没有这一行");
        assert!(d.fields.is_empty(), "无本地行可比字段");
        assert!(!d.identical, "采用远端会重新插入该行，属实质变化");

        let (found, applied) = resolve_conflict(&conn, id, true).unwrap();
        assert_eq!((found, applied), (true, 1));
        let name: String = conn
            .query_row("SELECT name FROM funds WHERE code='000009'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "远端新增", "采用远端应把行插回本地");
    }

    // 两边内容完全一致时标为「无差异」，UI 据此提示无需改动数据。
    #[test]
    fn upsert_conflict_with_identical_content_is_flagged_identical() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        insert_local_fund(&conn, "同名");
        let payload = serde_json::json!({"code": "000001", "name": "同名", "platform": "alipay"})
            .to_string();
        let id = insert_conflict_raw(&conn, "funds", "[\"000001\"]", &payload);

        let d = conflict_detail(&conn, id).unwrap().unwrap();
        assert!(d.local_exists);
        assert!(d.fields.is_empty(), "字段全同 → 无差异");
        assert!(d.identical, "内容一致应标记为无差异");
    }

    // 真实库的坑：业务表有自然键唯一索引（如 positions 的 account_id+fund_code+platform），
    // 而跨设备同步主键是自增 id。两台设备各自新建「逻辑上同一条」记录 → id 不同、自然键相同，
    // 采用远端时按 id 找不到本地行 → 走 INSERT → 撞唯一索引。
    // 此时必须给出可操作的说明，而不是把原始 SQL 错误丢给用户，也不能留下半截数据。
    // 用 platform_templates（platform 列有 UNIQUE）复现同一形状。
    #[test]
    fn adopt_remote_unique_collision_gives_actionable_error() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute(
            "INSERT INTO platform_templates(id,platform,ocr_rules) VALUES(1,'alipay','本地规则')",
            [],
        )
        .unwrap();
        let payload =
            serde_json::json!({"id": 2, "platform": "alipay", "ocr_rules": "远端规则"}).to_string();
        let id = insert_conflict_raw(&conn, "platform_templates", "[\"2\"]", &payload);

        let err = resolve_conflict(&conn, id, true).unwrap_err().to_string();
        assert!(
            err.contains("唯一键冲突"),
            "应说明唯一键冲突而非抛原始 SQL: {err}"
        );
        assert!(
            err.contains("合并"),
            "应给出可照做的下一步（合并重复记录）: {err}"
        );
        assert!(
            !err.contains("Invalid parameter name"),
            "不得把内部错误前缀暴露给用户: {err}"
        );

        // 失败必须无副作用：本地行不被改动、冲突保持未解。
        let rules: String = conn
            .query_row(
                "SELECT ocr_rules FROM platform_templates WHERE platform='alipay'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rules, "本地规则", "写回失败不得改动本地行");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM platform_templates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "写回失败不得插入半截数据");
        assert_eq!(conflict_resolved(&conn, id), 0, "失败应保持未解");
    }
}
