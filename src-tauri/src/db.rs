// 本地 SQLite 存储层（rusqlite bundled，零系统依赖）
// 表结构与 SPEC.md 第 6 节一致。此处实现 v0.1 核心表 + 初始化。
use rusqlite::{Connection, Result as SqlResult};
use std::sync::Mutex;
use once_cell::sync::Lazy;

static DB: Lazy<Mutex<Option<Connection>>> = Lazy::new(|| Mutex::new(None));

fn db_path() -> std::path::PathBuf {
    // Tauri 提供的本地数据目录；回退到当前目录
    let dir = std::env::var("FUNDLENS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();
    dir.join("fundlens.db")
}

pub fn init_db() -> SqlResult<()> {
    let mut guard = DB.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let conn = Connection::open(db_path())?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS funds (
            code TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            platform TEXT NOT NULL,
            official_nav REAL NOT NULL,
            report_period TEXT,
            disclosure_type TEXT,
            fund_type TEXT,
            valuation_applicable INTEGER,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS positions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE CASCADE,
            shares REAL NOT NULL,
            cost_amount REAL NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS disclosures (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT NOT NULL REFERENCES funds(code) ON DELETE CASCADE,
            stock_code TEXT NOT NULL,
            stock_name TEXT,
            weight REAL NOT NULL,
            report_period TEXT,
            disclosure_type TEXT,
            fetched_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS quotes_cache (
            stock_code TEXT PRIMARY KEY,
            price REAL,
            prev_close REAL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- 基金盘中实时估值缓存（SQLite 持久化，替代原进程内 EST_CACHE）。
        -- 与 quotes_cache（基准成分股行情）语义不同，单列一张表，互不污染。
        -- 进程重启后仍在；TTL 与新鲜度由调用方判定（fetched_at + gztime）。
        CREATE TABLE IF NOT EXISTS est_cache (
            fund_code TEXT PRIMARY KEY,
            est_nav REAL,
            est_change_pct REAL,
            prev_nav REAL,
            gztime TEXT,
            fetched_at INTEGER
        );

        CREATE TABLE IF NOT EXISTS import_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            platform TEXT NOT NULL,
            source TEXT,
            detected_count INTEGER,
            status TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS platform_templates (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            platform TEXT NOT NULL UNIQUE,
            ocr_rules TEXT
        );

        CREATE TABLE IF NOT EXISTS ocr_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id INTEGER REFERENCES import_sessions(id) ON DELETE CASCADE,
            image_path TEXT,
            status TEXT,
            result_json TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS quote_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT,
            status TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT
        );

        CREATE TABLE IF NOT EXISTS accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            note TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS transactions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER NOT NULL DEFAULT 1,
            txn_type TEXT NOT NULL,   -- buy / sell / dividend / deposit / withdraw
            fund_code TEXT,           -- deposit/withdraw 为 NULL
            shares REAL,
            amount REAL NOT NULL,     -- 买卖=成交金额；出入金=现金流
            price REAL,
            txn_date TEXT NOT NULL,   -- YYYY-MM-DD（交易日）
            txn_time TEXT,            -- HH:MM（交易日具体时间，用于判断 15:00 前后净值结算日；可选）
            note TEXT,
            source TEXT NOT NULL DEFAULT 'manual',  -- import / manual
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- 组合每日市值快照（周报/月报/盈亏日历的数据源）。旧 snapshots 为未启用的死表，重建。
        DROP TABLE IF EXISTS snapshots;
        CREATE TABLE snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER NOT NULL DEFAULT 1,  -- 0 = 全部账户聚合
            snapshot_date TEXT NOT NULL,
            total_market_value REAL NOT NULL,
            total_cost REAL NOT NULL,
            total_pnl REAL NOT NULL,
            day_pnl REAL NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(account_id, snapshot_date)
        );

        CREATE TABLE IF NOT EXISTS migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- 历史净值缓存（东财 lsjz 自动拉取 + 本地缓存，支撑净值走势图）。
        -- 仅缓存、可由 refresh_nav_history 全量重建，故不进入基线迁移。
        CREATE TABLE IF NOT EXISTS nav_history (
            fund_code TEXT NOT NULL,
            nav_date TEXT NOT NULL,
            nav REAL NOT NULL,
            acc_nav REAL,
            PRIMARY KEY (fund_code, nav_date)
        );

        -- A 股交易日历（上交所休市口径：周末 + 法定节假日；调休补班日落在周末本就不开市）。
        -- 来源：DB 内缓存远程拉取的 holiday-cn（国务院放假安排），内置兜底节假日见 data.rs。
        -- is_open=1 表示交易日；本地缓存避免每次联网，启动时会尝试远程刷新补全/纠偏。
        CREATE TABLE IF NOT EXISTS trading_calendar (
            cal_date TEXT PRIMARY KEY,      -- YYYY-MM-DD
            is_open INTEGER NOT NULL,      -- 1=交易日, 0=休市
            source TEXT,                   -- 'builtin' | 'remote'
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    )?;
    // 记录基线迁移
    conn.execute(
        "INSERT OR IGNORE INTO migrations(version) VALUES(1)",
        [],
    )?;
    // v2：持仓增加支付宝风格字段（幂等）
    ensure_column(&conn, "positions", "holding_amount", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "positions", "holding_profit", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "positions", "yesterday_profit", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "positions", "profit_rate", "REAL NOT NULL DEFAULT 0")?;
    // v3：持仓归属账户（幂等）。已有持仓归入默认账户 id=1。
    ensure_column(&conn, "positions", "account_id", "INTEGER NOT NULL DEFAULT 1")?;
    // 历史数据回填：fund_type 早期允许 NULL（OCR/种子导入未写入），统一置空串，避免读取崩溃
    conn.execute("UPDATE funds SET fund_type='' WHERE fund_type IS NULL", [])?;
    // 索引：positions 按账户查询、transactions 按账户/基金查询
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_positions_account ON positions(account_id)",
        [],
    )?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_positions_account_fund ON positions(account_id, fund_code)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_txn_account ON transactions(account_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_txn_fund ON transactions(fund_code)",
        [],
    )?;
    // v4：流水增量导入批次标识（同一批次重复导入可幂等替换，避免叠加）
    ensure_column(&conn, "transactions", "source_ref", "TEXT")?;
    // v5：流水增加交易时间（用于判断 15:00 前后净值结算日）
    ensure_column(&conn, "transactions", "txn_time", "TEXT")?;
    // v7：平台维度下沉到「持仓/流水」层（单机单账户、支持同基金多平台分别持有）。
    // funds.platform 仅保留为基金级冗余信息（最后导入平台），持仓/总览/统计一律以 positions.platform 为准。
    ensure_column(&conn, "positions", "platform", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&conn, "transactions", "platform", "TEXT NOT NULL DEFAULT ''")?;
    // 旧唯一索引 (account_id, fund_code) 不允许同基金多平台 → 重建为 (account_id, fund_code, platform)
    conn.execute("DROP INDEX IF EXISTS uq_positions_account_fund", [])?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_positions_account_fund_platform \
         ON positions(account_id, fund_code, platform)",
        [],
    )?;
    // 存量数据回填：将既有持仓/流水挂回 funds.platform（最后导入平台），保证升级前数据不丢、且仍可单平台过滤
    conn.execute(
        "UPDATE transactions SET platform = (SELECT f.platform FROM funds f WHERE f.code = transactions.fund_code) \
         WHERE platform = '' AND fund_code IS NOT NULL",
        [],
    )?;
    conn.execute(
        "UPDATE positions SET platform = (SELECT f.platform FROM funds f WHERE f.code = positions.fund_code) \
         WHERE platform = ''",
        [],
    )?;
    // 迁移版本记录（幂等，避免重复执行）
    conn.execute(
        "INSERT OR IGNORE INTO migrations(version) VALUES(2),(3),(4),(5),(6),(7),(8),(9)",
        [],
    )?;
    *guard = Some(conn);
    // 已持有 DB 锁：下方直接用 guard 内的 conn，绝不能再调 with_conn/recompute_positions（非可重入锁→自死锁）。
    let c = guard.as_ref().expect("数据库未初始化");

    // 默认账户：首次启动 seed「默认账户」(id=1)，承接所有历史持仓
    {
        let cnt: i64 = c.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?;
        if cnt == 0 {
            c.execute(
                "INSERT INTO accounts(id, name, note) VALUES(1, '默认账户', '初始账户')",
                [],
            )?;
        }
    }

    // 存量持仓 → 导入基线流水（仅当 transactions 为空，幂等）。随后重算 positions 缓存。
    {
        let txn_cnt: i64 = c.query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))?;
        if txn_cnt == 0 {
            let mut stmt = c.prepare(
                "SELECT fund_code, shares, cost_amount, holding_amount FROM positions",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<usize, String>(0)?, r.get::<usize, f64>(1)?, r.get::<usize, f64>(2)?, r.get::<usize, f64>(3)?))
            })?;
            for row in rows {
                let (fund_code, shares, cost_amount, holding_amount) = row?;
                let (s, amt) = if shares > 0.0 {
                    (shares, cost_amount)
                } else {
                    (0.0, holding_amount)
                };
                c.execute(
                    "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, source)
                     VALUES(1, 'buy', ?1, ?2, ?3, ?4, '1970-01-01', 'import')",
                    rusqlite::params![fund_code, s, amt, if s > 0.0 { amt / s } else { 0.0 }],
                )?;
            }
        }
    }
    // 以事务账本为真相，重算 positions 缓存（账户 1）。直接传 conn，避免重复加锁死锁。
    recompute_positions_conn(c, 1)?;
    Ok(())
}

