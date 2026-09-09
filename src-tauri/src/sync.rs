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

/// 一条变更记录。
/// - `op` = "upsert"（行当前存在）或 "delete"（墓碑，行已被删）。
/// - `ts` = 该变更在源端发生的时刻（来自 sync_log.ts）。LWW 回放（apply_changeset_lww）
///   据此与目标行 updated_at 比较，决定应用或记冲突。注意：设计文档的 Change 仅列
///   {tbl, row_id, op, payload}，此处额外携带 ts 是 LWW 必需的，且 collect_changeset
///   本就从 sync_log 取到该值，不引入额外来源。
/// - `payload` = upsert 时该行的当前快照（serde_json::Value::Object）；delete 时为 None。
#[derive(Debug, Clone)]
pub struct Change {
    pub tbl: String,
    pub row_id: i64,
    pub op: String,
    pub ts: String,
    pub payload: Option<Value>,
}

/// 读取某行当前快照，序列化为 serde_json::Value::Object。行不存在返回 None。
/// 列名来自 PRAGMA table_info（表自身元信息，非外部输入）→ 安全。
fn select_row_json(conn: &Connection, tbl: &str, row_id: i64) -> SqlResult<Option<Value>> {
    let cols = synced_columns(conn, tbl)?;
    if cols.is_empty() {
        return Ok(None);
    }
    let col_list = cols.join(",");
    let sql = format!("SELECT {col_list} FROM {tbl} WHERE rowid=?");
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params![row_id])?;
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
fn apply_one_upsert(conn: &Connection, ch: &Change) -> SqlResult<()> {
    let map = match &ch.payload {
        Some(Value::Object(m)) if !m.is_empty() => m,
        _ => return Ok(()), // 无有效 payload 则不应用
    };
    let cols: Vec<&String> = map.keys().collect();
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
    Ok(())
}

/// 收集 after_ts 之后的变更集（按 ts 升序）。
///
/// - 白名单过滤：sync_log.tbl 不在 SYNCED_TABLES 的一律跳过（防 sync_log 被篡改导致注入）。
/// - op=upsert：读取该行当前快照进 payload；若行已被删（如 upsert 后又 delete），
///   则降级为 delete 墓碑（行已不在，无法再取 payload）。
/// - op=delete：保持墓碑，payload=None。
pub fn collect_changeset(conn: &Connection, after_ts: &str) -> SqlResult<Vec<Change>> {
    let mut stmt = conn.prepare(
        "SELECT id, tbl, row_id, op, ts FROM sync_log \
         WHERE ts > ?1 ORDER BY ts ASC, id ASC",
    )?;
    let mut log_rows = stmt.query(rusqlite::params![after_ts])?;
    let mut entries: Vec<(String, i64, String, String)> = Vec::new();
    while let Some(r) = log_rows.next()? {
        // id 已用于排序，丢弃；保留 (tbl, row_id, op, ts)
        let _id: i64 = r.get(0)?;
        let tbl: String = r.get(1)?;
        let row_id: i64 = r.get(2)?;
        let op: String = r.get(3)?;
        let ts: String = r.get(4)?;
        if !is_synced_table(&tbl) {
            continue;
        }
        entries.push((tbl, row_id, op, ts));
    }

    let mut out = Vec::new();
    for (tbl, row_id, op, ts) in entries {
        match op.as_str() {
            "upsert" => match select_row_json(conn, &tbl, row_id)? {
                Some(payload) => out.push(Change {
                    tbl,
                    row_id,
                    op: "upsert".into(),
                    ts,
                    payload: Some(payload),
                }),
                None => out.push(Change {
                    tbl,
                    row_id,
                    op: "delete".into(),
                    ts,
                    payload: None,
                }),
            },
            "delete" => out.push(Change {
                tbl,
                row_id,
                op: "delete".into(),
                ts,
                payload: None,
            }),
            _ => {}
        }
    }
    Ok(out)
}

