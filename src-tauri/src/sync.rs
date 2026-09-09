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

/// 参与同步表 → 业务主键列清单（表驱动，单一事实源）。
///
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
#[derive(Debug, Clone)]
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

/// 全量导出（首次同步基线）：逐参与表按业务主键 SELECT 全行，输出 upsert Change（ts 留空）。
pub fn baseline_export(conn: &Connection) -> SqlResult<Vec<Change>> {
    let mut out = Vec::new();
    for tbl in SYNCED_TABLES {
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
                out.push(Change {
                    tbl: (*tbl).to_string(),
                    row_key,
                    op: "upsert".into(),
                    ts: String::new(),
                    payload: Some(payload),
                });
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// 在内存库上建最小参与表集合（列名尽量贴近真实 schema）+ 排除表 nav_history，
    /// 再跑生产迁移函数建立 updated_at 列 / 触发器 / sync_* 表。
    fn setup(conn: &Connection) {
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
}