/// 幂等地为已存在的表增加列（SQLite 的 ADD COLUMN 不支持 IF NOT EXISTS）
fn ensure_column(conn: &Connection, table: &str, col: &str, def: &str) -> SqlResult<()> {
    let exists: bool = conn
        .query_row(
            &format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"),
            [col],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {def}"), [])?;
    }
    Ok(())
}

pub fn with_conn<F, T>(f: F) -> SqlResult<T>
where
    F: FnOnce(&Connection) -> SqlResult<T>,
{
    let guard = DB.lock().unwrap();
    let conn = guard.as_ref().expect("数据库未初始化，请先调用 init_db");
    f(conn)
}

// ---- 基础读写（供 commands 调用）----

#[derive(Debug, Clone, serde::Serialize)]
pub struct FundRow {
    pub code: String,
    pub name: String,
    pub platform: String,
    pub official_nav: f64,
    pub report_period: Option<String>,
    pub disclosure_type: Option<String>,
    pub fund_type: String,
    pub valuation_applicable: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PositionRow {
    pub fund_code: String,
    pub shares: f64,
    pub cost_amount: f64,
    /// 持仓金额（支付宝截图导入）
    pub holding_amount: f64,
    /// 持有收益（支付宝截图导入）
    pub holding_profit: f64,
    /// 昨日收益（支付宝截图导入）
    pub yesterday_profit: f64,
    /// 收益率（支付宝截图导入，百分数）
    pub profit_rate: f64,
}

pub fn list_funds() -> SqlResult<Vec<FundRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT code, name, platform, official_nav, report_period, disclosure_type, fund_type, valuation_applicable FROM funds ORDER BY code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FundRow {
                code: r.get(0)?,
                name: r.get(1)?,
                platform: r.get(2)?,
                official_nav: r.get(3)?,
                report_period: r.get(4)?,
                disclosure_type: r.get(5)?,
                // fund_type 在 OCR 导入路径下可能为空（INSERT 未写入），需容忍 NULL
                fund_type: r.get::<usize, Option<String>>(6)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(7).unwrap_or(1) != 0,
            })
        })?;
        rows.collect()
    })
}

/// 仅写入/更新基金元数据（不写持仓）。持仓由 set_baseline / recompute_positions 统一从流水账本派生。
pub fn insert_fund(f: &FundRow) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT OR REPLACE INTO funds(code,name,platform,official_nav,report_period,disclosure_type,fund_type,valuation_applicable)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                f.code, f.name, f.platform, f.official_nav, f.report_period, f.disclosure_type,
                f.fund_type, f.valuation_applicable
            ],
        )?;
        Ok(())
    })
}

/// 仅更新基金元数据（供导入/手动新增复用，不触碰持仓）。
pub fn upsert_fund_meta(
    code: &str,
    name: &str,
    platform: &str,
    nav: f64,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable)
             VALUES(?1,?2,?3,?4,'',1)
             ON CONFLICT(code) DO UPDATE SET name=?2, official_nav=?4",
            rusqlite::params![code, name, platform, nav],
        )?;
        Ok(())
    })
}

/// 更新官方净值、基金类型与估值适用性。
/// 注意：report_period（披露期）不再写 funds 表——它由 disclosures 表按持仓记录，
/// 由 get_fund_detail 从披露记录派生，避免被净值日期误覆盖。
pub fn update_fund_nav(
    code: &str,
    nav: f64,
    fund_type: &str,
    applicable: bool,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "UPDATE funds SET official_nav=?2, fund_type=?3, valuation_applicable=?4 WHERE code=?1",
            rusqlite::params![code, nav, fund_type, applicable],
        )?;
        Ok(())
    })
}

/// 清空某基金的全部披露持仓（重新抓取前调用，避免重复叠加）
pub fn delete_disclosures(fund_code: &str) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute("DELETE FROM disclosures WHERE fund_code = ?1", [fund_code])?;
        Ok(())
    })
}

pub fn upsert_disclosure(
    fund_code: &str,
    stock_code: &str,
    stock_name: &str,
    weight: f64,
    report_period: &str,
    disclosure_type: &str,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO disclosures(fund_code,stock_code,stock_name,weight,report_period,disclosure_type)
             VALUES(?1,?2,?3,?4,?5,?6)",
            rusqlite::params![fund_code, stock_code, stock_name, weight, report_period, disclosure_type],
        )?;
        Ok(())
    })
}

/// 从持仓截图识别结果写入「基金 + 持仓」。改用 import_positions_batch（批量、按账户、统一重算）。
pub fn list_disclosures(fund_code: &str) -> SqlResult<Vec<crate::valuation::DisclosedHolding>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT stock_code, stock_name, weight, report_period, disclosure_type FROM disclosures
             WHERE fund_code = ?1 ORDER BY weight DESC",
        )?;
        let rows = stmt.query_map([fund_code], |r| {
            Ok(crate::valuation::DisclosedHolding {
                stock_code: r.get(0)?,
                stock_name: r.get(1)?,
                weight: r.get(2)?,
                report_period: r.get(3)?,
                disclosure_type: r.get(4)?,
            })
        })?;
        rows.collect()
    })
}