/// 幂等回放变更集（无冲突处理，直接覆盖）。
/// - upsert：按 payload 列清单 INSERT OR REPLACE。
/// - delete：DELETE FROM <tbl> WHERE rowid=?（行不存在则无操作，仍计入 applied）。
/// 返回应用的变更条数（非受影响行数）。
pub fn apply_changeset(conn: &Connection, changes: &[Change]) -> SqlResult<usize> {
    conn.execute_batch("PRAGMA recursive_triggers = OFF;")?;
    let mut applied = 0;
    for ch in changes {
        if !is_synced_table(&ch.tbl) {
            continue;
        }
        match ch.op.as_str() {
            "upsert" => {
                apply_one_upsert(conn, ch)?;
                applied += 1;
            }
            "delete" => {
                conn.execute(
                    &format!("DELETE FROM {} WHERE rowid=?", ch.tbl),
                    rusqlite::params![ch.row_id],
                )?;
                applied += 1;
            }
            _ => {}
        }
    }
    Ok(applied)
}

/// LWW（Last-Write-Wins）变体回放。
///
/// 应用前比较目标行 updated_at 与变更 ts：
/// - 目标行 updated_at 比变更 ts 新（远端是更旧的变更）→ 跳过，并向 sync_conflicts 记一条
///   （resolved=0, device=来源设备, payload=变更快照），返回计数 (applied, conflicts)。
/// - 否则正常应用（upsert / delete）。
///
/// 注意：本函数只实现单设备对单变更的 LWW 判定；多设备汇合、冲突人工/自动解算在后续阶段处理。
pub fn apply_changeset_lww(
    conn: &Connection,
    changes: &[Change],
    device: &str,
) -> SqlResult<(usize, usize)> {
    conn.execute_batch("PRAGMA recursive_triggers = OFF;")?;
    let mut applied = 0usize;
    let mut conflicts = 0usize;
    for ch in changes {
        if !is_synced_table(&ch.tbl) {
            continue;
        }
        // 目标当前 updated_at
        let target_ts: Option<String> = conn
            .query_row(
                &format!("SELECT updated_at FROM {} WHERE rowid=?", ch.tbl),
                rusqlite::params![ch.row_id],
                |r| r.get(0),
            )
            .ok();
        let target_newer = match target_ts {
            Some(ref t) if !t.is_empty() => *t > ch.ts, // 同格式（YYYY-MM-DD HH:MM:SS.fff）字典序可比
            _ => false,                                 // 目标无行 / updated_at 为空 → 不冲突
        };
        if target_newer {
            let payload_str = ch.payload.as_ref().map(|v| v.to_string()).unwrap_or_default();
            conn.execute(
                "INSERT INTO sync_conflicts(tbl, row_id, device, payload, resolved, created_at) \
                 VALUES(?1, ?2, ?3, ?4, 0, strftime('%Y-%m-%d %H:%M:%f','now'))",
                rusqlite::params![ch.tbl, ch.row_id, device, payload_str],
            )?;
            conflicts += 1;
        } else {
            match ch.op.as_str() {
                "upsert" => {
                    apply_one_upsert(conn, ch)?;
                    applied += 1;
                }
                "delete" => {
                    conn.execute(
                        &format!("DELETE FROM {} WHERE rowid=?", ch.tbl),
                        rusqlite::params![ch.row_id],
                    )?;
                    applied += 1;
                }
                _ => {}
            }
        }
    }
    Ok((applied, conflicts))
}

