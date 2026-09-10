// FundLens · M4 自动备份（本地滚动整库备份：写库前 + 每日首次启动触发）
//
// 设计（详见仓库根 perf-cloudbase-v2.6.0-design-2026-09-09.md §4.3 M4）：
// - 备份产物 = 既有 `db::export_db_backup` 的**在线一致快照**，落在数据库同级的 `backups/` 目录；
// - 命名 `fundlens-YYYYMMDD-HHMMSS-<tag>.db`（tag: auto / manual / pre-import）→ 按文件名排序即按时间排序；
// - 滚动保留最近 N 份（settings.sync_backup_keep，默认 7，夹在 1..=60），超出的最旧自动删除；
// - 触发点：①每次快照导入（**写库前**）自动备份，作为合并前的安全回退点；
//           ②应用启动时「当日首次」自动备份（每天一份，不重复堆叠）。
// - 传输无关：M2 云通道落位后，直接上传最新备份产物到私有桶即可，本模块的触发与保留策略不变。
use rusqlite::Result as SqlResult;

/// 默认保留份数。
pub const DEFAULT_KEEP: i64 = 7;
/// settings 表键：保留份数。
pub const KEEP_KEY: &str = "sync_backup_keep";
/// 备份目录名（与数据库文件同级）。
pub const DIR_NAME: &str = "backups";
/// 备份文件名前缀。
const PREFIX: &str = "fundlens-";

/// 一份备份产物的元信息。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    /// 文件名（不含目录）
    pub file: String,
    /// 字节数
    pub size: i64,
    /// 生成时间（YYYY-MM-DD HH:MM:SS，取自文件名戳）
    pub at: String,
    /// 触发标签：auto / manual / pre-import / 其它
    pub tag: String,
}

fn sql_err<E: std::fmt::Display>(e: E) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(e.to_string()),
    )
}

/// 备份目录：数据库文件同级的 `backups/`（桌面/移动/FUNDLENS_DATA_DIR 覆盖均适用）。
pub fn backup_dir() -> std::path::PathBuf {
    let db = crate::db::db_file_path();
    db.parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(DIR_NAME)
}

/// 保留份数（settings 表；非法/缺失回落默认值，并夹在 1..=60）。
///
/// 注意：本函数**自己取全局连接**，只能在未持连接锁的上下文调用；
/// 若已在 `db::with_conn` 闭包内（如 sync_status），请改用 `keep_count_from(conn)`——
/// `db::with_conn` 的锁不可重入，嵌套调用会死锁。
pub fn keep_count() -> i64 {
    crate::db::with_conn(|c| Ok(keep_count_from(c))).unwrap_or(DEFAULT_KEEP)
}

/// 从给定连接读保留份数（供已持有连接的调用方使用，避免嵌套加锁）。
pub fn keep_count_from(conn: &rusqlite::Connection) -> i64 {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [KEEP_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok();
    raw.and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_KEEP)
        .clamp(1, 60)
}

/// 设置保留份数（夹在 1..=60）并立即按新值剪枝；返回生效值。
pub fn set_keep_count(n: i64) -> SqlResult<i64> {
    let v = n.clamp(1, 60);
    crate::db::with_conn(|c| {
        c.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            rusqlite::params![KEEP_KEY, v.to_string()],
        )?;
        Ok(())
    })?;
    let _ = prune(v)?;
    Ok(v)
}

/// 文件名戳 → 展示时间（`20260910-194657` → `2026-09-10 19:46:57`）。
fn stamp_to_at(stamp: &str) -> String {
    if stamp.len() != 15 || !stamp.contains('-') {
        return stamp.to_string();
    }
    let (d, t) = stamp.split_at(8);
    let t = &t[1..];
    if d.len() != 8 || t.len() != 6 {
        return stamp.to_string();
    }
    format!(
        "{}-{}-{} {}:{}:{}",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &t[0..2],
        &t[2..4],
        &t[4..6]
    )
}

/// 解析备份文件名 → (时间戳, tag)；不符合命名规范返回 None。
fn parse_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(PREFIX)?.strip_suffix(".db")?;
    if rest.len() < 15 {
        return None;
    }
    let (stamp, tag) = rest.split_at(15);
    let tag = tag.strip_prefix('-').unwrap_or(tag);
    // 同秒去重后缀 `_2/_3/...` 属文件名技术细节，不计入 tag
    let tag = tag.split('_').next().unwrap_or(tag);
    Some((stamp.to_string(), if tag.is_empty() { "auto".into() } else { tag.to_string() }))
}

/// 列出全部备份（按时间倒序，最新在前）。
pub fn list_backups() -> SqlResult<Vec<BackupEntry>> {
    let dir = backup_dir();
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(out), // 目录不存在 = 尚无备份
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let (stamp, tag) = match parse_name(&name) {
            Some(v) => v,
            None => continue,
        };
        let size = e.metadata().map(|m| m.len() as i64).unwrap_or(0);
        out.push(BackupEntry {
            file: name,
            size,
            at: stamp_to_at(&stamp),
            tag,
        });
    }
    // 文件名前缀即时间戳 → 按名排序等价于按时间排序
    out.sort_by(|a, b| b.file.cmp(&a.file));
    Ok(out)
}