/// 一次性批量拉取全部披露持仓（单次 SQL 往返），按 (fund_code, 持仓) 返回。
/// 用于持仓总览：避免对每只基金分别 list_disclosures 造成的 N 次 DB 往返。
pub fn list_disclosures_batch() -> SqlResult<Vec<(String, crate::valuation::DisclosedHolding)>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fund_code, stock_code, stock_name, weight, report_period, disclosure_type \
             FROM disclosures ORDER BY fund_code, weight DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![], |r| {
            Ok((
                r.get::<usize, String>(0)?,
                crate::valuation::DisclosedHolding {
                    stock_code: r.get(1)?,
                    stock_name: r.get(2)?,
                    weight: r.get(3)?,
                    report_period: r.get(4)?,
                    disclosure_type: r.get(5)?,
                },
            ))
        })?;
        rows.collect()
    })
}

/// 汇总指定日期的净现金流（入金 − 出金），用于每日快照的当日真实收益计算。
/// 定向 SQL 聚合（WHERE txn_date=? AND txn_type IN (deposit,withdraw)），
/// 避免每次 get_overview 全表扫描所有交易记录。
pub fn sum_cash_flow_on(date: &str) -> SqlResult<f64> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(SUM(CASE txn_type WHEN 'deposit' THEN amount \
             WHEN 'withdraw' THEN -amount ELSE 0 END), 0) \
             FROM transactions WHERE txn_date = ?1 AND txn_type IN ('deposit','withdraw')",
        )?;
        let v: f64 = stmt.query_row([date], |r| r.get(0))?;
        Ok(v)
    })
}

pub fn upsert_quote(stock_code: &str, price: f64, prev_close: f64) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO quotes_cache(stock_code,price,prev_close) VALUES(?1,?2,?3)
             ON CONFLICT(stock_code) DO UPDATE SET price=?2, prev_close=?3, updated_at=datetime('now')",
            rusqlite::params![stock_code, price, prev_close],
        )?;
        Ok(())
    })
}

pub fn get_cached_quote(stock_code: &str) -> SqlResult<Option<crate::valuation::StockQuote>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT stock_code, price, prev_close FROM quotes_cache WHERE stock_code = ?1")?;
        let mut rows = stmt.query_map([stock_code], |r| {
            Ok(crate::valuation::StockQuote {
                stock_code: r.get(0)?,
                name: String::new(),
                price: r.get(1)?,
                prev_close: r.get(2)?,
            })
        })?;
        match rows.next() {
            Some(Ok(q)) => Ok(Some(q)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    })
}

// ============ 基金盘中实时估值缓存（SQLite 持久化，替代原进程内 EST_CACHE）============

/// 一笔基金盘中实时估值的持久化形态（与 data::FundEstimate 一一对应 + fetched_at）。
#[derive(Clone)]
pub struct CachedEst {
    pub est_nav: f64,
    pub est_change_pct: f64,
    pub prev_nav: f64,
    pub gztime: String,
    pub fetched_at: i64,
}

impl CachedEst {
    /// 还原为上游估值结构（供估值引擎使用）。
    pub fn to_estimate(&self) -> crate::data::FundEstimate {
        crate::data::FundEstimate {
            est_nav: self.est_nav,
            est_change_pct: self.est_change_pct,
            prev_nav: self.prev_nav,
            gztime: self.gztime.clone(),
        }
    }
}

/// 批量读取估值缓存（仅返回存在且无过期判定由调用方做；此处直接取所有命中行）。
pub fn load_est_cache(codes: &[String]) -> SqlResult<std::collections::HashMap<String, CachedEst>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fund_code, est_nav, est_change_pct, prev_nav, gztime, fetched_at \
             FROM est_cache WHERE fund_code = ?1",
        )?;
        let mut map: std::collections::HashMap<String, CachedEst> = std::collections::HashMap::new();
        for c in codes {
            let mut rows = stmt.query_map([c], |r| {
                Ok(CachedEst {
                    est_nav: r.get(1)?,
                    est_change_pct: r.get(2)?,
                    prev_nav: r.get(3)?,
                    gztime: r.get(4)?,
                    fetched_at: r.get(5)?,
                })
            })?;
            if let Some(Ok(e)) = rows.next() {
                map.insert(c.clone(), e);
            }
        }
        Ok(map)
    })
}

/// 批量写入/刷新估值缓存（事务内 upsert，进程重启后仍在）。
pub fn save_est_cache(
    items: &[(String, crate::data::FundEstimate, i64)],
) -> SqlResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        for (code, est, fetched_at) in items {
            tx.execute(
                "INSERT INTO est_cache(fund_code, est_nav, est_change_pct, prev_nav, gztime, fetched_at) \
                 VALUES(?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT(fund_code) DO UPDATE SET \
                   est_nav=?2, est_change_pct=?3, prev_nav=?4, gztime=?5, fetched_at=?6",
                rusqlite::params![
                    code,
                    est.est_nav,
                    est.est_change_pct,
                    est.prev_nav,
                    est.gztime,
                    fetched_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

// ===================== A 股交易日历缓存 =====================
// 上交所休市口径：周末 + 法定节假日（春节/国庆/清明/劳动节/端午/中秋/元旦等）。
// 数据来自 holiday-cn（国务院放假安排），由 data.rs 启动时远程拉取并写入本表；
// 内置兜底节假日见 data.rs::BUILTIN_OFF_DAYS，保证离线也能识别长假休市。

/// 数据库是否已初始化（供 data.rs 在 DB 未就绪时安全跳过缓存读写）。
pub fn db_ready() -> bool {
    DB.lock().unwrap().is_some()
}

/// 批量写入/刷新交易日历（事务内 upsert，远程数据覆盖旧值）。
pub fn upsert_calendar_days(days: &[(String, bool, &str)]) -> SqlResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        for (date, is_open, source) in days {
            tx.execute(
                "INSERT INTO trading_calendar(cal_date, is_open, source, updated_at) \
                 VALUES(?1,?2,?3,datetime('now')) \
                 ON CONFLICT(cal_date) DO UPDATE SET is_open=?2, source=?3, updated_at=datetime('now')",
                rusqlite::params![date, *is_open as i64, source],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

/// 读取某日是否开市（None 表示本地暂无该日数据，调用方按兜底策略处理）。
pub fn get_calendar_open(date: &str) -> SqlResult<Option<bool>> {
    with_conn(|conn| {
        let r = conn.query_row(
            "SELECT is_open FROM trading_calendar WHERE cal_date = ?1",
            [date],
            |r| r.get::<usize, i64>(0),
        );
        match r {
            Ok(v) => Ok(Some(v != 0)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    })
}

/// 读取某年全部已缓存交易日历（供 data.rs 预热内存缓存）。
pub fn load_calendar_year_from_db(year: i32) -> SqlResult<Vec<(String, bool)>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT cal_date, is_open FROM trading_calendar WHERE cal_date LIKE ?1",
        )?;
        let pat = format!("{year}-%");
        let rows = stmt.query_map([pat], |r| {
            Ok((r.get::<usize, String>(0)?, r.get::<usize, i64>(1)? != 0))
        })?;
        let mut v = Vec::new();
        for row in rows {
            v.push(row?);
        }
        Ok(v)
    })
}

// ===================== 多账户 / 交易流水 / 快照 =====================

#[derive(Debug, Clone, serde::Serialize)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TransactionRow {
    pub id: i64,
    pub account_id: i64,
    pub txn_type: String, // buy / sell / deposit / withdraw
    pub fund_code: Option<String>,
    pub shares: Option<f64>,
    pub amount: f64,
    pub price: Option<f64>,
    pub txn_date: String,
    pub txn_time: String,
    pub note: Option<String>,
    pub source: String, // import / manual_set / import_txn / manual_txn
    pub source_ref: Option<String>, // 导入批次标识（增量合并用）
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HoldingRow {
    pub account_id: i64,
    pub code: String,
    pub name: String,
    pub platform: String,
    pub official_nav: f64,
    pub report_period: Option<String>,
    pub disclosure_type: Option<String>,
    pub fund_type: String,
    pub valuation_applicable: bool,
    pub shares: f64,
    pub cost_amount: f64,
    pub holding_amount: f64,
    pub holding_profit: f64,
    pub yesterday_profit: f64,
    pub profit_rate: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SnapshotRow {
    pub id: i64,
    pub account_id: i64,
    pub snapshot_date: String,
    pub total_market_value: f64,
    pub total_cost: f64,
    pub total_pnl: f64,
    pub day_pnl: f64,
}

/// 导入持仓批次项（截图导入专用）。
pub struct ImportHolding {
    pub code: String,
    pub name: String,
    pub platform: String,
    pub nav: f64,
    pub shares: f64,
    pub holding_amount: f64,
    pub holding_profit: f64,
    pub yesterday_profit: f64,
    pub profit_rate: f64,
}

/// 导入交易记录项（买卖/分红增量导入）。txn_type 已规范为 buy/sell/dividend。
#[derive(Clone)]
pub struct ImportTxn {
    pub fund_code: String,
    pub fund_name: Option<String>,
    pub txn_type: String,
    pub shares: Option<f64>,
    pub amount: f64,
    pub price: Option<f64>,
    pub txn_date: String,
    pub txn_time: String,
    pub note: Option<String>,
    /// 该笔交易所属平台（交易截图导入时已知）。用于补全 funds.platform：
    /// 基金首次创建时写入；若已存在但平台为空（仅来自交易的基金），冲突时补全。
    pub platform: String,
}

// ---- 账户 ----

pub fn list_accounts() -> SqlResult<Vec<AccountRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id, name, note FROM accounts ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok(AccountRow {
                id: r.get(0)?,
                name: r.get(1)?,
                note: r.get(2)?,
            })
        })?;
        rows.collect()
    })
}

