// FundLens · Phase-2 CloudBase 同步 M1 内核（纯本地、无云、无 UI、无新命令）。
//
// 设计要点（详见仓库根 perf-cloudbase-v2.6.0-design-2026-09-09.md §4）：
// - 本地 SQLite 仍是唯一事实源；变更由 db.rs 中的 SQLite 触发器在 DB 层自动跟踪，
//   业务写代码零侵入（见 db::init_sync_schema）。
// - 本模块是「纯函数内核」：所有函数都接收 &Connection，不依赖 Tauri / 全局 DB 单例，
//   因此可在内存库上无副作用单测，未来也可在云拉取/回放路径直接复用。
// - 表名一律走白名单（SYNCED_TABLES），列名一律来自行 json 自身而非外部输入，杜绝 SQL 注入。

use rusqlite::types::Value as RusqliteValue;
use rusqlite::{params_from_iter, Connection, OptionalExtension, Result as SqlResult};
use serde_json::Value;

/// 本地内部列：只在本设备内有意义（自增 rowid 别名 / 本地自引用），**绝不跨设备回放**。
/// 远端载荷携带这些列时一律丢弃——覆盖本地值会撕裂本地引用链或指向错误的本地行
/// （2026-09-11 P0 身份改造）。
/// - `id`：各 GUIDED 表的本地自增主键；
/// - `related_tx_id`：transactions 本地自引用；
/// - `position_id`：position_daily → positions 的本地自增外键。跨设备由 `position_guid`
///   （父行 sync_guid）承载业务联动，回放 INSERT 时反解为本地 positions.id（见
///   resolve_position_daily_parent），绝不直接回放远端 id。
const INTERNAL_LOCAL_COLS: &[&str] = &["id", "related_tx_id", "position_id"];

/// 参与同步的用户态表（白名单）。派生/缓存表（nav_history、disclosures、quotes_cache、
/// est_cache、stock_profile、stock_style、index_constituent、ocr_jobs、quote_jobs、
/// import_sessions、trading_calendar、migrations、sync_* 等）不在此列——各设备自行从官方源重拉。
///
/// position_daily（2026-09-11 身份映射改造）回归同步集合：同步身份 = 自身 `sync_guid`，
/// 与父持仓的业务联动由 `position_guid`（= 父行 positions.sync_guid）承载；
/// 本地自增 position_id 列入 INTERNAL_LOCAL_COLS，回放 INSERT 时反解。
///
/// 这是「参与同步的表」的唯一事实源，db::init_sync_schema 也引用它。
pub const SYNCED_TABLES: &[&str] = &[
    "positions",
    "funds",
    "transactions",
    "snapshots",
    "settings",
    "grid_funds",
    "grid_signal",
    "grid_signal_history",
    "grid_pending_rebuy",
    "grid_settings",
    "accounts",
    "platform_templates",
    "position_daily",
];

/// 同步身份 = `sync_guid`（跨设备稳定的 32 位随机 hex，行插入后由 ai 触发器内联派生、
/// 启动时批量回填）的表。这些表的本地主键是自增 rowid（id），跨设备必然错位——同一 id
/// 在两台设备上是两行不同的数据，用作同步身份会导致 LWW 覆盖错行、删除墓碑误删
/// （2026-09-11 P0 改造）。GUIDED 表的 PK_COLUMNS 一律为 ["sync_guid"]。
/// position_daily 另有父链列 position_guid（= 父 positions.sync_guid），见 db.rs 触发器生成处。
pub const GUIDED_TABLES: &[&str] = &[
    "positions",
    "transactions",
    "snapshots",
    "accounts",
    "platform_templates",
    "grid_signal",
    "grid_signal_history",
    "grid_pending_rebuy",
    "position_daily",
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
    "position_daily",
    "transactions",
    "snapshots",
    "grid_signal",
    "grid_signal_history",
    "grid_pending_rebuy",
];

/// 变更集回放的**父表优先级**（SNAPSHOT_TABLE_ORDER 序，未知表排最后）。
/// apply_changeset / apply_changeset_lww 先按此稳定排序再回放：同批变更里
/// positions 先于 position_daily，position_daily 回放反解父 id 才不会因「父行未到」跳过。
fn table_priority(tbl: &str) -> usize {
    SNAPSHOT_TABLE_ORDER
        .iter()
        .position(|t| *t == tbl)
        .unwrap_or(usize::MAX)
}

/// 参与同步表 → 业务主键列清单（表驱动，单一事实源）。
/// D2：跨设备稳定身份必须走业务主键，而非内部 rowid。各表主键逐一核对（取自 db.rs 建表 DDL）：
/// - GUIDED_TABLES（positions / transactions / snapshots / accounts / platform_templates /
///   grid_signal / grid_signal_history / grid_pending_rebuy / position_daily）：
///   **同步身份 = `sync_guid`**
///   （2026-09-11 P0 改造——自增 `id` 跨设备错位，用作身份会导致 LWW 覆盖错行、墓碑误删；
///   guid 由 db.rs 触发器在行插入后内联派生 + 启动回填，列定义见 GUIDED_TABLES 注释）
/// - funds：`code`；settings：`key`；grid_funds：`fund_code`；grid_settings：`k`（均为 TEXT 主键）
///
/// 本表同时被 db.rs（生成触发器记录 json_array(<pk>)）与 sync.rs（DELETE/LWW 按 pk 定位）引用，
/// 保证「记日志」与「按主键回放」口径一致。必须与 SYNCED_TABLES 完全对应（13 张）。
pub const PK_COLUMNS: &[(&str, &[&str])] = &[
    ("positions", &["sync_guid"]),
    ("funds", &["code"]),
    ("transactions", &["sync_guid"]),
    ("snapshots", &["sync_guid"]),
    ("settings", &["key"]),
    ("grid_funds", &["fund_code"]),
    ("grid_signal", &["sync_guid"]),
    ("grid_signal_history", &["sync_guid"]),
    ("grid_pending_rebuy", &["sync_guid"]),
    ("grid_settings", &["k"]),
    ("accounts", &["sync_guid"]),
    ("platform_templates", &["sync_guid"]),
    ("position_daily", &["sync_guid"]),
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

/// 幂等回放单条 upsert：UPDATE 命中则按载荷列更新，未命中才 INSERT。
///
/// ⚠️ P0 禁用 `INSERT OR REPLACE`（2026-09-11 麒麟披露丢失事故）：
/// REPLACE 对 funds 行会先 DELETE 旧行再 INSERT，沿外键 `ON DELETE CASCADE`
/// 静默抹掉该基金的 positions / disclosures / position_daily——而 disclosures 是
/// 设备本地派生表（不在同步集合），删了永远无法从快照恢复。改为 UPDATE-or-INSERT 后，
/// 已存在行的回放只改列值、绝不触发行删除，级联链从根上失效。
///
/// 语义差异说明：REPLACE 会把载荷未携带的列重置为默认值，UPDATE 则保留本地旧值；
/// 本项目快照载荷恒为整行（live_rows 导出全部 synced_columns），两者等价；
/// 对增量载荷 UPDATE 反而更安全（不丢本地独有列）。
///
/// D4（列白名单校验）：列名必须属于目标表真实列（PRAGMA table_info 取），杜绝任意列名拼接注入；
/// - 未知列：丢弃该列、其余正常写入（返回 0 = 已应用）。
/// - 主键列缺失：整条跳过（返回 1 = 错误计数），因为无主键无法定位。
/// - 无任何合法列：整条跳过（返回 1）。
/// 调用方累加返回值得到 (applied, errors)。
/// position_daily 回放专用：把远端载荷里的 `position_guid`（父 positions.sync_guid）
/// 反解为本地 `positions.id`。父行尚未同步到本地 → 返回 Ok(None)，
/// 调用方跳过该行（计错误/冲突），待父行随下轮拉取落库后重放即成功。
fn resolve_position_daily_parent(
    conn: &Connection,
    map: &serde_json::Map<String, Value>,
) -> SqlResult<Option<i64>> {
    let guid = match map.get("position_guid").and_then(|v| v.as_str()) {
        Some(g) if !g.is_empty() => g,
        _ => return Ok(None), // 载荷无父链 → 无法反解（旧格式/损坏载荷）
    };
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM positions WHERE sync_guid = ?1",
            [guid],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id)
}

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
    // 仅保留合法列（未知列丢弃）；并剔除**本地内部列**：
    // - id：本地 rowid 别名，跨设备错位（同步身份是 sync_guid），回放覆盖会撕裂本地引用链
    //   （position_daily.position_id、related_tx_id 自引用等）；
    // - related_tx_id：transactions 自引用本地 id，远端值在本地无意义，宁缺勿错。
    // 主键（sync_guid）保留在 cols 里：INSERT 路径需要它；UPDATE 路径由 set_cols 排除。
    let pk_set: std::collections::HashSet<&str> = pks.iter().copied().collect();
    let cols: Vec<&String> = map
        .keys()
        .filter(|k| {
            valid_set.contains(k.as_str())
                && (pk_set.contains(k.as_str()) || !INTERNAL_LOCAL_COLS.contains(&k.as_str()))
        })
        .collect();
    if cols.is_empty() {
        return Ok(1); // 无任何合法列 → 跳过
    }
    let set_cols: Vec<&String> = cols.iter().filter(|c| !pk_set.contains(c.as_str())).copied().collect();

    if !set_cols.is_empty() {
        let set_clause = set_cols
            .iter()
            .map(|c| format!("{c}=?"))
            .collect::<Vec<_>>()
            .join(",");
        let where_clause = pks
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", set_cols.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!("UPDATE {} SET {} WHERE {}", ch.tbl, set_clause, where_clause);
        let mut boxes: Vec<Box<dyn rusqlite::ToSql>> = set_cols
            .iter()
            .map(|c| json_to_boxed_sql(map.get(*c).unwrap_or(&Value::Null)))
            .collect();
        boxes.extend(
            pks.iter()
                .map(|pk| json_to_boxed_sql(map.get(*pk).unwrap_or(&Value::Null))),
        );
        let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
        let updated = conn.execute(&sql, params_from_iter(refs.iter().copied()))?;
        if updated > 0 {
            return Ok(0); // 命中并更新：完成，绝不触发行删除（P0：见函数头注释）
        }
    }

    // 未命中 → INSERT（含主键列）
    // position_daily 特例：本地外键 position_id 已被 INTERNAL_LOCAL_COLS 剔除，
    // INSERT 必须按载荷 position_guid 反解出本地 positions.id 补上；父行未到 → 跳过（错误计数），
    // 待父行落库后的下一轮拉取重放即成功（apply_* 已按父表优先排序，同批内通常不会发生）。
    let resolved_pid: Option<i64> = if ch.tbl == "position_daily" {
        match resolve_position_daily_parent(conn, map)? {
            Some(pid) => Some(pid),
            None => return Ok(1), // 父持仓未同步 → 本轮跳过
        }
    } else {
        None
    };
    let mut col_names: Vec<String> = cols.iter().map(|s| s.as_str().to_string()).collect();
    if resolved_pid.is_some() {
        col_names.push("position_id".to_string());
    }
    let col_list = col_names.join(",");
    let placeholders = vec!["?"; col_names.len()].join(",");
    let sql = format!("INSERT INTO {} ({}) VALUES ({})", ch.tbl, col_list, placeholders);
    let mut boxes: Vec<Box<dyn rusqlite::ToSql>> = cols
        .iter()
        .map(|c| json_to_boxed_sql(map.get(*c).unwrap_or(&Value::Null)))
        .collect();
    if let Some(pid) = resolved_pid {
        boxes.push(Box::new(pid));
    }
    let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
    conn.execute(&sql, params_from_iter(refs.iter().copied()))?;
    Ok(0)
}