/// 生成一份整库备份（在线一致快照），随后按保留份数剪枝。
pub fn create_backup(tag: &str) -> SqlResult<BackupEntry> {
    let tag = if tag.is_empty() { "manual" } else { tag };
    let dir = backup_dir();
    std::fs::create_dir_all(&dir).map_err(sql_err)?;
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d-%H%M%S").to_string();
    // 同秒内多次备份（如导入前连点）时追加序号，避免覆盖
    let mut name = format!("{PREFIX}{stamp}-{tag}.db");
    let mut path = dir.join(&name);
    let mut n = 2;
    while path.exists() && n <= 99 {
        name = format!("{PREFIX}{stamp}-{tag}_{n}.db");
        path = dir.join(&name);
        n += 1;
    }
    crate::db::export_db_backup(&path)?;
    let size = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
    let _ = prune(keep_count())?;
    Ok(BackupEntry {
        file: name,
        size,
        at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
        tag: tag.to_string(),
    })
}

/// 按保留份数剪枝：删除最旧的超出部分，返回删除数量。
pub fn prune(keep: i64) -> SqlResult<usize> {
    let keep = keep.clamp(1, 60) as usize;
    let mut all = list_backups()?;
    // list_backups 为倒序（最新在前）→ 尾部即最旧
    if all.len() <= keep {
        return Ok(0);
    }
    let dir = backup_dir();
    let mut removed = 0usize;
    while all.len() > keep {
        let victim = all.pop().expect("len > keep 时必有元素");
        if std::fs::remove_file(dir.join(&victim.file)).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 今天是否已有备份。
pub fn has_backup_today() -> bool {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    list_backups()
        .map(|v| v.iter().any(|b| b.at.starts_with(&today)))
        .unwrap_or(false)
}

/// 启动时调用：当日尚无备份则做一次（每天一份，不重复堆叠）。失败静默返回 None。
pub fn auto_backup_daily() -> Option<BackupEntry> {
    if has_backup_today() {
        return None;
    }
    match create_backup("auto") {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("FundLens 自动备份失败: {e}");
            None
        }
    }
}

/// 写库前调用（快照导入等）：best-effort 备份，失败不阻断主流程。
pub fn auto_backup_before_write(tag: &str) -> Option<BackupEntry> {
    match create_backup(tag) {
        Ok(e) => Some(e),
        Err(e) => {
            eprintln!("FundLens 写库前自动备份失败: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试专用：清空备份目录。
    ///
    /// 必要性：`db::db_file_path()` 的 DB_FILE 是进程级 OnceLock，多个临时库测试会共用
    /// 第一个临时目录 → 备份文件在同一目录里跨测试残留，会让「份数/当日是否已备份」这类断言不确定。
    /// 每个涉及文件的测试先清空，保证独立可重复。
    fn clear_backups() {
        let _ = std::fs::remove_dir_all(backup_dir());
    }

    #[test]
    fn stamp_to_at_formats() {
        assert_eq!(stamp_to_at("20260910-194657"), "2026-09-10 19:46:57");
        assert_eq!(stamp_to_at("bad"), "bad");
    }

    #[test]
    fn parse_name_extracts_stamp_and_tag() {
        assert_eq!(
            parse_name("fundlens-20260910-194657-pre-import.db"),
            Some(("20260910-194657".to_string(), "pre-import".to_string()))
        );
        assert_eq!(
            parse_name("fundlens-20260910-194657-manual_3.db"),
            Some(("20260910-194657".to_string(), "manual".to_string()))
        );
        assert_eq!(parse_name("other-file.db"), None);
    }

    #[test]
    fn create_list_and_prune_keep_n() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        clear_backups();

        assert_eq!(set_keep_count(2).unwrap(), 2);
        for _ in 0..3 {
            create_backup("manual").unwrap();
        }
        let listed = list_backups().unwrap();
        assert_eq!(listed.len(), 2, "保留份数=2 时应只剩 2 份");
        assert!(
            listed.iter().all(|b| b.size > 0),
            "备份文件应非空"
        );

        // 立刻再建一份 → 仍是 2 份（滚动）
        let latest = create_backup("pre-import").unwrap();
        let listed2 = list_backups().unwrap();
        assert_eq!(listed2.len(), 2);
        assert_eq!(listed2[0].file, latest.file, "最新备份应排在首位");
        assert_eq!(listed2[0].tag, "pre-import");
    }

    #[test]
    fn keep_count_clamped_and_persisted() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        assert_eq!(keep_count(), DEFAULT_KEEP, "默认保留份数");
        assert_eq!(set_keep_count(0).unwrap(), 1, "下限夹到 1");
        assert_eq!(keep_count(), 1);
        assert_eq!(set_keep_count(999).unwrap(), 60, "上限夹到 60");
        assert_eq!(keep_count(), 60);
    }

    #[test]
    fn auto_backup_daily_only_once_per_day() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        clear_backups();
        assert!(!has_backup_today(), "全新库当日应无备份");
        let first = auto_backup_daily().expect("首次应生成备份");
        assert!(!first.file.is_empty());
        assert!(has_backup_today());
        assert!(auto_backup_daily().is_none(), "当日已有备份 → 不再重复");
    }
}