pub fn create_account(name: &str, note: &str) -> SqlResult<i64> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO accounts(name, note) VALUES(?1, ?2)",
            rusqlite::params![name, note],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

pub fn rename_account(id: i64, name: &str, note: Option<&str>) -> SqlResult<()> {
    with_conn(|conn| {
        match note {
            Some(n) => conn.execute(
                "UPDATE accounts SET name=?2, note=?3 WHERE id=?1",
                rusqlite::params![id, name, n],
            )?,
            None => conn.execute("UPDATE accounts SET name=?2 WHERE id=?1", rusqlite::params![id, name])?,
        };
        Ok(())
    })
}

/// 删除账户：非空（仍有持仓）则拒绝，避免误删丢失数据。
pub fn delete_account(id: i64) -> SqlResult<()> {
    with_conn(|conn| {
        let cnt: i64 = conn.query_row(
            "SELECT COUNT(*) FROM positions WHERE account_id = ?1 AND (shares > 0 OR holding_amount > 0)",
            [id],
            |r| r.get(0),
        )?;
        if cnt > 0 {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
                Some("账户仍有持仓，无法删除（请先清空或删除其中基金）".into()),
            ));
        }
        conn.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        Ok(())
    })
}

// ---- 交易流水 ----

pub fn list_transactions(
    account_id: Option<i64>,
    fund_code: Option<String>,
) -> SqlResult<Vec<TransactionRow>> {
    with_conn(|conn| {
        let mut sql = String::from(
            "SELECT id, account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, source, source_ref \
             FROM transactions WHERE 1=1",
        );
        // 占位符与参数必须一一对应：仅有筛选条件时才追加占位符与参数，避免参数数量不匹配。
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if account_id.is_some() {
            sql.push_str(" AND account_id = ?");
            params.push(account_id.map(|v| rusqlite::types::Value::Integer(v)).unwrap_or(rusqlite::types::Value::Null));
        }
        if fund_code.is_some() {
            sql.push_str(" AND fund_code = ?");
            params.push(fund_code.map(|v| rusqlite::types::Value::Text(v)).unwrap_or(rusqlite::types::Value::Null));
        }
        sql.push_str(" ORDER BY txn_date DESC, id DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
            Ok(TransactionRow {
                id: r.get(0)?,
                account_id: r.get(1)?,
                txn_type: r.get(2)?,
                fund_code: r.get(3)?,
                shares: r.get(4)?,
                amount: r.get(5)?,
                price: r.get(6)?,
                txn_date: r.get(7)?,
                // txn_time/note/source_ref 等列允许 NULL（如基线流水、早期行未写入），读取时必须容忍空值。
                txn_time: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                note: r.get(9)?,
                source: r.get(10)?,
                source_ref: r.get(11)?,
            })
        })?;
        rows.collect()
    })
}

/// 新增一笔手动流水（买卖/出入金）。写完后按账户重算持仓缓存。
pub fn add_transaction(
    account_id: i64,
    txn_type: &str,
    fund_code: Option<String>,
    shares: Option<f64>,
    amount: f64,
    price: Option<f64>,
    txn_date: &str,
    txn_time: &str,
    note: Option<String>,
    platform: &str,
) -> SqlResult<i64> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform, source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'manual_txn')",
            rusqlite::params![account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform],
        )?;
        let id = conn.last_insert_rowid();
        recompute_positions_conn(conn, account_id)?;
        Ok(id)
    })
}

pub fn delete_transaction(id: i64) -> SqlResult<()> {
    with_conn(|conn| {
        let account_id: i64 = conn.query_row(
            "SELECT account_id FROM transactions WHERE id = ?1",
            [id],
            |r| r.get(0),
        )?;
        conn.execute("DELETE FROM transactions WHERE id = ?1", [id])?;
        recompute_positions_conn(conn, account_id)?;
        Ok(())
    })
}

/// 设置某基金在某账户的「基线持仓」（截图导入或手动增改）。
/// 替换该基金在该账户内的 import/manual_set 基线（互斥，避免重复计数），随后重算。
pub fn set_baseline(
    account_id: i64,
    code: &str,
    shares: f64,
    cost_amount: f64,
    holding_amount: f64,
    holding_profit: f64,
    yesterday_profit: f64,
    profit_rate: f64,
    platform: &str,
    source: &str,
) -> SqlResult<()> {
    with_conn(|conn| {
        write_baseline_conn(conn, account_id, code, shares, cost_amount, holding_amount, holding_profit, yesterday_profit, profit_rate, platform, source)?;
        recompute_positions_conn(conn, account_id)?;
        Ok(())
    })
}