/// 收集表上的**唯一索引**列组合（含多列），用于识别「自然键相撞」。
/// 取自表自身元信息（PRAGMA index_list / index_info），非外部输入；
/// 跳过部分索引（partial，语义不完整）与表达式索引（列名为 NULL）。
fn unique_index_columns(conn: &Connection, tbl: &str) -> SqlResult<Vec<Vec<String>>> {
    let mut names: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare(&format!("PRAGMA index_list('{tbl}')"))?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            // index_list 列序：seq, name, unique, origin, partial
            let unique: i64 = r.get(2)?;
            let partial: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
            if unique != 0 && partial == 0 {
                names.push(r.get::<_, String>(1)?);
            }
        }
    }
    let mut out = Vec::new();
    for name in names {
        let mut stmt = conn.prepare(&format!("PRAGMA index_info('{name}')"))?;
        let mut rows = stmt.query([])?;
        let mut cols = Vec::new();
        while let Some(r) = rows.next()? {
            // index_info 列序：seqno, cid, name（表达式索引 name 为 NULL）
            if let Some(c) = r.get::<_, Option<String>>(2)? {
                cols.push(c);
            }
        }
        if !cols.is_empty() {
            out.push(cols);
        }
    }
    Ok(out)
}

/// 读取表的「列有效默认值」——即 `INSERT` 时该列若未出现在语句里会落成什么值。
///
/// `PRAGMA table_info` 的 `dflt_value` 是**默认值表达式的原文**（实测：`''`、`0`、`1`、
/// `datetime('now')`），交给 SQLite 自己求值（`SELECT <expr>`）即可得到与真实 INSERT
/// 完全一致的结果，无需自己解析 SQL 字面量。无默认值（`dflt_value` 为 NULL）→ 落 NULL。
///
/// 为什么需要它：`INSERT OR REPLACE` 只绑载荷里带的列，其余列取默认值。若某个自然键列
/// 不在载荷里，必须用默认值代入才能算出「将要插入的那一行」的自然键，否则会漏判相撞。
fn column_effective_defaults(
    conn: &Connection,
    tbl: &str,
) -> SqlResult<std::collections::HashMap<String, Value>> {
    let mut out = std::collections::HashMap::new();
    let mut stmt = conn.prepare(&format!("PRAGMA table_info('{tbl}')"))?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        // table_info 列序：cid, name, type, notnull, dflt_value, pk
        let name: String = r.get(1)?;
        let dflt: Option<String> = r.get(4)?;
        let v = match dflt {
            // 表达式来自本表 schema 自身（非外部输入），且只取单值。
            // **不吞错**：求值失败就往上抛，让本次回放整体失败并报出来 —— 这是数据安全守卫，
            // 一旦静默退化成 NULL 就可能重现「漏判相撞 → REPLACE 删数据」，宁可大声失败。
            Some(expr) => conn.query_row(&format!("SELECT {expr}"), [], |r| {
                Ok(sql_value_to_json(r.get::<_, RusqliteValue>(0)?))
            })?,
            None => Value::Null,
        };
        out.insert(name, v);
    }
    Ok(out)
}

/// 检测「载荷的自然键会撞上另一条本地行」——即 `INSERT OR REPLACE` 会**静默删掉**那条行
/// （并沿 `ON DELETE CASCADE` 级联抹掉其子表数据），而载荷自身的主键在本地并不存在。
///
/// 返回撞上的那行的主键值，未相撞返回 None。
///
/// 为什么必须前置检测：SQLite 的 `INSERT OR REPLACE` 遇唯一冲突不会报错，而是直接删除冲突行
/// 再插入 —— 无法靠捕获错误发现。典型场景：positions 的同步主键是自增 `id`，业务身份却是
/// `(account_id, fund_code, platform)`，两台设备各自新建同一持仓 → id 不同、自然键相同。
///
/// 载荷缺失的自然键列按 `column_effective_defaults` 代入默认值参与比对 —— 必须这样做，
/// 否则「载荷缺 `platform`（默认 `''`）」这类情况会被漏判：新行以 `''` 落库照样撞上本地行，
/// REPLACE 依旧静默删数据（该缺口由独立验证在真实库副本上复现）。
fn natural_key_collision(
    conn: &Connection,
    tbl: &str,
    map: &serde_json::Map<String, Value>,
) -> SqlResult<Option<Vec<Value>>> {
    let pk_cols = match pk_columns(tbl) {
        Some(c) => c,
        None => return Ok(None), // 非白名单表：不介入
    };
    // 载荷自带的主键值：若撞上的正是这一行，属正常覆盖，不算冲突。
    let own_pk: Vec<Value> = pk_cols
        .iter()
        .map(|c| map.get(*c).cloned().unwrap_or(Value::Null))
        .collect();
    // 把载荷缺失的自然键列补成「INSERT 时会落成的默认值」，据此还原**将要插入的那一行**的
    // 自然键。绝不能因为载荷缺列就跳过该索引 —— 那正是漏判相撞、静默删数据的入口。
    let defaults = column_effective_defaults(conn, tbl)?;
    for cols in unique_index_columns(conn, tbl)? {
        let key_vals: Vec<Value> = cols
            .iter()
            .map(|c| {
                map.get(c)
                    .cloned()
                    .unwrap_or_else(|| defaults.get(c).cloned().unwrap_or(Value::Null))
            })
            .collect();
        let where_clause = cols
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!(
            "SELECT {} FROM {} WHERE {} LIMIT 2",
            pk_cols.join(","),
            tbl,
            where_clause
        );
        // 注意仍用 `=` 而非 `IS`：SQLite 的唯一索引把 NULL 视为互不相同，
        // 因此「自然键含 NULL」本就不会触发 REPLACE 删除，`=` 不匹配才与之一致。
        let boxes: Vec<Box<dyn rusqlite::ToSql>> =
            key_vals.iter().map(json_to_boxed_sql).collect();
        let refs: Vec<&dyn rusqlite::ToSql> = boxes.iter().map(|b| b.as_ref()).collect();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(refs.iter().copied()))?;
        while let Some(r) = rows.next()? {
            let found: Vec<Value> = (0..pk_cols.len())
                .map(|i| sql_value_to_json(r.get::<_, RusqliteValue>(i).unwrap_or(RusqliteValue::Null)))
                .collect();
            if found != own_pk {
                return Ok(Some(found));
            }
        }
    }
    Ok(None)
}

