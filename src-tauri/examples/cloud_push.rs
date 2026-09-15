//! 一次性维护工具：对指定库执行一次「云端推送」，等价于 UI 的
//! 「数据同步 → 云端同步 → 立即推送」。
//!
//! 存在意义：本机没有可点击的 GUI（麒麟无头/自动化场景）时，仍能把本设备快照——
//! 特别是 `sync_log` 里的**删除墓碑**——推到云端。走的正是应用自身的代码路径
//! （`sync::full_device_snapshot` → `cloud::transport_from_config` → `cloud::push`），
//! 因此与点按钮的结果逐字节一致。
//!
//! 用法：
//! ```sh
//! FUNDLENS_DB=/path/to/fundlens.db cargo run --example cloud_push -- --dry-run
//! FUNDLENS_DB=/path/to/fundlens.db cargo run --example cloud_push
//! ```
//! 不设 `FUNDLENS_DB` 时回落到 Linux 默认数据目录。
//!
//! **写库范围**：仅 `sync_meta.cloud_last_push` 一条；不改任何业务行，不删任何数据。

use fundlens_lib::{cloud, sync};

fn default_db_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.local/share/com.fundlens.app/fundlens.db")
}

fn main() {
    let dry = std::env::args().any(|a| a == "--dry-run");
    let path = std::env::var("FUNDLENS_DB").unwrap_or_else(|_| default_db_path());
    println!("[库] {path}");

    let conn = rusqlite::Connection::open(&path).expect("打开库失败（路径或权限问题）");

    let cfg = cloud::load_config(&conn);
    println!(
        "[配置] mode={} endpoint={} token_len={} ready={}",
        cfg.mode,
        cfg.endpoint,
        cfg.token.len(),
        cfg.is_ready()
    );

    // 快照构成统计——推送前必须肉眼确认墓碑数量符合预期。
    let changes = sync::full_device_snapshot(&conn).expect("生成快照失败");
    let upserts = changes.iter().filter(|c| c.op != "delete").count();
    let deletes = changes.iter().filter(|c| c.op == "delete").count();
    let del_txn = changes
        .iter()
        .filter(|c| c.op == "delete" && c.tbl == "transactions")
        .count();
    println!(
        "[快照] 总={} upsert={} 墓碑={}（transactions 墓碑={}）",
        changes.len(),
        upserts,
        deletes,
        del_txn
    );

    if dry {
        println!("[dry-run] 未上传，未改库。");
        return;
    }

    let transport = cloud::transport_from_config(&cfg).expect("构造云通道失败");
    let out = cloud::push(&conn, transport.as_ref()).expect("推送失败");
    println!(
        "[推送] key={} count={} size={} at={}",
        out.key, out.count, out.size, out.at
    );
    let last = sync::read_meta(&conn, cloud::META_LAST_PUSH)
        .ok()
        .flatten()
        .unwrap_or_default();
    println!("[校验] sync_meta.cloud_last_push={last}");
}