/// 截图导入批次：逐基金替换 import 基线并写入基金元数据，最后统一重算一次（避免 N 次重算）。
pub fn import_positions_batch(account_id: i64, items: &[ImportHolding]) -> SqlResult<()> {
    with_conn(|conn| {
        for it in items {
            conn.execute(
                "INSERT INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable)
                 VALUES(?1,?2,?3,?4,'',1)
                 ON CONFLICT(code) DO UPDATE SET name=?2, official_nav=?4",
                rusqlite::params![it.code, it.name, it.platform, it.nav],
            )?;
            write_baseline_conn(
                conn,
                account_id,
                &it.code,
                it.shares,
                it.shares * it.nav,
                it.holding_amount,
                it.holding_profit,
                it.yesterday_profit,
                it.profit_rate,
                &it.platform,
                "import",
            )?;
        }
        recompute_positions_conn(conn, account_id)?;
        Ok(())
    })
}

/// 增量导入交易记录（买卖/分红）。
/// 设计要点（扩展自 delete_disclosures 的「前置清空避免叠加」模式）：
///  - 若提供 source_ref：先删除本账户下同源批次(import_txn + 同 ref)的旧记录，再插入 → 同批次重复导入幂等、不叠加。
///  - 不同批次 / 手动流水(manual_txn) 互不干扰，实现「流水增量合并」。
///  - 对某基金导入真实流水时，移除其截图/手动基线(import/manual_set)，避免与真实流水重复计数（账本为单一真相）。
///  - 写完后统一重算一次持仓（避免 N 次重算）。
pub fn import_transactions(
    account_id: i64,
    items: &[ImportTxn],
    source_ref: Option<String>,
) -> SqlResult<usize> {
    with_conn(|conn| {
        // 1) 同批次幂等：先清后写
        if let Some(ref_) = &source_ref {
            conn.execute(
                "DELETE FROM transactions WHERE account_id=?1 AND source='import_txn' AND source_ref=?2",
                rusqlite::params![account_id, ref_],
            )?;
        }
        let mut count = 0usize;
        for it in items {
            // 2) 确保基金元数据存在（不覆盖已有名称/净值）
            let name = it
                .fund_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| it.fund_code.clone());
            // 交易截图导入时已知平台：首次创建写入平台；若基金已存在但平台为空
            // （仅来自交易、从未导入持仓截图的基金），冲突时用 excluded.platform 补全，
            // 避免「导入交易记录后持仓明细出现平台为空」的问题。official_nav 等不在 SET 中，
            // 冲突时保持原值（不破坏持仓截图写入的真实净值）。
            conn.execute(
                "INSERT INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable) \
                 VALUES(?1,?2,?3,0,'',1) \
                 ON CONFLICT(code) DO UPDATE SET platform = COALESCE(platform, excluded.platform)",
                rusqlite::params![it.fund_code, name, it.platform],
            )?;
            // 3) 移除该基金「同平台」在账户内的合成基线（截图/手动），真实流水接管，避免重复计数。
            //    必须限定 platform，否则重导支付宝流水会误删京东金融基线，造成跨平台持仓丢失。
            conn.execute(
                "DELETE FROM transactions WHERE account_id=?1 AND fund_code=?2 AND platform=?3 AND source IN ('import','manual_set')",
                rusqlite::params![account_id, it.fund_code, it.platform],
            )?;
            // 4) 写入/更新导入流水（import_txn + source_ref）
            //    去重：同一账户内已存在「基金代码/类型/日期/时间/金额」完全相同的 import_txn 流水时，
            //    更新其份额/价格而非新增，避免多次识别同一截图产生重复记录（满足「相同平台和时间不新增，更新」）。
            //    与上方按 source_ref 整批替换互不冲突：有批次标签时先删旧批再插入；无标签时靠此自然键去重。
            let existing_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM transactions WHERE account_id=?1 AND source='import_txn' \
                     AND fund_code=?2 AND platform=?3 AND txn_type=?4 AND txn_date=?5 AND txn_time=?6 \
                     AND abs(amount - ?7) < 0.005 LIMIT 1",
                    rusqlite::params![
                        account_id, it.fund_code, it.platform, it.txn_type, it.txn_date, it.txn_time, it.amount
                    ],
                    |r| r.get(0),
                )
                .ok();
            if let Some(eid) = existing_id {
                conn.execute(
                    "UPDATE transactions SET shares=?1, price=?2 WHERE id=?3",
                    rusqlite::params![it.shares, it.price, eid],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform, source, source_ref) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'import_txn',?11)",
                    rusqlite::params![
                        account_id,
                        it.txn_type,
                        it.fund_code,
                        it.shares,
                        it.amount,
                        it.price,
                        it.txn_date,
                        it.txn_time,
                        it.note,
                        it.platform,
                        source_ref
                    ],
                )?;
            }
            count += 1;
        }
        recompute_positions_conn(conn, account_id)?;
        Ok(count)
    })
}

/// 基线写入（同一 with_conn 闭包内调用，避免嵌套加锁死锁）：替换 import/manual_set 基线流水 + 直写 positions（含支付宝展示字段）。
fn write_baseline_conn(
    conn: &Connection,
    account_id: i64,
    code: &str,
    shares: f64,
    cost_amount: f64,
    holding_amount: f64,
    holding_profit: f64,
    yesterday_profit: f64,
    profit_rate: f64,
    platform: &str,
    source: &str,
) -> SqlResult<()> {
    // 确保基金元数据存在（positions 外键指向 funds），名称先用代码占位，后续刷新行情/导入会修正
    conn.execute(
        "INSERT OR IGNORE INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable) \
         VALUES(?1,?2,'',0,'',1)",
        rusqlite::params![code, code],
    )?;
    // 仅删除「同账户 + 同基金 + 同平台」的既有 import/manual_set 基线，避免覆盖其他平台的持仓
    conn.execute(
        "DELETE FROM transactions WHERE account_id=?1 AND fund_code=?2 AND platform=?3 AND source IN ('import','manual_set')",
        rusqlite::params![account_id, code, platform],
    )?;
    let (s, amt, price) = if shares > 0.0 {
        (shares, cost_amount, cost_amount / shares)
    } else {
        (0.0, holding_amount, 0.0)
    };
    // 基线买入使用最早日期，作为「期初持仓」最先重放，避免历史买卖/分红被排到基线之前而失效
    conn.execute(
        "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, source, platform)
         VALUES(?1,'buy',?2,?3,?4,?5,'1970-01-01',?6,?7)",
        rusqlite::params![account_id, code, s, amt, price, source, platform],
    )?;
    conn.execute(
        "INSERT INTO positions(account_id, fund_code, platform, shares, cost_amount, holding_amount, holding_profit, yesterday_profit, profit_rate)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(account_id, fund_code, platform) DO UPDATE SET
           shares=excluded.shares, cost_amount=excluded.cost_amount, holding_amount=excluded.holding_amount,
           holding_profit=excluded.holding_profit, yesterday_profit=excluded.yesterday_profit, profit_rate=excluded.profit_rate",
        rusqlite::params![account_id, code, platform, shares, cost_amount, holding_amount, holding_profit, yesterday_profit, profit_rate],
    )?;
    Ok(())
}