/// 向 sync_conflicts 记一条待裁决冲突（`payload` 为远端变更快照，空串 = 远端删行意图）。
///
/// 同一 `(表, 业务主键, 来源设备)` 只保留**一条未解**冲突：LWW 下同一来源对同一行的多次被拒
/// 变更只有最新一次有意义（更旧的按定义不是胜者，本地行另行保留），重复回放（水位未推进、
/// 手工重复导入快照）不该在 UI 上堆出多条一模一样的待裁决项。来源设备不同的冲突各自保留。
fn record_conflict(
    conn: &Connection,
    tbl: &str,
    row_key: &str,
    device: &str,
    payload: &str,
) -> SqlResult<()> {
    conn.execute(
        "DELETE FROM sync_conflicts WHERE tbl=?1 AND row_key=?2 AND device=?3 AND resolved=0",
        rusqlite::params![tbl, row_key, device],
    )?;
    conn.execute(
        "INSERT INTO sync_conflicts(tbl, row_key, device, payload, resolved, created_at) \
         VALUES(?1, ?2, ?3, ?4, 0, strftime('%Y-%m-%d %H:%M:%f','now'))",
        rusqlite::params![tbl, row_key, device, payload],
    )?;
    Ok(())
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
///
/// **墓碑级联收敛（2026-09-11 Android 拉取 FK 失败实证）**：源端删父行时必已先删其子行
/// （如清库先删 transactions 再删 funds），但对端可能残留「永远等不到墓碑」的子行——
/// 源端墓碑被清除、或对端在墓碑窗口外拉取。此时父行墓碑一到，RESTRICT/NO ACTION 引用
/// 会让 DELETE 当场报错（RESTRICT）或拖到 defer_foreign_keys 的 COMMIT 才爆（NO ACTION，
/// 用户看到的「提交事务失败: FOREIGN KEY constraint failed」）。
/// 故应用父行墓碑时，按 FK 图把 RESTRICT/NO ACTION 引用子行一并收敛；
/// CASCADE / SET NULL / SET DEFAULT 交给 DDL 引擎语义（CASCADE 自动级联、SET NULL 置空
/// 配对流水），绝不用自己的级联去碰它们——否则会误删 `related_tx_id` 指向的配对行。
/// 终态与源端一致：子行是父行删除后的死数据。级联发生在 sync_pause 窗口内，不记 sync_log
/// （父行墓碑会传播到每台设备，各端各自收敛，无需转发子行墓碑）。
fn delete_by_pk(conn: &Connection, ch: &Change) -> SqlResult<()> {
    let pk = parse_row_key(&ch.row_key)?;
    let pk_cols = pk_columns(&ch.tbl).ok_or_else(|| rusqlite::Error::QueryReturnedNoRows)?;
    if pk.len() != pk_cols.len() {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    cascade_restrict_children(
        conn,
        &ch.tbl,
        pk_cols,
        &pk,
        &mut std::collections::HashSet::new(),
    )?;
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

/// 递归删除「引用 (tbl, pk_vals) 且 on_delete 为 RESTRICT / NO ACTION」的子行。
///
/// - FK 图取自 `PRAGMA foreign_key_list`（各表自身元信息，非外部输入）；
/// - visited 防自引用环（transactions.related_tx_id → transactions.id）；
/// - 递归时先取子行自身主键再逐行下钻（子行也可能被更深层 RESTRICT 引用）；
/// - 只处理单列父键匹配（本库全部 FK 均为单列，复合 FK 出现时需扩展）。
fn cascade_restrict_children(
    conn: &Connection,
    tbl: &str,
    pk_cols: &[&str],
    pk_vals: &[Value],
    visited: &mut std::collections::HashSet<String>,
) -> SqlResult<()> {
    if !visited.insert(tbl.to_string()) {
        return Ok(()); // 环：本表已在下钻链上
    }
    // 全部用户表（FK 图扫描面）；排除 sqlite_ 内部表
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    for child in &tables {
        let mut fstmt = conn.prepare(&format!("PRAGMA foreign_key_list('{child}')"))?;
        let mut rows = fstmt.query([])?;
        // 收集 (from_col, to_col, on_delete)——先取完再执行删除（避免迭代中再 prepare 写语句）
        let mut hits: Vec<(String, String, String)> = Vec::new();
        while let Some(r) = rows.next()? {
            let parent_tbl: String = r.get(2)?;
            if parent_tbl != tbl {
                continue;
            }
            let from_col: String = r.get(3)?;
            let to_col: String = r.get(4)?;
            let on_delete: String = r.get::<_, String>(6).unwrap_or_default();
            // 只级联会「卡住父行删除」的动作；CASCADE/SET NULL/SET DEFAULT 交给引擎
            if on_delete == "RESTRICT" || on_delete.is_empty() || on_delete == "NO ACTION" {
                hits.push((from_col, to_col, on_delete));
            }
        }
        drop(rows);
        drop(fstmt);
        for (from_col, to_col, _) in hits {
            // 本删除的 pk 值在该 FK 上的取值（单列匹配）
            let val = match pk_cols.iter().position(|c| *c == to_col) {
                Some(i) => pk_vals.get(i).cloned().unwrap_or(Value::Null),
                None => continue, // 该 FK 不指向本次删除的主键列（如同表多条 FK 指向不同父表）
            };
            if matches!(val, Value::Null) {
                continue;
            }
            // 先取子行自身主键（供递归下钻），再删
            let child_pks: Option<Vec<&str>> = pk_columns(child).map(|c| c.to_vec());
            let ids: Vec<Vec<Value>> = match &child_pks {
                Some(cpks) => {
                    let sql = format!(
                        "SELECT {} FROM {child} WHERE {from_col}=?1",
                        cpks.join(",")
                    );
                    let mut s = conn.prepare(&sql)?;
                    let mut rows = s.query(rusqlite::params![json_to_sql_value(&val)?])?;
                    let mut v = Vec::new();
                    while let Some(r) = rows.next()? {
                        let row: Vec<Value> = (0..cpks.len())
                            .map(|i| {
                                r.get::<_, RusqliteValue>(i)
                                    .map(sql_value_to_json)
                                    .unwrap_or(Value::Null)
                            })
                            .collect();
                        v.push(row);
                    }
                    v
                }
                None => Vec::new(), // 无白名单主键（非同步表）→ 无需下钻
            };
            conn.execute(
                &format!("DELETE FROM {child} WHERE {from_col}=?1"),
                rusqlite::params![json_to_sql_value(&val)?],
            )?;
            // 递归：子行的 RESTRICT 孙行一并收敛（如 funds 墓碑 → positions → position_daily）
            if let Some(cpks) = &child_pks {
                let cpk_refs: Vec<&str> = cpks.iter().copied().collect();
                for id in &ids {
                    if cpk_refs.len() == id.len() {
                        cascade_restrict_children(conn, child, &cpk_refs, id, visited)?;
                    }
                }
            }
        }
    }
    visited.remove(&tbl.to_string());
    Ok(())
}

/// JSON Value → rusqlite 绑定值（delete_by_pk 级联路径用；列名/表名来自白名单与元信息，值绑定无注入）。
fn json_to_sql_value(v: &Value) -> SqlResult<RusqliteValue> {
    Ok(match v {
        Value::Null => RusqliteValue::Null,
        Value::Bool(b) => RusqliteValue::Integer(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                RusqliteValue::Integer(i)
            } else {
                RusqliteValue::Real(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => RusqliteValue::Text(s.clone()),
        other => RusqliteValue::Text(other.to_string()),
    })
}

/// 幂等回放变更集（无冲突处理，直接覆盖）。
/// - upsert：UPDATE 命中按载荷列更新、未命中 INSERT（D4 列白名单校验；P0 禁用 REPLACE——见 apply_one_upsert）。
/// - delete：按 row_key（业务主键）DELETE（D2，跨设备稳定定位；行不存在则无操作，仍计入 applied）。
///
/// D1：全程 `SyncPauseGuard`（sync_meta 暂停标记，RAII）确保回放不点燃触发器，
/// 不产生 sync_log、不回写 updated_at → 不会与源端形成双向无限同步（回环）。
/// 返回 (applied, errors)：applied = 成功应用的条数；errors = 因主键缺失/无合法列被跳过的条数（D4）。
pub fn apply_changeset(conn: &Connection, changes: &[Change]) -> SqlResult<(usize, usize)> {
    let _guard = SyncPauseGuard::new(conn)?;
    // 两段式回放（FK 安全，2026-09-11）：
    // 段1 upsert 按父表优先（SNAPSHOT_TABLE_ORDER 序）——子行回放反解父 id 需父行先落库；
    // 段2 delete 按**子表优先**（逆序）——删除墓碑必须先删子行再删父行，否则在
    // defer_foreign_keys 事务里先删父行、子行还挂着 → COMMIT 时 FOREIGN KEY constraint failed。
    // 旧实现统一按父表优先排序，正是 Android 拉取失败的排序缺陷（与墓碑级联双修）。
    let mut ups: Vec<&Change> = changes.iter().filter(|c| c.op == "upsert").collect();
    let mut dels: Vec<&Change> = changes.iter().filter(|c| c.op == "delete").collect();
    ups.sort_by_key(|c| table_priority(&c.tbl));
    dels.sort_by_key(|c| std::cmp::Reverse(table_priority(&c.tbl)));
    let mut applied = 0usize;
    let mut errors = 0usize;
    for ch in ups.into_iter().chain(dels.into_iter()) {
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
    // 两段式回放（同 apply_changeset）：upsert 父表优先，delete 子表优先（FK 安全）。
    let mut ups: Vec<&Change> = changes.iter().filter(|c| c.op == "upsert").collect();
    let mut dels: Vec<&Change> = changes.iter().filter(|c| c.op == "delete").collect();
    ups.sort_by_key(|c| table_priority(&c.tbl));
    dels.sort_by_key(|c| std::cmp::Reverse(table_priority(&c.tbl)));
    let mut applied = 0usize;
    let mut conflicts = 0usize;
    for ch in ups.into_iter().chain(dels.into_iter()) {
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
            record_conflict(conn, &ch.tbl, &ch.row_key, device, &payload_str)?;
            conflicts += 1;
            continue;
        }
        // 主键未命中、但载荷的自然键撞上另一条本地行 → 按 id 插入会**顶替/混淆**那条行
        // （并沿 ON DELETE CASCADE 抹掉其子表），而 SQLite 不会报错、无法靠捕获错误发现。
        // 这种「逻辑上同一条业务记录、但跨设备 id 不同」的情形一律记冲突交给用户裁决，
        // 绝不静默丢数据。
        if ch.op == "upsert" {
            if let Some(Value::Object(map)) = ch.payload.as_ref() {
                if !map.is_empty() && natural_key_collision(conn, &ch.tbl, map)?.is_some() {
                    let payload_str = ch.payload.as_ref().map(|v| v.to_string()).unwrap_or_default();
                    record_conflict(conn, &ch.tbl, &ch.row_key, device, &payload_str)?;
                    conflicts += 1;
                    continue;
                }
            }
        }
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
    /// 无法「采用远端」的原因（如会撞上另一条本地行的自然键）；None = 可以采用。
    /// 由后端前置判定，UI 据此直接禁用按钮并说明，而不是让用户点完再吃一个错误。
    pub blocked_reason: Option<String>,
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
    // 「采用远端」是否会撞上另一条本地行（那会覆盖并删掉它）→ 提前说明并禁用该操作。
    let blocked_reason = match remote.as_ref() {
        Some(Value::Object(map)) if !map.is_empty() => {
            match natural_key_collision(conn, &c.tbl, map)? {
                Some(pk) => Some(format!(
                    "远端这条记录与本地另一条记录（{}）指向同一条业务记录；采用远端会覆盖并删除本地那一条。\
                     请先在对应页面合并这两条重复记录，再回头处理本冲突。",
                    pk_display(&pk)
                )),
                None => None,
            }
        }
        _ => None,
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
        blocked_reason,
    }))
}

/// 把主键值渲染成可读文本（字符串去引号，其余按其 JSON 文本）。
fn pk_display(pk: &[Value]) -> String {
    pk.iter()
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" / ")
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
    // 另剔除本地内部列（id / related_tx_id）：跨设备错位，覆盖会撕裂本地引用链（2026-09-11 P0）。
    let mutable: Vec<&str> = valid_cols
        .iter()
        .map(|s| s.as_str())
        .filter(|c| {
            *c != "updated_at"
                && map.contains_key(*c)
                && (pks.contains(c) || !INTERNAL_LOCAL_COLS.contains(c))
        })
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

    // 定位策略：
    // ① 按远端主键（sync_guid）命中 → 直接 UPDATE 覆盖。
    // ② 未命中但自然键撞上本地另一行（跨设备各自创建了同一业务记录，各自 guid 不同）
    //    → 「收养远端身份」：更新那条本地行（含把 sync_guid 改写为远端 guid），
    //      本地自增 id 不动 → 本地引用链（position_daily 等）完好，两设备此后身份收敛。
    //    （旧实现直接 INSERT，会撞唯一索引报错，把「同一持仓两台设备各建了一条」这种
    //      最常见的多设备场景变成永远解不开的死结。）
    // ③ 都没有 → INSERT。
    let mut adopt_where: Option<Vec<Value>> = None;
    if !exists {
        let collide = natural_key_collision(conn, &ch.tbl, map).map_err(|e| e.to_string())?;
        if collide.is_some() {
            adopt_where = collide;
        }
    }

    // position_daily INSERT 特例：本地外键 position_id 被剔除，必须按 position_guid
    // 反解本地 positions.id；父行未同步 → 显式报错（这是用户裁决路径，须给出原因）。
    let resolved_pid: Option<i64> = if ch.tbl == "position_daily" && !exists && adopt_where.is_none() {
        match resolve_position_daily_parent(conn, map).map_err(|e| e.to_string())? {
            Some(pid) => Some(pid),
            None => {
                return Err(
                    "该持仓日线记录的父持仓尚未同步到本地（position_guid 无匹配），\
                     请先同步/应用其父持仓记录，再采用此行。"
                        .to_string(),
                )
            }
        }
    } else {
        None
    };

    let mut boxes: Vec<Box<dyn rusqlite::ToSql>> = mutable
        .iter()
        .map(|c| json_to_boxed_sql(map.get(*c).unwrap_or(&Value::Null)))
        .collect();
    let sql = if exists || adopt_where.is_some() {
        let set_clause = mutable
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let where_keys = adopt_where.as_ref().unwrap_or(&pk_vals);
        let where_clause = pks
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c}=?{}", mutable.len() + i + 1))
            .collect::<Vec<_>>()
            .join(" AND ");
        boxes.extend(where_keys.iter().map(json_to_boxed_sql));
        format!("UPDATE {} SET {} WHERE {}", ch.tbl, set_clause, where_clause)
    } else {
        let mut col_names: Vec<String> = mutable.iter().map(|s| s.to_string()).collect();
        if let Some(pid) = resolved_pid {
            col_names.push("position_id".to_string());
            boxes.push(Box::new(pid));
        }
        let placeholders = vec!["?"; col_names.len()].join(",");
        format!(
            "INSERT INTO {} ({}) VALUES ({})",
            ch.tbl,
            col_names.join(","),
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
    fn upsert_must_not_cascade_delete_children() {
        // P0 回归（2026-09-11 麒麟披露丢失）：funds 行回放绝不允许触发行删除。
        // 旧实现 INSERT OR REPLACE 会先 DELETE 旧 funds 行，沿 ON DELETE CASCADE
        // 静默抹掉 positions / disclosures（disclosures 不在同步集合，删了无法从快照恢复）。
        // 修复后 UPDATE-or-INSERT：同 code 回放必须保留全部子表行。
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE funds (code TEXT PRIMARY KEY, name TEXT NOT NULL, platform TEXT NOT NULL, updated_at TEXT DEFAULT '');
             CREATE TABLE positions (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE CASCADE, shares REAL NOT NULL, updated_at TEXT DEFAULT '');
             CREATE TABLE disclosures (id INTEGER PRIMARY KEY AUTOINCREMENT, fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE CASCADE, period TEXT NOT NULL, updated_at TEXT DEFAULT '');
             INSERT INTO funds(code,name,platform,updated_at) VALUES('000001','旧名','alipay','2026-01-01 00:00:00.000');
             INSERT INTO positions(fund_code,shares,updated_at) VALUES('000001',100.0,'2026-01-01 00:00:00.000');
             INSERT INTO disclosures(fund_code,period,updated_at) VALUES('000001','2026Q2','2026-01-01 00:00:00.000');",
        )
        .unwrap();
        let ch = Change {
            tbl: "funds".into(),
            row_key: "[\"000001\"]".into(),
            op: "upsert".into(),
            ts: "2026-09-11 20:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "000001",
                "name": "新名",
                "platform": "alipay",
                "updated_at": "2026-09-11 20:00:00.000"
            })),
        };
        let applied = apply_one_upsert(&conn, &ch).unwrap();
        assert_eq!(applied, 0, "同 code 已存在行应走 UPDATE 路径成功应用");
        let positions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM positions WHERE fund_code='000001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let disclosures: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM disclosures WHERE fund_code='000001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let name: String = conn
            .query_row("SELECT name FROM funds WHERE code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(positions, 1, "funds 回放不得级联删除 positions");
        assert_eq!(disclosures, 1, "funds 回放不得级联删除 disclosures");
        assert_eq!(name, "新名", "funds 行本身应被更新");
        // 未命中主键 → 走 INSERT 路径
        let ch_new = Change {
            tbl: "funds".into(),
            row_key: "[\"000002\"]".into(),
            op: "upsert".into(),
            ts: "2026-09-11 20:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "000002", "name": "新基金", "platform": "jd",
                "updated_at": "2026-09-11 20:00:00.000"
            })),
        };
        assert_eq!(apply_one_upsert(&conn, &ch_new).unwrap(), 0, "新主键应走 INSERT");
        let n2: i64 = conn
            .query_row("SELECT COUNT(*) FROM funds WHERE code='000002'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 1, "INSERT 路径应落库");
    }

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
    // 2026-09-11 身份映射改造：position_daily 回归同步集合（身份 = 自身 sync_guid，
    // 父联动 = position_guid；本地 position_id 由回放端反解），必须排在 positions 之后。
    #[test]
    fn snapshot_table_order_is_fk_safe() {
        let idx = |t: &str| SNAPSHOT_TABLE_ORDER.iter().position(|x| *x == t).unwrap();
        assert!(idx("funds") < idx("positions"), "funds 应先于 positions");
        assert!(idx("funds") < idx("transactions"), "funds 应先于 transactions");
        assert!(idx("funds") < idx("snapshots"), "funds 应先于 snapshots");
        assert!(
            idx("positions") < idx("position_daily"),
            "positions 应先于 position_daily（子行回放反解父 id 依赖父行已落库）"
        );
        assert!(
            SYNCED_TABLES.contains(&"position_daily"),
            "position_daily 已随身份映射改造回归同步集合"
        );
    }

    // ⑫' 身份改造回归：GUIDED 表的触发器 row_key 必须记 sync_guid（而非本地自增 id）。
    #[test]
    fn guided_tables_record_sync_guid_row_key() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute(
            "INSERT INTO positions(fund_code, shares) VALUES('000001', 100.0)",
            [],
        )
        .unwrap();
        let guid: String = conn
            .query_row("SELECT sync_guid FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(guid.len(), 32, "ai 触发器应内联派生 32hex guid");
        let rk: String = conn
            .query_row(
                "SELECT row_key FROM sync_log WHERE tbl='positions' AND op='upsert' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let parsed: Vec<String> = serde_json::from_str(&rk).unwrap();
        assert_eq!(parsed, vec![guid.clone()], "row_key 应为 json_array(sync_guid)");
        // 跨设备身份稳定：同业务行在两台设备各自派生了不同 guid（存量库各自回填的必然结果），
        // 主路径 LWW 应记冲突（不自动合并——影响资产的语义决策须用户裁决），
        // 裁决「采用远端」时按自然键收养远端身份，本地自增 id 不变、行数不增。
        // （真实库 positions 有 (account_id,fund_code,platform) 唯一索引，测试库补齐以贴近真实行为。）
        let src = &conn;
        src.execute_batch(
            "ALTER TABLE positions ADD COLUMN account_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE positions ADD COLUMN platform TEXT NOT NULL DEFAULT '';
             CREATE UNIQUE INDEX uq_pos_test ON positions(account_id, fund_code, platform);",
        )
        .unwrap();
        let payload: String = src
            .query_row(
                "SELECT json_object('sync_guid', sync_guid, 'fund_code', fund_code, 'account_id', account_id, 'platform', platform, 'shares', 200.0, 'updated_at', '2027-01-01 00:00:00.000') FROM positions WHERE fund_code='000001'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        dst.execute_batch(
            "ALTER TABLE positions ADD COLUMN account_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE positions ADD COLUMN platform TEXT NOT NULL DEFAULT '';
             CREATE UNIQUE INDEX uq_pos_test ON positions(account_id, fund_code, platform);",
        )
        .unwrap();
        dst.execute(
            "INSERT INTO positions(fund_code, shares) VALUES('000001', 999.0)",
            [],
        )
        .unwrap();
        let changes = vec![Change {
            tbl: "positions".into(),
            row_key: format!("[\"{guid}\"]"),
            op: "upsert".into(),
            ts: "2027-01-01 00:00:00.000".into(),
            payload: Some(serde_json::from_str(&payload).unwrap()),
        }];
        let (applied, conflicts) = apply_changeset_lww(&dst, &changes, "dev-src").unwrap();
        assert_eq!(
            (applied, conflicts),
            (0, 1),
            "同业务行不同 guid：LWW 应记冲突交用户裁决，不自动合并"
        );
        let n: i64 = dst
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "冲突路径不得新增重复行");
        // 用户裁决「采用远端」→ 按自然键收养远端身份
        let adopted = force_apply_remote(&dst, &changes[0]).unwrap();
        assert_eq!(adopted, 1);
        let n2: i64 = dst
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 1, "收养后仍是一行");
        let shares: f64 = dst
            .query_row("SELECT shares FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(shares, 200.0, "载荷值应覆盖本地");
        let dst_guid: String = dst
            .query_row("SELECT sync_guid FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dst_guid, guid, "本地行应收养远端 guid（身份收敛）");
        // 本地自增 id 不得被远端载荷改写（INTERNAL_LOCAL_COLS 过滤）
        let local_ids: Vec<i64> = {
            let mut stmt = dst.prepare("SELECT id FROM positions ORDER BY id").unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert_eq!(local_ids, vec![1], "本地 id 必须保持不变");
    }

    // ⑫'' position_daily 身份映射回归：同步身份 = 自身 sync_guid，父联动 = position_guid。
    #[test]
    fn position_daily_identity_mapping() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO positions(fund_code, shares) VALUES('000001', 100.0)", [])
            .unwrap();
        let parent_guid: String = src
            .query_row("SELECT sync_guid FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        src.execute(
            "INSERT INTO position_daily(position_id, nav_date, shares) \
             SELECT id, '2026-09-01', 100.0 FROM positions WHERE fund_code='000001'",
            [],
        )
        .unwrap();
        // ai 触发器应派生自身 guid 与父链 guid（两 guid 不同、父链 = 父行 guid）
        let (row_guid, pg): (String, String) = src
            .query_row(
                "SELECT sync_guid, position_guid FROM position_daily WHERE nav_date='2026-09-01'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row_guid.len(), 32, "ai 应派生 32hex 自身 guid");
        assert_eq!(pg, parent_guid, "position_guid 应等于父行 sync_guid");
        // 单次插入只产生 1 条 sync_log（ai 内联派生 UPDATE 不得点燃 au——嵌套触发守卫）
        let n_log: i64 = src
            .query_row("SELECT COUNT(*) FROM sync_log WHERE tbl='position_daily'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_log, 1, "position_daily 插入应恰好记 1 条 upsert");
        // row_key = json_array(sync_guid)
        let rk: String = src
            .query_row(
                "SELECT row_key FROM sync_log WHERE tbl='position_daily' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rk, format!("[\"{row_guid}\"]"));

        // 回放端：空库按序回放（positions 先于 position_daily）→ 子行 INSERT 反解
        // position_id = 本地父行 id；position_id 不被远端值覆盖
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let changes = collect_changeset(&src, "", 0).unwrap();
        let (applied, errors) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(errors, 0, "父行已在同批变更中先落（排序保证），子行不应跳过");
        assert_eq!(applied, changes.len());
        // 子行落库：本地 position_id 反解为 dst 本地父行 id=1，position_guid 收敛为远端父 guid
        let (local_pid, dst_pg): (i64, String) = dst
            .query_row(
                "SELECT position_id, position_guid FROM position_daily WHERE nav_date='2026-09-01'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(local_pid, 1, "position_id 必须反解为本地父行 id");
        assert_eq!(dst_pg, parent_guid, "position_guid 应为远端父 guid（身份收敛）");
        // LWW 同步主键 = 自身 sync_guid：重复回放幂等
        let (applied2, errors2) = apply_changeset(&dst, &changes).unwrap();
        assert_eq!((applied2, errors2), (changes.len(), 0));
        let n_rows: i64 = dst
            .query_row("SELECT COUNT(*) FROM position_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_rows, 1, "幂等回放不得翻倍");
    }

    // ⑫''' position_daily 父行未同步 → 子行跳过（不崩、不计入 applied），父行到位后重放成功。
    #[test]
    fn position_daily_child_waits_for_parent() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO positions(fund_code, shares) VALUES('000001', 100.0)", [])
            .unwrap();
        src.execute(
            "INSERT INTO position_daily(position_id, nav_date, shares) \
             SELECT id, '2026-09-01', 100.0 FROM positions WHERE fund_code='000001'",
            [],
        )
        .unwrap();
        let all = collect_changeset(&src, "", 0).unwrap();
        let child_only: Vec<Change> = all
            .into_iter()
            .filter(|c| c.tbl == "position_daily")
            .collect();
        assert_eq!(child_only.len(), 1);

        // 目标库没有父持仓 → 子行 INSERT 反解不到父 id → 跳过（错误计数 1）
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let (applied, errors) = apply_changeset(&dst, &child_only).unwrap();
        assert_eq!((applied, errors), (0, 1), "父行缺失时子行必须跳过");
        let n: i64 = dst
            .query_row("SELECT COUNT(*) FROM position_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "不得落库半截数据");

        // 父行到位（同步父持仓）→ 重放子行成功
        let parent_changes = vec![{
            let src2 = &src;
            let mut cs = collect_changeset(src2, "", 0).unwrap();
            cs.retain(|c| c.tbl == "positions");
            cs.remove(0)
        }];
        let (pa, pe) = apply_changeset(&dst, std::slice::from_ref(&parent_changes[0])).unwrap();
        assert_eq!((pa, pe), (1, 0));
        let (a2, e2) = apply_changeset(&dst, &child_only).unwrap();
        assert_eq!((a2, e2), (1, 0), "父行到位后子行重放应成功");
        let local_pid: i64 = dst
            .query_row(
                "SELECT position_id FROM position_daily WHERE nav_date='2026-09-01'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let dst_parent_id: i64 = dst
            .query_row("SELECT id FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(local_pid, dst_parent_id, "反解的本地父 id 应与实际父行一致");
    }

    // ⑫'''' 跨设备同持仓同日各写一行（guid 不同、(position_guid, nav_date) 相撞）→ LWW 记冲突。
    #[test]
    fn position_daily_same_day_collision_records_conflict() {
        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        dst.execute("INSERT INTO positions(fund_code, shares) VALUES('000001', 100.0)", [])
            .unwrap();
        let dst_pg: String = dst
            .query_row("SELECT sync_guid FROM positions WHERE fund_code='000001'", [], |r| r.get(0))
            .unwrap();
        dst.execute(
            "INSERT INTO position_daily(position_id, nav_date, shares) \
             SELECT id, '2026-09-01', 100.0 FROM positions WHERE fund_code='000001'",
            [],
        )
        .unwrap();
        // 本地已有一行 2026-09-01（自身 guid）；远端对同一 (position_guid, nav_date)
        // 写了另一行（不同 sync_guid）→ 唯一索引相撞 → 记冲突，不静默顶替。
        let local_row_guid: String = dst
            .query_row("SELECT sync_guid FROM position_daily WHERE nav_date='2026-09-01'", [], |r| r.get(0))
            .unwrap();
        let remote_guid = "cccccccccccccccccccccccccccccccc";
        let ch = Change {
            tbl: "position_daily".into(),
            row_key: format!("[\"{remote_guid}\"]"),
            op: "upsert".into(),
            ts: "2999-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({
                "sync_guid": remote_guid,
                "position_guid": dst_pg,
                "nav_date": "2026-09-01",
                "shares": 88.0
            })),
        };
        let (applied, conflicts) = apply_changeset_lww(&dst, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (0, 1), "同日相撞应记冲突交裁决");
        let shares: f64 = dst
            .query_row(
                "SELECT shares FROM position_daily WHERE sync_guid=?1",
                [local_row_guid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shares, 100.0, "本地行不得被静默顶替");
        // 用户裁决「采用远端」→ 按自然键收养：本地行改写为远端身份与值，行数不变
        let adopted = force_apply_remote(
            &dst,
            &Change {
                tbl: "position_daily".into(),
                row_key: format!("[\"{remote_guid}\"]"),
                op: "upsert".into(),
                ts: "2999-01-01 00:00:00.000".into(),
                payload: Some(serde_json::json!({
                    "sync_guid": remote_guid,
                    "position_guid": dst_pg,
                    "nav_date": "2026-09-01",
                    "shares": 88.0
                })),
            },
        )
        .unwrap();
        assert_eq!(adopted, 1);
        let n: i64 = dst
            .query_row("SELECT COUNT(*) FROM position_daily", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "收养后仍是一行");
        let (shares2, guid2): (f64, String) = dst
            .query_row(
                "SELECT shares, sync_guid FROM position_daily WHERE nav_date='2026-09-01'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(shares2, 88.0);
        assert_eq!(guid2, remote_guid, "本地行应收养远端 guid");
    }

    // ⑫''''' 墓碑级联收敛（Android 拉取 FK 失败实证的回归）：
    // 父行（funds）删除墓碑到达时，对端残留的 RESTRICT 引用子行（transactions，且其
    // 删除墓碑已被源端清除、永远等不到）必须随父行一并收敛，否则 DELETE 当场报错
    // （RESTRICT）或拖到 defer_foreign_keys 的 COMMIT 才爆（NO ACTION）。
    // SET NULL 引用（related_tx_id 配对流水）必须交给引擎置空，绝不级联误删。
    #[test]
    fn tombstone_delete_cascades_restrict_children() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        // 把 transactions 改造成生产 FK 形状：RESTRICT 引用 funds + SET NULL 自引用
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             DROP TABLE transactions;
             CREATE TABLE transactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE RESTRICT,
                related_tx_id INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
                amount REAL NOT NULL);",
        )
        .unwrap();
        crate::db::init_sync_schema(&conn).unwrap();

        conn.execute("INSERT INTO funds(code,name,platform) VALUES('A','a','alipay')", [])
            .unwrap();
        conn.execute("INSERT INTO funds(code,name,platform) VALUES('B','b','alipay')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO transactions(fund_code,amount) VALUES('A',10.0)",
            [],
        )
        .unwrap();
        let t2_id = {
            conn.execute(
                "INSERT INTO transactions(fund_code,amount) VALUES('A',20.0)",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        conn.execute(
            "INSERT INTO transactions(fund_code,amount,related_tx_id) VALUES('B',30.0,?1)",
            [t2_id],
        )
        .unwrap();
        let n_txn: i64 = conn
            .query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n_txn, 3);

        // funds A 删除墓碑到达：A 的两条 RESTRICT 子流水必须级联收敛，B 的流水（related_tx_id
        // 指向 A 的流水，SET NULL）必须存活且被引擎置空
        let ch = Change {
            tbl: "funds".into(),
            row_key: "[\"A\"]".into(),
            op: "delete".into(),
            ts: "2999-01-01 00:00:00.000".into(),
            payload: None,
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (1, 0), "父行墓碑应成功应用");

        let fa: i64 = conn
            .query_row("SELECT COUNT(*) FROM funds WHERE code='A'", [], |r| r.get(0))
            .unwrap();
        let fb: i64 = conn
            .query_row("SELECT COUNT(*) FROM funds WHERE code='B'", [], |r| r.get(0))
            .unwrap();
        assert_eq!((fa, fb), (0, 1), "A 删除、B 存活");
        let txn_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions WHERE fund_code='A'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(txn_a, 0, "A 的 RESTRICT 子流水应被级联收敛");
        let (txn_b, rel): (i64, Option<i64>) = conn
            .query_row(
                "SELECT COUNT(*), (SELECT related_tx_id FROM transactions WHERE fund_code='B') FROM transactions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(txn_b, 1, "B 的流水不得被误删");
        assert_eq!(rel, None, "SET NULL 配对引用应交给引擎置空，而非级联误删");
    }

    // ⑫'''''' 两段式回放排序：delete 墓碑必须子表优先——先删 transactions 再删 funds，
    // 否则 RESTRICT 引用卡住父行删除（upsert 仍父表优先，二者方向相反）。
    #[test]
    fn two_phase_order_deletes_child_first() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             DROP TABLE transactions;
             CREATE TABLE transactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE RESTRICT,
                amount REAL NOT NULL);",
        )
        .unwrap();
        crate::db::init_sync_schema(&conn).unwrap();
        conn.execute("INSERT INTO funds(code,name,platform) VALUES('A','a','alipay')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO transactions(fund_code,amount) VALUES('A',10.0)",
            [],
        )
        .unwrap();
        let txn_guid: String = conn
            .query_row("SELECT sync_guid FROM transactions LIMIT 1", [], |r| r.get(0))
            .unwrap();

        // 变更集同时含父表（funds）与子表（transactions）删除墓碑
        let changes = vec![
            Change {
                tbl: "funds".into(),
                row_key: "[\"A\"]".into(),
                op: "delete".into(),
                ts: "2999-01-01 00:00:00.000".into(),
                payload: None,
            },
            Change {
                tbl: "transactions".into(),
                row_key: format!("[\"{txn_guid}\"]"),
                op: "delete".into(),
                ts: "2999-01-01 00:00:00.000".into(),
                payload: None,
            },
        ];
        let (applied, errors) = apply_changeset(&conn, &changes).unwrap();
        assert_eq!((applied, errors), (2, 0), "子表墓碑先应用，父行删除不卡 FK");
        let n: i64 = conn
            .query_row("SELECT (SELECT COUNT(*) FROM funds) + (SELECT COUNT(*) FROM transactions)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "父行与子行都应被删除");
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

    // ⑧ D2：跨设备墓碑按业务身份删对行——TEXT 主键(funds) 与 sync_guid 身份(GUIDED 表 positions)。
    //    （position_daily 已移出同步集合，其复合主键墓碑场景随身份改造一并退场。）
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

        // positions：GUIDED 表（sync_guid 身份）——先同步两行建立远端身份，再重放含墓碑的
        // 完整变更集，验证墓碑按 sync_guid 精确删除目标行。
        let src2 = Connection::open_in_memory().unwrap();
        setup(&src2);
        src2.execute("INSERT INTO positions(fund_code,shares) VALUES('A',10)", [])
            .unwrap();
        src2.execute("INSERT INTO positions(fund_code,shares) VALUES('B',20)", [])
            .unwrap();
        src2.execute("DELETE FROM positions WHERE fund_code='A'", [])
            .unwrap();
        let changes2 = collect_changeset(&src2, "", 0).unwrap();
        assert_eq!(changes2.len(), 3, "两行插入 + 一条墓碑");

        let dst2 = Connection::open_in_memory().unwrap();
        setup(&dst2);
        let (n, e) = apply_changeset(&dst2, &changes2).unwrap();
        assert_eq!(n, 3);
        assert_eq!(e, 0);
        let cnt_a2: i64 = dst2
            .query_row("SELECT COUNT(*) FROM positions WHERE fund_code='A'", [], |r| r.get(0))
            .unwrap();
        let cnt_b2: i64 = dst2
            .query_row("SELECT COUNT(*) FROM positions WHERE fund_code='B'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt_a2, 0, "A 应被 sync_guid 墓碑删对行");
        assert_eq!(cnt_b2, 1, "B 不应被误删");
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
        // 未给 env 时**跳过**而非 panic：该用例已被 #[ignore] 排除在默认门禁之外，
        // 批量跑 `--ignored`（如做端到端核验）时不该因为少一个环境变量而整批变红。
        let Ok(path) = std::env::var("FUNDLENS_REAL_DB") else {
            eprintln!("跳过 migrate_real_db_copy：未设置 FUNDLENS_REAL_DB（指向真实库副本）");
            return;
        };
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
    // 真实库的坑（身份改造前）：业务表有自然键唯一索引（如 positions 的
    // account_id+fund_code+platform），跨设备各自新建「逻辑上同一条」记录 → guid 不同、
    // 自然键相同。**用户显式裁决「采用远端」**时，按自然键「收养远端身份」：覆盖本地行内容、
    // 把 sync_guid 改写为远端 guid，本地自增 id 不动（旧实现直接 INSERT 会撞唯一索引死结）。
    // 用 platform_templates（platform 列有 UNIQUE）复现同一形状。
    #[test]
    fn adopt_remote_with_natural_key_collision_adopts_local_row() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute(
            "INSERT INTO platform_templates(platform,ocr_rules) VALUES('alipay','本地规则')",
            [],
        )
        .unwrap();
        let local_id: i64 = conn
            .query_row("SELECT id FROM platform_templates WHERE platform='alipay'", [], |r| r.get(0))
            .unwrap();
        let remote_guid = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let payload = serde_json::json!({
            "sync_guid": remote_guid, "platform": "alipay", "ocr_rules": "远端规则"
        })
        .to_string();
        let id = insert_conflict_raw(&conn, "platform_templates", &format!("[\"{remote_guid}\"]"), &payload);

        let (found, n) = resolve_conflict(&conn, id, true).unwrap();
        assert!(found, "冲突应存在");
        assert_eq!(n, 1, "收养远端身份应成功写回 1 行");

        // 收敛结果：仍是一行、内容为远端、身份（sync_guid）收养远端、本地自增 id 不变。
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM platform_templates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 1, "收养不得新增重复行");
        let (rules, guid, new_id): (String, String, i64) = conn
            .query_row(
                "SELECT ocr_rules, sync_guid, id FROM platform_templates WHERE platform='alipay'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(rules, "远端规则");
        assert_eq!(guid, remote_guid, "本地行应收养远端 guid（身份收敛）");
        assert_eq!(new_id, local_id, "本地自增 id 必须保持不变");
        assert_eq!(conflict_resolved(&conn, id), 1, "裁决后冲突应标记已解");
    }

    // 【选 A 的核心】主键未命中、但自然键撞上另一条本地行的远端变更，
    // 不得走 INSERT OR REPLACE（那会静默删掉本地行并级联抹掉子表），而应记冲突交给用户裁决。
    #[test]
    fn colliding_upsert_records_conflict_and_preserves_local_row() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute(
            "INSERT INTO platform_templates(id,platform,ocr_rules) VALUES(1,'alipay','本地规则')",
            [],
        )
        .unwrap();

        // 远端用另一个 id 表达同一个 platform（自然键相同），ts 比本地新 → 会走到「正常应用」分支
        let ch = Change {
            tbl: "platform_templates".to_string(),
            row_key: "[\"2\"]".to_string(),
            op: "upsert".to_string(),
            ts: "2999-01-01 00:00:00.000".to_string(),
            payload: Some(
                serde_json::json!({"id": 2, "platform": "alipay", "ocr_rules": "远端规则"}),
            ),
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (0, 1), "撞自然键应记冲突而非应用");

        let rules: String = conn
            .query_row(
                "SELECT ocr_rules FROM platform_templates WHERE platform='alipay'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rules, "本地规则", "本地行不得被静默删改");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM platform_templates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "不得因 REPLACE 把本地行替换成远端行");

        // 详情里应前置给出「无法采用远端」的原因，UI 才能直接禁用该操作
        let id: i64 = conn
            .query_row("SELECT MAX(id) FROM sync_conflicts", [], |r| r.get(0))
            .unwrap();
        let d = conflict_detail(&conn, id).unwrap().unwrap();
        assert_eq!(d.tbl, "platform_templates");
        assert!(!d.local_exists, "本地没有远端那个 sync_guid 的行");
        let reason = d.blocked_reason.expect("应给出无法采用远端的原因");
        assert!(reason.contains("指向同一条业务记录"), "原因应说明自然键相撞: {reason}");
        // 原因应点出撞上的本地记录身份（sync_guid），UI 才能定位到具体行
        let local_guid: String = conn
            .query_row(
                "SELECT sync_guid FROM platform_templates WHERE platform='alipay'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(reason.contains(&local_guid), "原因应点出撞上的本地记录身份: {reason}");
    }

    // 按自身身份（sync_guid）正常更新，不得被误判为「自然键相撞」。
    #[test]
    fn self_update_is_not_flagged_as_collision() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute(
            "INSERT INTO platform_templates(platform,ocr_rules) VALUES('alipay','旧规则')",
            [],
        )
        .unwrap();
        let guid: String = conn
            .query_row(
                "SELECT sync_guid FROM platform_templates WHERE platform='alipay'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let ch = Change {
            tbl: "platform_templates".to_string(),
            row_key: format!("[\"{guid}\"]"),
            op: "upsert".to_string(),
            ts: "2999-01-01 00:00:00.000".to_string(),
            payload: Some(serde_json::json!({
                "sync_guid": guid, "platform": "alipay", "ocr_rules": "新规则"
            })),
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (1, 0), "按自身 sync_guid 更新应正常应用");
        let rules: String = conn
            .query_row(
                "SELECT ocr_rules FROM platform_templates WHERE platform='alipay'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rules, "新规则");
    }

    // 新记录（本地既无该身份、也无同自然键的行）照常插入，不被误判。
    #[test]
    fn brand_new_row_is_applied_not_flagged() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        // 全新 32hex 身份（模拟远端派生的 guid）
        let guid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1".to_string();
        let ch = Change {
            tbl: "platform_templates".to_string(),
            row_key: format!("[\"{guid}\"]"),
            op: "upsert".to_string(),
            ts: "2999-01-01 00:00:00.000".to_string(),
            payload: Some(serde_json::json!({
                "sync_guid": guid, "platform": "jd", "ocr_rules": "新平台"
            })),
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (1, 0), "全新记录应正常应用");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM platform_templates", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    /// 把 positions 改造成与生产一致的形状：自然键 `(account_id, fund_code, platform)` 上建唯一索引，
    /// 且其中 `platform` 是「有默认值、因此可被载荷合法省略」的列。position_daily 重建为
    /// `ON DELETE CASCADE`（SQLite 不能给既有表补 FK，只能重建）。
    fn with_production_like_positions(conn: &Connection) {
        setup(conn);
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             ALTER TABLE positions ADD COLUMN account_id INTEGER NOT NULL DEFAULT 1;
             ALTER TABLE positions ADD COLUMN platform TEXT NOT NULL DEFAULT '';
             CREATE UNIQUE INDEX uq_positions_account_fund_platform
                 ON positions(account_id, fund_code, platform);
             DROP TABLE position_daily;
             CREATE TABLE position_daily (
                 position_id INTEGER NOT NULL REFERENCES positions(id) ON DELETE CASCADE,
                 nav_date TEXT NOT NULL,
                 shares REAL NOT NULL,
                 PRIMARY KEY(position_id, nav_date));",
        )
        .unwrap();
        crate::db::init_sync_schema(conn).unwrap();
    }

    // 独立验证发现的假阴性（命题 7）：载荷**省略**某个自然键列时，早期实现直接跳过该索引，
    // 于是漏判相撞 → 仍走 REPLACE → 静默删本地行。修正后按「该列会落成的默认值」代入比对。
    #[test]
    fn collision_is_detected_even_when_payload_omits_a_natural_key_column() {
        let conn = Connection::open_in_memory().unwrap();
        with_production_like_positions(&conn);
        conn.execute(
            "INSERT INTO positions(fund_code,shares,account_id,platform) VALUES('MIS',10,3,'')",
            [],
        )
        .unwrap();

        // 远端载荷缺 platform（该列默认 ''）→ 新行会以 '' 落库，照样撞上本地行 → 必须检出
        let lacking = serde_json::json!({"id": 99000258, "fund_code": "MIS", "shares": 20, "account_id": 3})
            .as_object()
            .cloned()
            .unwrap();
        let hit = natural_key_collision(&conn, "positions", &lacking)
            .unwrap()
            .expect("载荷缺 platform 时也必须检出相撞（否则仍会静默删数据）");
        // 身份改造后冲突按业务身份（sync_guid）定位本地行，而非本地自增 id
        let local_guid: String = conn
            .query_row("SELECT sync_guid FROM positions WHERE fund_code='MIS'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hit, vec![serde_json::json!(local_guid)]);

        // 载荷带上了 platform 且取值不同 → 落库后不撞唯一索引，不算相撞（不得误报）
        let differing = serde_json::json!({
            "id": 99000258, "fund_code": "MIS", "shares": 20, "account_id": 3, "platform": "alipay"
        })
        .as_object()
        .cloned()
        .unwrap();
        assert!(
            natural_key_collision(&conn, "positions", &differing).unwrap().is_none(),
            "自然键不同不应判为相撞"
        );
    }

    // 端到端：载荷缺自然键列导致的相撞，本地行与其 position_daily 子记录都必须原样存活。
    #[test]
    fn colliding_upsert_with_omitted_key_column_preserves_local_row_and_children() {
        let conn = Connection::open_in_memory().unwrap();
        with_production_like_positions(&conn);
        conn.execute(
            "INSERT INTO positions(fund_code,shares,account_id,platform) VALUES('MIS',10,3,'')",
            [],
        )
        .unwrap();
        let local_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO position_daily(position_id,nav_date,shares) VALUES(?1,'2026-09-01',10),\
             (?1,'2026-09-02',10),(?1,'2026-09-03',10)",
            rusqlite::params![local_id],
        )
        .unwrap();

        // 远端同自然键、不同 id，且**省略 platform**
        let ch = Change {
            tbl: "positions".to_string(),
            row_key: "[\"99000258\"]".to_string(),
            op: "upsert".to_string(),
            ts: "2999-01-01 00:00:00.000".to_string(),
            payload: Some(
                serde_json::json!({"id": 99000258, "fund_code": "MIS", "shares": 20, "account_id": 3}),
            ),
        };
        let (applied, conflicts) = apply_changeset_lww(&conn, &[ch], "devB").unwrap();
        assert_eq!((applied, conflicts), (0, 1), "应记冲突而非 REPLACE 应用");

        let alive: i64 = conn
            .query_row("SELECT COUNT(*) FROM positions WHERE id=?1", [local_id], |r| r.get(0))
            .unwrap();
        assert_eq!(alive, 1, "本地持仓行不得被静默删除");
        let children: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM position_daily WHERE position_id=?1",
                [local_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(children, 3, "position_daily 子记录不得被级联抹掉（ON DELETE CASCADE）");
    }

    // 同一来源对同一行的被拒变更重复回放，只应留下一条未解冲突，且保留最新一次远端意图。
    #[test]
    fn repeated_rejected_change_keeps_one_unresolved_conflict_with_latest_payload() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        conn.execute("INSERT INTO funds(code,name,platform) VALUES('F1','local','alipay')", [])
            .unwrap();
        // 把本地时间推到未来 → 远端变更一律被判为更旧，走冲突分支
        conn.execute("UPDATE funds SET updated_at='2099-01-01 00:00:00.000' WHERE code='F1'", [])
            .unwrap();

        let mk = |name: &str| Change {
            tbl: "funds".to_string(),
            row_key: "[\"F1\"]".to_string(),
            op: "upsert".to_string(),
            ts: "2026-01-01 00:00:00.000".to_string(),
            payload: Some(serde_json::json!({"code": "F1", "name": name, "platform": "alipay"})),
        };
        apply_changeset_lww(&conn, &[mk("远端旧")], "devB").unwrap();
        apply_changeset_lww(&conn, &[mk("远端新")], "devB").unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_conflicts WHERE tbl='funds' AND row_key='[\"F1\"]' \
                 AND device='devB' AND resolved=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "重复回放不应堆出多条未解冲突");
        let payload: String = conn
            .query_row(
                "SELECT payload FROM sync_conflicts WHERE tbl='funds' AND resolved=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(payload.contains("远端新"), "应保留最新一次远端意图: {payload}");
    }
}
