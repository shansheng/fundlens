//! 维护/验证工具：对一份**库副本**跑一遍生产启动迁移（`db::init_db`），并前后对比关键计数。
//!
//! 用途：**在不碰生产库、不联网**的前提下，预演「装上新版后首次启动」会对数据做什么
//! （账户身份归一 / 派生列回填 / 索引重建），确认行数守恒、外键完好、没有意外删除。
//!
//! 用法：
//! ```sh
//! cargo build --example init_migrate
//! FUNDLENS_DATA_DIR=/tmp/somewhere ./target/debug/examples/init_migrate [--expect-accounts N]
//! ```
//! 约定：`<FUNDLENS_DATA_DIR>/fundlens.db` 必须**已经**是你要预演的那份副本
//! （自己先 `cp` 好）。本工具只写这个文件。
//!
//! 输出：迁移前后的 accounts / positions / transactions / position_daily 行数，
//! 账户身份分组，account_guid 覆盖率，`PRAGMA integrity_check` / `foreign_key_check` 结果。

use rusqlite::Connection;
use std::path::PathBuf;

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap_or(-1)
}

fn snapshot(c: &Connection) -> Vec<(&'static str, i64)> {
    vec![
        (
            "accounts",
            count(c, "SELECT COUNT(*) FROM accounts"),
        ),
        ("positions", count(c, "SELECT COUNT(*) FROM positions")),
        (
            "transactions",
            count(c, "SELECT COUNT(*) FROM transactions"),
        ),
        (
            "position_daily",
            count(c, "SELECT COUNT(*) FROM position_daily"),
        ),
        (
            "snapshots",
            count(c, "SELECT COUNT(*) FROM snapshots"),
        ),
    ]
}

fn report(label: &str, s: &[(&str, i64)]) {
    let joined = s
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("[{label}] {joined}");
}

fn main() {
    let dir = std::env::var("FUNDLENS_DATA_DIR").unwrap_or_else(|_| {
        eprintln!("必须设置 FUNDLENS_DATA_DIR（指向含 fundlens.db 的目录）");
        std::process::exit(2);
    });
    let db: PathBuf = std::path::PathBuf::from(&dir).join("fundlens.db");
    if !db.is_file() {
        eprintln!("找不到库文件：{}", db.display());
        std::process::exit(2);
    }
    println!("== 预演库：{} ==", db.display());

    // 迁移前：只读打开取基线（不触发任何写）
    let before = {
        let c = Connection::open(&db).expect("打开库失败");
        let s = snapshot(&c);
        report("迁移前", &s);
        let groups: Vec<String> = {
            let mut stmt = c
                .prepare(
                    "SELECT name, COALESCE(note,''), COALESCE(created_at,''), COUNT(*), MIN(id) \
                     FROM accounts GROUP BY 1,2,3 ORDER BY 5",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok(format!(
                        "name={} note={} created_at={} n={} min_id={}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?
                    ))
                })
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for g in &groups {
            println!("[迁移前-账户身份] {g}");
        }
        s
    };

    // 跑生产迁移
    fundlens_lib::db::init_db(None).expect("init_db 失败（迁移未跑通）");

    // 迁移后：复查（连接由 init_db 持有；此处另开只读连接读同一文件）
    let after = {
        let c = Connection::open(&db).expect("重新打开库失败");
        let s = snapshot(&c);
        report("迁移后", &s);
        let groups: Vec<String> = {
            let mut stmt = c
                .prepare(
                    "SELECT name, COALESCE(note,''), COALESCE(created_at,''), COUNT(*), MIN(id) \
                     FROM accounts GROUP BY 1,2,3 ORDER BY 5",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |r| {
                    Ok(format!(
                        "name={} note={} created_at={} n={} min_id={}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?
                    ))
                })
                .unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        for g in &groups {
            println!("[迁移后-账户身份] {g}");
        }
        // account_guid 覆盖率（列存在才有意义）
        let has_guid: bool = c
            .query_row(
                "SELECT 1 FROM pragma_table_info('positions') WHERE name='account_guid'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if has_guid {
            for t in ["positions", "transactions"] {
                let total = count(&c, &format!("SELECT COUNT(*) FROM {t}"));
                let nulls = count(
                    &c,
                    &format!("SELECT COUNT(*) FROM {t} WHERE account_guid IS NULL"),
                );
                let covered = total - nulls;
                println!("[{t}] account_guid 已覆盖 {covered}/{total}（未覆盖 {nulls}）");
            }
        }
        let integrity: String = c
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|e| format!("<错误: {e}>"));
        println!("[完整性] integrity_check = {integrity}");
        let fk_bad = count(&c, "SELECT COUNT(*) FROM pragma_foreign_key_check");
        println!("[外键] foreign_key_check 违规行数 = {fk_bad}");
        // 重复账户（同身份多行）应为 0
        let dup_acc = count(
            &c,
            "SELECT COALESCE(SUM(n-1),0) FROM (SELECT COUNT(*) n FROM accounts \
             GROUP BY name, COALESCE(note,''), COALESCE(created_at,'') HAVING COUNT(*)>1)",
        );
        println!("[账户] 同身份重复账户数 = {dup_acc}");
        // 重复持仓（同账户同基金同平台）应为 0
        let dup_pos = count(
            &c,
            "SELECT COALESCE(SUM(n-1),0) FROM (SELECT COUNT(*) n FROM positions \
             GROUP BY account_id, fund_code, platform HAVING COUNT(*)>1)",
        );
        println!("[持仓] 同(账户,基金,平台)重复行数 = {dup_pos}");
        s
    };

    println!("== 前后对比 ==");
    let mut same = true;
    for ((k, b), (_, a)) in before.iter().zip(after.iter()) {
        let flag = if b == a { "OK " } else { "CHANGED" };
        if b != a {
            same = false;
        }
        println!("  {flag} {k}: {b} -> {a}");
    }
    println!(
        "== 结论：{} ==",
        if same {
            "除账户归一外行数完全守恒"
        } else {
            "存在行数变化，请逐项核对（account_guid 派生列回填不应改变行数）"
        }
    );
}