/// 按账户重放全部流水，以平均成本法重建 positions 缓存（份额/成本/持仓金额）。
/// 支付宝风格（份额为 0）以 holding_amount 记录，不参与成本口径。
fn recompute_positions_conn(conn: &Connection, account_id: i64) -> SqlResult<()> {
    struct St {
        shares: f64,
        basis: f64,
        holding_amount: f64,
    }
    // 按 (基金代码, 平台) 聚合，支持同基金多平台分别持有
    let mut map: std::collections::HashMap<(String, String), St> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT fund_code, platform, txn_type, shares, amount FROM transactions
             WHERE account_id = ?1 ORDER BY txn_date ASC, id ASC",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok((
                r.get::<usize, Option<String>>(0)?,
                r.get::<usize, String>(1)?,
                r.get::<usize, String>(2)?,
                r.get::<usize, Option<f64>>(3)?,
                r.get::<usize, f64>(4)?,
            ))
        })?;
        for row in rows {
            let (fund_code, platform, txn_type, shares, amount) = row?;
            let code = match fund_code {
                Some(c) => c,
                None => continue, // 出入金不影响个基
            };
            // 同一基金按 (基金, 平台) 聚合：同基金多平台各自独立核算持仓与成本
            let st = map
                .entry((code, platform))
                .or_insert(St { shares: 0.0, basis: 0.0, holding_amount: 0.0 });
            match txn_type.as_str() {
                "buy" => {
                    if let Some(s) = shares {
                        if s > 0.0 {
                            st.shares += s;
                            st.basis += amount;
                        } else {
                            st.holding_amount = amount;
                        }
                    }
                }
                "sell" => {
                    if let Some(s) = shares {
                        if s > 0.0 && st.shares > 0.0 {
                            let sell_basis = s * if st.shares > 0.0 { st.basis / st.shares } else { 0.0 };
                            st.basis -= sell_basis;
                            st.shares -= s;
                            if st.shares <= 1e-9 {
                                st.shares = 0.0;
                                st.basis = 0.0;
                            }
                        }
                    }
                }
                // 现金分红：按「成本还原法」减少持仓成本基数，份额不变。
                // 除息后净值下调带来的市值下降，被成本下调抵消，使累计收益准确包含分红。
                // 仅对份额型持仓生效（支付宝式 holding_amount 持仓无成本基数，分红不计入口径）。
                "dividend" => {
                    if st.shares > 0.0 {
                        st.basis -= amount; // 允许为负（收回全部成本后，分红即净收益）
                    }
                }
                _ => {}
            }
        }
    }
    // upsert（保留支付宝展示字段 holding_profit/yesterday_profit/profit_rate，不在 SET 中）
    for ((code, platform), st) in &map {
        if st.shares > 0.0 || st.holding_amount > 0.0 {
            conn.execute(
                "INSERT INTO positions(account_id, fund_code, platform, shares, cost_amount, holding_amount)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(account_id, fund_code, platform) DO UPDATE SET
                   shares=excluded.shares, cost_amount=excluded.cost_amount, holding_amount=excluded.holding_amount",
                rusqlite::params![account_id, code, platform, st.shares, st.basis, st.holding_amount],
            )?;
        }
    }
    // 清除已清仓（份额=0 且 持仓金额=0）的持仓行
    conn.execute(
        "DELETE FROM positions WHERE account_id = ?1 AND shares <= 0 AND holding_amount <= 0",
        [account_id],
    )?;
    Ok(())
}

/// 对外包装：在 with_conn 内重算。
pub fn recompute_positions(account_id: i64) -> SqlResult<()> {
    with_conn(|conn| recompute_positions_conn(conn, account_id))
}

// ---- 持仓视图（按账户过滤） ----

pub fn list_holdings(account_id: Option<i64>) -> SqlResult<Vec<HoldingRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.account_id, f.code, f.name, p.platform, f.official_nav, f.report_period,
                    f.disclosure_type, f.fund_type, f.valuation_applicable,
                    p.shares, p.cost_amount, p.holding_amount, p.holding_profit, p.yesterday_profit, p.profit_rate
             FROM positions p JOIN funds f ON f.code = p.fund_code
             WHERE (?1 IS NULL OR p.account_id = ?1)
             ORDER BY p.account_id, f.code",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(HoldingRow {
                account_id: r.get(0)?,
                code: r.get(1)?,
                name: r.get(2)?,
                platform: r.get(3)?,
                official_nav: r.get(4)?,
                report_period: r.get(5)?,
                disclosure_type: r.get(6)?,
                fund_type: r.get::<usize, Option<String>>(7)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(8).unwrap_or(1) != 0,
                shares: r.get(9)?,
                cost_amount: r.get(10)?,
                holding_amount: r.get(11)?,
                holding_profit: r.get(12)?,
                yesterday_profit: r.get(13)?,
                profit_rate: r.get(14)?,
            })
        })?;
        rows.collect()
    })
}

pub fn get_holding(code: &str, account_id: i64) -> SqlResult<Option<HoldingRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.account_id, f.code, f.name, p.platform, f.official_nav, f.report_period,
                    f.disclosure_type, f.fund_type, f.valuation_applicable,
                    p.shares, p.cost_amount, p.holding_amount, p.holding_profit, p.yesterday_profit, p.profit_rate
             FROM positions p JOIN funds f ON f.code = p.fund_code
             WHERE p.fund_code = ?1 AND p.account_id = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![code, account_id], |r| {
            Ok(HoldingRow {
                account_id: r.get(0)?,
                code: r.get(1)?,
                name: r.get(2)?,
                platform: r.get(3)?,
                official_nav: r.get(4)?,
                report_period: r.get(5)?,
                disclosure_type: r.get(6)?,
                fund_type: r.get::<usize, Option<String>>(7)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(8).unwrap_or(1) != 0,
                shares: r.get(9)?,
                cost_amount: r.get(10)?,
                holding_amount: r.get(11)?,
                holding_profit: r.get(12)?,
                yesterday_profit: r.get(13)?,
                profit_rate: r.get(14)?,
            })
        })?;
        match rows.next() {
            Some(Ok(h)) => Ok(Some(h)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    })
}

// ---- 快照 ----

pub fn record_snapshot(
    account_id: i64,
    snapshot_date: &str,
    total_market_value: f64,
    total_cost: f64,
    total_pnl: f64,
    day_pnl: f64,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO snapshots(account_id, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(account_id, snapshot_date) DO UPDATE SET
               total_market_value=excluded.total_market_value, total_cost=excluded.total_cost,
               total_pnl=excluded.total_pnl, day_pnl=excluded.day_pnl",
            rusqlite::params![account_id, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl],
        )?;
        Ok(())
    })
}

pub fn list_snapshots(account_id: i64) -> SqlResult<Vec<SnapshotRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, account_id, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl
             FROM snapshots WHERE account_id = ?1 ORDER BY snapshot_date ASC",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(SnapshotRow {
                id: r.get(0)?,
                account_id: r.get(1)?,
                snapshot_date: r.get(2)?,
                total_market_value: r.get(3)?,
                total_cost: r.get(4)?,
                total_pnl: r.get(5)?,
                day_pnl: r.get(6)?,
            })
        })?;
        rows.collect()
    })
}

pub fn delete_fund(code: &str) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute("DELETE FROM transactions WHERE fund_code = ?1", [code])?;
        conn.execute("DELETE FROM funds WHERE code = ?1", [code])?;
        Ok(())
    })
}

// ===================== 历史净值缓存 / 成本走势 / 交易标记 =====================

