//! 维护/验证工具：把一份快照回放到一个库上，并报告结果。
//!
//! 用途：**在不碰生产库、不碰远端**的前提下，预演「立即拉取」会发生什么。
//! 走应用自身的回放路径 `sync::apply_changeset_lww`，因此结论与点按钮一致。
//!
//! 用法：
//! ```sh
//! cargo build --example replay_snapshot
//! ./target/debug/examples/replay_snapshot <db> <snapshot.json> [--table transactions] [--device NAME]
//! ```
//! `<snapshot.json>` 可以是
//! ① `cloud::push` 落库的 JSONL 正文（每行一个 Change），或
//! ② 从 CloudBase postgREST 直接 curl 下来的 `[{"body":"..."}]` 包装体（本工具自动剥壳）。
//!
//! **写库范围**：只写传入的那个 `<db>`（应当是一份副本）。不联网、不动 sync_meta 的水位。

use fundlens_lib::sync::{self, Change};

fn extract_body(raw: &str) -> String {
    let t = raw.trim_start();
    if !t.starts_with('[') {
        return raw.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v
            .get(0)
            .and_then(|o| o.get("body"))
            .and_then(|b| b.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| raw.to_string()),
        Err(_) => raw.to_string(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("用法: replay_snapshot <db> <snapshot.json> [--table T] [--device NAME]");
        std::process::exit(2);
    }
    let db = &args[0];
    let snap = &args[1];
    let only_table = args
        .iter()
        .position(|a| a == "--table")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let device = args
        .iter()
        .position(|a| a == "--device")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "dev-rehearsal".to_string());

    let raw = std::fs::read_to_string(snap).expect("读取快照失败");
    let body = extract_body(&raw);
    let mut lines = body.lines();
    let header = lines.next().unwrap_or_default();
    println!("[快照头] {header}");

    let mut changes: Vec<Change> = Vec::new();
    for l in body.lines().skip(1) {
        let l = l.trim();
        if l.is_empty() {
            continue;
        }
        match serde_json::from_str::<Change>(l) {
            Ok(c) => changes.push(c),
            Err(_) => continue,
        }
    }
    let total = changes.len();
    if let Some(t) = &only_table {
        changes.retain(|c| &c.tbl == t);
    }
    println!(
        "[快照] 解析 {total} 条，本次回放 {} 条{}",
        changes.len(),
        only_table.map(|t| format!("（仅 {t}）")).unwrap_or_default()
    );

    let conn = rusqlite::Connection::open(db).expect("打开库失败");
    let count = |c: &rusqlite::Connection| -> i64 {
        c.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))
            .unwrap_or(-1)
    };
    let before = count(&conn);
    let conflicts_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_conflicts WHERE resolved=0", [], |r| r.get(0))
        .unwrap_or(-1);
    println!("[回放前] transactions={before}  未解冲突={conflicts_before}");

    let (applied, conflicts) =
        sync::apply_changeset_lww(&conn, &changes, &device).expect("回放失败");

    let after = count(&conn);
    let conflicts_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_conflicts WHERE resolved=0", [], |r| r.get(0))
        .unwrap_or(-1);
    println!("[回放] applied={applied} conflicts={conflicts}");
    println!("[回放后] transactions={after}  未解冲突={conflicts_after}");
    println!(
        "[净变化] transactions {before} → {after} （{}）",
        after - before
    );
}