/// 全量导出（首次同步基线）：逐参与表 SELECT * 全行，输出 upsert Change（ts 留空）。
pub fn baseline_export(conn: &Connection) -> SqlResult<Vec<Change>> {
    let mut out = Vec::new();
    for tbl in SYNCED_TABLES {
        let mut stmt = conn.prepare(&format!("SELECT rowid FROM {tbl}"))?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let rid: i64 = r.get(0)?;
            if let Some(payload) = select_row_json(conn, tbl, rid)? {
                out.push(Change {
                    tbl: (*tbl).to_string(),
                    row_id: rid,
                    op: "upsert".into(),
                    ts: String::new(),
                    payload: Some(payload),
                });
            }
        }
    }
    Ok(out)
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

    // ① 触发器生效：insert/update 参与表 → sync_log 出现对应 upsert 且 updated_at 被填；
    //    delete → delete 墓碑。
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

    // ② collect_changeset 按 watermark 过滤、upsert 行带完整 payload。
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

        let all = collect_changeset(&conn, "").unwrap();
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

        // watermark = 最新变更 ts → 之后无变更
        let max_ts: String = conn
            .query_row("SELECT MAX(ts) FROM sync_log", [], |r| r.get(0))
            .unwrap();
        let after_max = collect_changeset(&conn, &max_ts).unwrap();
        assert_eq!(after_max.len(), 0, "watermark 之后的变更应为空");

        // watermark = 第一条变更 ts → 其自身（等于）被排除，仅剩更晚的
        let first_ts: String = conn
            .query_row("SELECT ts FROM sync_log ORDER BY id ASC LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let after_first = collect_changeset(&conn, &first_ts).unwrap();
        assert!(
            after_first.iter().all(|c| c.ts > first_ts),
            "watermark 之后的变更 ts 必须严格大于 watermark"
        );
        assert_eq!(after_first.len(), 1);
    }

    // ③ apply_changeset 幂等：同一 changeset 应用两遍结果一致（行数/内容）。
    #[test]
    fn apply_is_idempotent() {
        let src = Connection::open_in_memory().unwrap();
        setup(&src);
        src.execute("INSERT INTO settings(key,value) VALUES('a','1')", [])
            .unwrap();
        src.execute("INSERT INTO settings(key,value) VALUES('b','2')", [])
            .unwrap();
        let changes = collect_changeset(&src, "").unwrap();

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
        let n1 = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n1, 2);
        let snap1 = dump(&dst);

        let n2 = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n2, 2, "第二遍应用条数应一致");
        let snap2 = dump(&dst);
        assert_eq!(snap1, snap2, "两遍应用后数据内容必须一致");
    }

    // ④ LWW：旧变更被跳过并写 sync_conflicts；新变更正常应用。
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

        // 旧变更（ts 在过去）针对已存在行 X → 冲突、跳过
        let stale = Change {
            tbl: "funds".into(),
            row_id: 1,
            op: "upsert".into(),
            ts: "2000-01-01 00:00:00.000".into(),
            payload: Some(serde_json::json!({
                "code": "X", "name": "stale", "platform": "alipay", "updated_at": ""
            })),
        };
        // 新变更（目标无此行 Y）→ 正常应用
        let fresh = Change {
            tbl: "funds".into(),
            row_id: 2,
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
            .query_row("SELECT COUNT(*) FROM sync_conflicts WHERE resolved=0", [], |r| r.get(0))
            .unwrap();
        assert_eq!(nc, 1, "应记录 1 条未解冲突");
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
        let changes = collect_changeset(&src, "").unwrap();
        assert_eq!(
            changes.iter().filter(|c| c.op == "delete").count(),
            2,
            "应含 2 条 delete（insert 行已删降级 + 显式 delete）"
        );

        let dst = Connection::open_in_memory().unwrap();
        setup(&dst);
        let n = apply_changeset(&dst, &changes).unwrap();
        assert_eq!(n, changes.len());

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
        let n = apply_changeset(&dst, &baseline).unwrap();
        assert_eq!(n, baseline.len());

        let cnt_funds: i64 = dst
            .query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0))
            .unwrap();
        let cnt_acc: i64 = dst
            .query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt_funds, 1);
        assert_eq!(cnt_acc, 1);
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
        // 连跑两遍，验证幂等（列已存在跳过、触发器 DROP+CREATE 不报错）
        crate::db::init_sync_schema(&conn).unwrap();
        crate::db::init_sync_schema(&conn).unwrap();

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