/// 批量写入/更新历史净值（按 (fund_code, nav_date) 幂等 upsert）。
pub fn upsert_nav_history(code: &str, points: &[crate::data::NavPoint]) -> SqlResult<usize> {
    with_conn(|conn| {
        let mut count = 0usize;
        for p in points {
            conn.execute(
                "INSERT INTO nav_history(fund_code, nav_date, nav, acc_nav) VALUES(?1,?2,?3,?4)
                 ON CONFLICT(fund_code, nav_date) DO UPDATE SET nav=excluded.nav, acc_nav=excluded.acc_nav",
                rusqlite::params![
                    code,
                    p.date,
                    p.nav,
                    if p.acc_nav > 0.0 { Some(p.acc_nav) } else { None }
                ],
            )?;
            count += 1;
        }
        Ok(count)
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NavPointRow {
    pub date: String,
    pub nav: f64,
    pub acc_nav: f64, // 缺失记为 0
}

/// 读取某基金全部历史净值（按日期升序）。缓存不足时由 refresh_nav_history 补齐。
pub fn get_nav_history(code: &str) -> SqlResult<Vec<NavPointRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT nav_date, nav, COALESCE(acc_nav,0) FROM nav_history WHERE fund_code = ?1 ORDER BY nav_date ASC",
        )?;
        let rows = stmt.query_map([code], |r| {
            Ok(NavPointRow {
                date: r.get(0)?,
                nav: r.get(1)?,
                acc_nav: r.get(2)?,
            })
        })?;
        rows.collect()
    })
}

/// 单个成本序列点：在某交易日之后，账户的累计成本/单位成本/份额。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CostPoint {
    pub date: String,
    pub cumulative_cost: f64, // 累计成本基数（basis）
    pub unit_cost: f64,       // 单位成本 = basis / shares（份额为 0 时记 0）
    pub shares: f64,
}

/// 成本走势序列：复用 `recompute_positions_conn` 的平均成本法，但逐笔交易产出成本点。
/// 仅对真实交易（txn_date != '1970-01-01'）产出可见点；1970 基线仅用于初始化状态，
/// 不产出点（避免 1970 时间轴被压缩）。卖出/分红沿用平均成本口径（成本还原法）。
pub fn get_cost_series(code: &str, account_id: i64) -> SqlResult<Vec<CostPoint>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT txn_type, shares, amount, txn_date FROM transactions
             WHERE account_id = ?1 AND fund_code = ?2 ORDER BY txn_date ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![account_id, code], |r| {
            Ok((
                r.get::<usize, String>(0)?,
                r.get::<usize, Option<f64>>(1)?,
                r.get::<usize, f64>(2)?,
                r.get::<usize, String>(3)?,
            ))
        })?;
        let mut shares = 0.0f64;
        let mut basis = 0.0f64;
        let mut points: Vec<CostPoint> = Vec::new();
        for row in rows {
            let (txn_type, shares_opt, amount, txn_date) = row?;
            if txn_date == "1970-01-01" {
                // 基线仅初始化运行态（不产出可见点）
                if let Some(s) = shares_opt {
                    if s > 0.0 {
                        shares = s;
                        basis = amount;
                    } else {
                        basis = amount;
                    }
                }
                continue;
            }
            match txn_type.as_str() {
                "buy" => {
                    if let Some(s) = shares_opt {
                        if s > 0.0 {
                            shares += s;
                            basis += amount;
                        } else {
                            basis = amount;
                        }
                    }
                }
                "sell" => {
                    if let Some(s) = shares_opt {
                        if s > 0.0 && shares > 0.0 {
                            let sell_basis = s * if shares > 0.0 { basis / shares } else { 0.0 };
                            basis -= sell_basis;
                            shares -= s;
                            if shares <= 1e-9 {
                                shares = 0.0;
                                basis = 0.0;
                            }
                        }
                    }
                }
                // 现金分红：成本还原法，份额不变；允许成本转负（收回全部成本后为净收益）
                "dividend" => {
                    if shares > 0.0 {
                        basis -= amount;
                    }
                }
                _ => {}
            }
            points.push(CostPoint {
                date: txn_date.clone(),
                cumulative_cost: basis,
                unit_cost: if shares > 0.0 { basis / shares } else { 0.0 },
                shares,
            });
        }
        Ok(points)
    })
}

/// 单个交易标记（供走势图叠加买入/卖出/分红点）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TxnMarker {
    pub date: String,
    pub txn_type: String, // buy / sell / dividend
    pub shares: f64,
    pub amount: f64,
}

/// 取某基金在某账户下的真实买卖/分红交易标记（不含 1970 合成基线）。
pub fn get_txn_markers(code: &str, account_id: i64) -> SqlResult<Vec<TxnMarker>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT txn_type, COALESCE(shares,0), amount, txn_date FROM transactions
             WHERE account_id = ?1 AND fund_code = ?2 AND txn_date != '1970-01-01'
               AND txn_type IN ('buy','sell','dividend')
             ORDER BY txn_date ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![account_id, code], |r| {
            Ok(TxnMarker {
                date: r.get(3)?,
                txn_type: r.get(0)?,
                shares: r.get(1)?,
                amount: r.get(2)?,
            })
        })?;
        rows.collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_temp_db() {
        let dir = std::env::temp_dir().join(format!("fundlens_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("FUNDLENS_DATA_DIR", dir.to_string_lossy().to_string());
        let _ = init_db();
    }

    #[test]
    fn account_and_txn_recompute() {
        init_temp_db();
        let acc = create_account("测试账户", "").unwrap();
        // 基线持仓：100 份，成本 1000（均价 10）
        set_baseline(acc, "000001", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 1000.0).abs() < 1e-6);
        // 入金不影响个基持仓
        add_transaction(acc, "deposit", None, None, 5000.0, None, "2026-01-01", "", None, "").unwrap();
        assert_eq!(list_holdings(Some(acc)).unwrap().len(), 1);
        // 卖出 40 份，金额 500（均价 10 → 减成本 400，剩 60 份 / 600）
        add_transaction(
            acc,
            "sell",
            Some("000001".to_string()),
            Some(40.0),
            500.0,
            None,
            "2026-01-02",
            "",
            None,
            "alipay",
        )
        .unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 60.0);
        assert!((hs[0].cost_amount - 600.0).abs() < 1e-6);
        // 快照落库
        record_snapshot(acc, "2026-01-02", 660.0, 600.0, 60.0, 60.0).unwrap();
        let snaps = list_snapshots(acc).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].total_market_value, 660.0);
    }

    #[test]
    fn import_txn_incremental_and_dividend() {
        init_temp_db();
        let acc = create_account("导入账户", "").unwrap();

        let buy = ImportTxn {
            fund_code: "000002".to_string(),
            fund_name: Some("测试基金".to_string()),
            txn_type: "buy".to_string(),
            shares: Some(100.0),
            amount: 1000.0,
            price: Some(10.0),
            txn_date: "2026-01-01".to_string(),
            txn_time: String::new(),
            note: None,
            platform: String::new(),
        };
        // 第一次导入批次 B1：买入 100 份 / 成本 1000
        let n = import_transactions(acc, &[buy.clone()], Some("B1".to_string())).unwrap();
        assert_eq!(n, 1);
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 1000.0).abs() < 1e-6);

        // 同批次 B1 重复导入（仍是 100 份买入）→ 幂等不叠加
        let _ = import_transactions(acc, &[buy.clone()], Some("B1".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 1000.0).abs() < 1e-6);

        // 同批次 B1 整批重导（买入 + 现金分红 50）：成本基数下调至 950，份额不变（累计收益准确包含分红）
        // 注意：批次为整体替换，故分红须与买入在同一批次调用中导入，否则会清掉同批买入。
        let div = ImportTxn {
            fund_code: "000002".to_string(),
            fund_name: None,
            txn_type: "dividend".to_string(),
            shares: None,
            amount: 50.0,
            price: None,
            txn_date: "2026-02-01".to_string(),
            txn_time: String::new(),
            note: Some("现金分红".to_string()),
            platform: String::new(),
        };
        let _ = import_transactions(acc, &[buy.clone(), div], Some("B1".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 950.0).abs() < 1e-6);

        // 不同批次 B2 增量合并：再买入 50 份 / 成本 600 → 150 份 / 1550
        let buy2 = ImportTxn {
            fund_code: "000002".to_string(),
            fund_name: None,
            txn_type: "buy".to_string(),
            shares: Some(50.0),
            amount: 600.0,
            price: Some(12.0),
            txn_date: "2026-03-01".to_string(),
            txn_time: String::new(),
            note: None,
            platform: String::new(),
        };
        let _ = import_transactions(acc, &[buy2], Some("B2".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 150.0);
        assert!((hs[0].cost_amount - 1550.0).abs() < 1e-6);

        // 同批次 B1 重导（仅含买入，不含分红）→ 替换 B1，但 B2 仍在，分红基线被移除
        let _ = import_transactions(acc, &[buy.clone()], Some("B1".to_string())).unwrap();
        let txns = list_transactions(Some(acc), None).unwrap();
        // B1 买入(1) + B2 买入(1) = 2 条，分红(属 B1 旧批次)已被 B1 重导清除
        assert_eq!(txns.len(), 2);
    }

    #[test]
    fn cost_series_and_markers() {
        init_temp_db();
        let acc = create_account("成本账户", "").unwrap();
        set_baseline(acc, "000003", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        add_transaction(acc, "sell", Some("000003".to_string()), Some(20.0), 220.0, None, "2026-02-01", "", None, "").unwrap();
        add_transaction(acc, "dividend", Some("000003".to_string()), None, 30.0, None, "2026-03-01", "", None, "").unwrap();
        // 成本序列：1970 基线不产出点；仅 2 条真实交易
        let cost = get_cost_series("000003", acc).unwrap();
        assert_eq!(cost.len(), 2);
        // 卖出 20 份（均价 10）→ 剩 80 份 / 800 成本
        assert_eq!(cost[0].date, "2026-02-01");
        assert!((cost[0].shares - 80.0).abs() < 1e-6);
        assert!((cost[0].cumulative_cost - 800.0).abs() < 1e-6);
        assert!((cost[0].unit_cost - 10.0).abs() < 1e-9);
        // 分红 30 → 成本 770，份额不变
        assert_eq!(cost[1].date, "2026-03-01");
        assert!((cost[1].cumulative_cost - 770.0).abs() < 1e-6);
        assert!((cost[1].shares - 80.0).abs() < 1e-6);
        // 标记：sell + dividend 共 2 条（不含 1970 基线）
        let markers = get_txn_markers("000003", acc).unwrap();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].txn_type, "sell");
        assert_eq!(markers[1].txn_type, "dividend");
        // nav_history 写入与升序读取
        let pts = vec![
            crate::data::NavPoint { date: "2026-03-01".into(), nav: 9.5, acc_nav: 9.5 },
            crate::data::NavPoint { date: "2026-03-02".into(), nav: 9.6, acc_nav: 9.6 },
        ];
        assert_eq!(upsert_nav_history("000003", &pts).unwrap(), 2);
        let got = get_nav_history("000003").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].date, "2026-03-01");
        assert!((got[0].nav - 9.5).abs() < 1e-9);
    }

    #[test]
    fn multi_platform_same_fund() {
        init_temp_db();
        let acc = create_account("多平台账户", "").unwrap();
        // 同一只 003095 在支付宝与京东金融分别持有（份额型）→ 必须成为两条独立持仓，而非互相覆盖
        set_baseline(acc, "003095", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "import").unwrap();
        set_baseline(acc, "003095", 200.0, 2600.0, 0.0, 0.0, 0.0, 0.0, "jd_finance", "import").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 2);
        let alipay = hs.iter().find(|h| h.platform == "alipay").expect("支付宝持仓缺失");
        let jd = hs.iter().find(|h| h.platform == "jd_finance").expect("京东金融持仓缺失");
        assert!((alipay.shares - 100.0).abs() < 1e-6);
        assert!((alipay.cost_amount - 1000.0).abs() < 1e-6);
        assert!((jd.shares - 200.0).abs() < 1e-6);
        assert!((jd.cost_amount - 2600.0).abs() < 1e-6);
        // 重新导入支付宝基线，不应破坏京东金融持仓（验证 write_baseline_conn 的 platform 限定删除）
        set_baseline(acc, "003095", 150.0, 1500.0, 0.0, 0.0, 0.0, 0.0, "alipay", "import").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 2);
        let jd = hs.iter().find(|h| h.platform == "jd_finance").expect("京东金融持仓被误删");
        assert!((jd.shares - 200.0).abs() < 1e-6);
    }

    #[test]
    fn est_cache_roundtrip_and_refresh() {
        init_temp_db();
        // 模拟一次刷新：写入两只基金的盘中估值
        let items = vec![
            (
                "003095".to_string(),
                crate::data::FundEstimate {
                    est_nav: 2.345,
                    est_change_pct: 0.0123,
                    prev_nav: 2.316,
                    gztime: "2026-08-16 14:30".to_string(),
                },
                1700000000,
            ),
            (
                "000001".to_string(),
                crate::data::FundEstimate {
                    est_nav: 1.111,
                    est_change_pct: -0.0045,
                    prev_nav: 1.116,
                    gztime: "2026-08-16 14:30".to_string(),
                },
                1700000000,
            ),
        ];
        save_est_cache(&items).unwrap();
        // 读回应命中两条
        let cached = load_est_cache(&["003095".to_string(), "000001".to_string()]).unwrap();
        assert_eq!(cached.len(), 2);
        let e = cached.get("003095").expect("003095 缓存缺失");
        assert!((e.est_nav - 2.345).abs() < 1e-9);
        assert!((e.est_change_pct - 0.0123).abs() < 1e-9);
        assert!((e.prev_nav - 2.316).abs() < 1e-9);
        assert_eq!(e.fetched_at, 1700000000);
        // to_estimate 还原一致
        let est = e.to_estimate();
        assert!((est.est_nav - 2.345).abs() < 1e-9);
        assert_eq!(est.gztime, "2026-08-16 14:30");
        // upsert 刷新：同一 code 覆盖（不应变成两条）
        let refresh = vec![(
            "003095".to_string(),
            crate::data::FundEstimate {
                est_nav: 2.400,
                est_change_pct: 0.0363,
                prev_nav: 2.316,
                gztime: "2026-08-16 15:00".to_string(),
            },
            1700000360,
        )];
        save_est_cache(&refresh).unwrap();
        let cached = load_est_cache(&["003095".to_string(), "000001".to_string()]).unwrap();
        assert_eq!(cached.len(), 2, "刷新不应新增行，仍是两基金");
        let e = cached.get("003095").unwrap();
        assert!((e.est_nav - 2.400).abs() < 1e-9, "刷新值应覆盖");
        assert_eq!(e.gztime, "2026-08-16 15:00");
        // 不存在的 code 不应命中
        let cached = load_est_cache(&["999999".to_string()]).unwrap();
        assert!(cached.is_empty());
    }
}

