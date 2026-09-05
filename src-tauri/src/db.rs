// 本地 SQLite 存储层（rusqlite bundled，零系统依赖）
// 表结构与 SPEC.md 第 6 节一致。此处实现 v0.1 核心表 + 初始化。
use rusqlite::{Connection, Result as SqlResult};
use std::sync::Mutex;
use once_cell::sync::Lazy;
use tauri::Manager;

static DB: Lazy<Mutex<Option<Connection>>> = Lazy::new(|| Mutex::new(None));

/// 记录 init_db 实际使用的数据库文件路径，供 db_file_path / 导出导入保持一致。
static DB_FILE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// 解析数据库文件所在目录（按优先级）：
/// 1. 环境变量 FUNDLENS_DATA_DIR
/// 2. Tauri 应用数据目录（推荐：GUI 从 Finder 启动时稳定且可写）
/// 3. $HOME/Library/Application Support/FundLens（兜底）
/// 注意：绝不能默认用当前工作目录 "." —— 从 Finder 启动 GUI 应用时工作目录是
/// "/"，会导致 /fundlens.db 不可写、init_db 静默失败、后续命令 unwrap None 连接而
/// 崩溃（表现为"意外退出"）。
fn resolve_db_dir(app: Option<&tauri::App>) -> std::path::PathBuf {
    if let Ok(d) = std::env::var("FUNDLENS_DATA_DIR") {
        return std::path::PathBuf::from(d);
    }
    if let Some(a) = app {
        if let Ok(d) = a.path().app_data_dir() {
            return d;
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("FundLens");
    }
    std::path::PathBuf::from(".")
}

fn db_path(app: Option<&tauri::App>) -> std::path::PathBuf {
    let dir = resolve_db_dir(app);
    let _ = std::fs::create_dir_all(&dir);
    dir.join("fundlens.db")
}

/// 当前活动数据库文件的绝对路径（供导出/导入与 UI 展示）。
/// 优先返回 init_db 实际使用的路径，保证与运行实例一致；未初始化时按兜底逻辑推导。
pub fn db_file_path() -> std::path::PathBuf {
    DB_FILE
        .get()
        .cloned()
        .unwrap_or_else(|| db_path(None))
}

/// 金融隐私数据：将数据库文件权限收紧为仅本人可读写（0600），避免裸放在数据目录被其他用户/进程读取。
/// Windows 无 Unix 权限模型，降级为无操作。
#[cfg(unix)]
fn harden_db_perms(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn harden_db_perms(_path: &std::path::Path) {}

pub fn init_db(app: Option<&tauri::App>) -> SqlResult<()> {
    let mut guard = DB.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let path = db_path(app);
    let conn = Connection::open(&path)?;
    // 开启外键约束（SQLite 默认关闭）。transactions.fund_code→funds(code) ON DELETE RESTRICT、
    // transactions.related_tx_id→transactions(id) ON DELETE SET NULL、positions.fund_code→funds
    // ON DELETE CASCADE 等约束只有开启后才会真正生效（防止孤儿交易/持仓、误删基金连带数据）。
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    // 金融隐私数据：数据库文件仅本人可读写（0600），避免裸放在数据目录被其他用户读取。
    harden_db_perms(&path);
    // 记录实际使用的路径，供 db_file_path / 导出导入保持一致
    let _ = DB_FILE.set(path.clone());
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

        -- 逐仓逐日估值物化表（P2：一表同时满足「成本曲线」与「逐仓逐日估值持久化」）。
        -- 以 positions.id 为主键维度（不因平台拆分而漂移）；每日日终对每条 active position upsert 一行，
        -- 由 compute_position_metrics 已算出的指标落库。成本曲线直接读 cost_amount/avg_cost/shares，无需单独表。
        CREATE TABLE IF NOT EXISTS position_daily (
            position_id   INTEGER NOT NULL REFERENCES positions(id) ON DELETE CASCADE,
            nav_date      TEXT NOT NULL,
            shares        REAL NOT NULL,
            avg_cost      REAL NOT NULL,
            cost_amount   REAL NOT NULL,
            official_nav  REAL,
            est_nav       REAL,
            reference_nav REAL,
            market_value  REAL NOT NULL,
            day_pnl_act   REAL NOT NULL,
            day_pnl_est   REAL NOT NULL,
            day_pnl_pct_act REAL NOT NULL,
            day_pnl_pct_est REAL NOT NULL,
            is_estimated  INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (position_id, nav_date)
        );
        CREATE INDEX IF NOT EXISTS idx_position_daily_date ON position_daily(nav_date);

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
            txn_type TEXT NOT NULL
                CHECK (txn_type IN ('buy','sell','dividend','reinvest_dividend','deposit','withdraw','adjust')),
            fund_code TEXT REFERENCES funds(code) ON DELETE RESTRICT,  -- 补外键：删基金前须先清其流水
            related_tx_id INTEGER REFERENCES transactions(id) ON DELETE SET NULL,  -- 分红↔红利再投/转换配对
            shares REAL,
            amount REAL NOT NULL,     -- 买卖=成交金额；出入金=现金流
            price REAL,
            txn_date TEXT NOT NULL,   -- YYYY-MM-DD（交易日）
            txn_time TEXT,            -- HH:MM（交易日具体时间，用于判断 15:00 前后净值结算日；可选）
            note TEXT,
            source TEXT NOT NULL DEFAULT 'manual',  -- import / manual
            source_ref TEXT,          -- 导入批次标识（增量合并用）
            platform TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- 组合每日市值快照（日报/周报/月报/年报/盈亏日历的数据源）。
        -- P3 增厚：新增 platform 维度（''=全平台聚合）、total_return_pct（累计收益率）、
        -- max_drawdown_pct（最大回撤）；唯一键由 (account_id, snapshot_date) 升级为
        -- (account_id, platform, snapshot_date)，支持按平台分别留存快照。
        -- ⚠️ 必须 IF NOT EXISTS（不能 DROP 重建）：快照是活数据，每次启动 DROP 会清空
        -- 全部历史快照，导致日报/周报/日历永远只有当天 1 条（2026-08-25 线上 bug）。
        -- 旧库缺列由下方 ensure_column 幂等补齐。
        CREATE TABLE IF NOT EXISTS snapshots (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER NOT NULL DEFAULT 1,  -- 0 = 全部账户聚合
            platform TEXT NOT NULL DEFAULT '',
            snapshot_date TEXT NOT NULL,
            total_market_value REAL NOT NULL,
            total_cost REAL NOT NULL,
            total_pnl REAL NOT NULL,
            day_pnl REAL NOT NULL,
            total_return_pct REAL NOT NULL DEFAULT 0,
            max_drawdown_pct REAL NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(account_id, platform, snapshot_date)
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
    // ─────────────────────────────────────────────────────────────
    // 策略信号层表（valuation_grid 决策引擎移植，2026-09-06，方案 §6.1）
    // 原则：strategy 层只读 positions/transactions，写仅限 grid_* 新表；
    // 信号只做「建议」，绝不触碰持仓自动变更（v9 铁律）。
    // ─────────────────────────────────────────────────────────────
    conn.execute_batch(
        r#"
        -- 策略基金配置（每 fund_code 一行，跨平台聚合 —— OD-1）
        CREATE TABLE IF NOT EXISTS grid_funds (
            fund_code TEXT PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0,
            max_position REAL,
            vol_sensitivity REAL,
            fee_schedule TEXT,
            cooldown_sell_date TEXT,
            peak_nav REAL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- 当日信号（同日同源覆盖：UNIQUE(fund_code, signal_date, source)）
        CREATE TABLE IF NOT EXISTS grid_signal (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT NOT NULL,
            signal_date TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'estimation',
            signal_name TEXT,
            action TEXT NOT NULL,
            priority REAL,
            sub_priority REAL,
            amount REAL,
            sell_pct REAL,
            est_change_pct REAL,
            current_nav REAL,
            total_profit_pct REAL,
            confidence REAL,
            reason TEXT,
            alert INTEGER DEFAULT 0,
            fifo_plan TEXT,
            executed INTEGER DEFAULT 0,
            executed_txn_id INTEGER,
            UNIQUE(fund_code, signal_date, source)
        );
        CREATE INDEX IF NOT EXISTS idx_grid_signal_date ON grid_signal(signal_date);

        -- 信号历史（每码保留 90 条；T+3/5/10 outcome 回填 —— P1）
        CREATE TABLE IF NOT EXISTS grid_signal_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT NOT NULL,
            signal_date TEXT NOT NULL,
            source TEXT NOT NULL DEFAULT 'estimation',
            signal_name TEXT,
            action TEXT,
            priority REAL,
            reason TEXT,
            amount REAL,
            sell_pct REAL,
            today_change REAL,
            current_nav REAL,
            nav_at_signal REAL,
            total_profit_pct REAL,
            outcome_t3 REAL,
            outcome_t5 REAL,
            outcome_t10 REAL,
            executed INTEGER DEFAULT 0,
            UNIQUE(fund_code, signal_date, source)
        );
        CREATE INDEX IF NOT EXISTS idx_grid_signal_history_fund ON grid_signal_history(fund_code, signal_date);

        -- 延迟回补挂单（P2）
        CREATE TABLE IF NOT EXISTS grid_pending_rebuy (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            fund_code TEXT NOT NULL,
            created_date TEXT,
            expire_date TEXT,
            trigger_nav REAL,
            amount REAL,
            ratio REAL,
            source_signal TEXT,
            signal_label TEXT,
            sell_nav REAL,
            status TEXT NOT NULL DEFAULT 'pending',
            triggered_date TEXT,
            triggered_batch_id INTEGER
        );

        -- 全局设置（regime 自动/手动、可投现金预算等 k/v）
        CREATE TABLE IF NOT EXISTS grid_settings (
            k TEXT PRIMARY KEY,
            v TEXT
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
    // v4：funds 表落地「上一交易日净值 prev_nav」与「官方净值日期 nav_date」。
    // prev_nav 用于业界口径的当日收益（份额 ×(official_nav − prev_nav)）；
    // nav_date 用于判定 prev_nav 是否需要随净值刷新滑动（见 update_fund_nav）。
    ensure_column(&conn, "funds", "prev_nav", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "funds", "nav_date", "TEXT")?;
    ensure_column(&conn, "funds", "track_index", "TEXT")?;
    // 历史数据回填：fund_type 早期允许 NULL（OCR/种子导入未写入），统一置空串，避免读取崩溃
    conn.execute("UPDATE funds SET fund_type='' WHERE fund_type IS NULL", [])?;
    // 索引：positions 按账户查询、transactions 按账户/基金查询
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_positions_account ON positions(account_id)",
        [],
    )?;
    // 注意：不再创建 (account_id, fund_code) 两列唯一索引。v7 起持仓下沉到「同基金多平台」，
    // 同一基金可有多行（platform 不同），两列索引会与多平台模型冲突并导致 init_db 失败
    // （真实库中 008923/017811/260112 等即存在不同 platform 的同基金多行）。
    // 唯一性由下方的 (account_id, fund_code, platform) 三列索引保证（见第 239 行）。
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_txn_account ON transactions(account_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_txn_fund ON transactions(fund_code)",
        [],
    )?;
    // P2：补齐缺失索引（幂等）。disclosures(fund_code)、positions(fund_code)（现有 uq 以 account_id 开头，
    // fund_code 单独查询全扫）、nav_history(nav_date)（PK 仅覆盖 fund_code 前缀）。
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_disclosures_fund ON disclosures(fund_code)",
        [],
    )?;
    // 多期披露持仓：唯一索引（同基金+期次+股票唯一，重复抓取幂等）+ 期次索引（历史查询）。
    ensure_disclosure_history_schema(&conn)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_positions_fund ON positions(fund_code)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_nav_history_date ON nav_history(nav_date)",
        [],
    )?;
    // P0：用 funds 现有 official_nav/nav_date 给 nav_history 补种子行，使「从 nav_history 派生前一交易日
    // 净值」在存量数据上即可成立（仅当 nav_history 尚无该 (fund_code,nav_date) 行时插入，幂等）。
    conn.execute(
        "INSERT OR IGNORE INTO nav_history(fund_code, nav_date, nav)
         SELECT code, nav_date, official_nav FROM funds
          WHERE nav_date IS NOT NULL AND nav_date <> '' AND official_nav > 0",
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
    // 存量数据回填：将既有持仓/流水挂回 funds.platform（最后导入平台），保证升级前数据不丢、且仍可单平台过滤。
    // 防护：空平台持仓若与同 (账户,基金,目标平台) 的既有持仓撞 uq_positions_account_fund_platform 唯一键，
    // 先删除该幻影再回填——否则 UPDATE 触发唯一冲突 → init_db 整体失败 → 全应用读不到数据（记账 bug 曾触发）。
    // 注意：SQLite 的 DELETE 不支持目标表别名，故用 rowid 子查询（内层 positions p 为子查询别名，合法）。
    conn.execute(
        "DELETE FROM positions \
         WHERE platform = '' \
           AND rowid IN ( \
             SELECT p.rowid FROM positions p \
             WHERE p.platform = '' \
               AND EXISTS ( \
                 SELECT 1 FROM positions q \
                 WHERE q.account_id = p.account_id AND q.fund_code = p.fund_code \
                   AND q.platform = (SELECT f.platform FROM funds f WHERE f.code = p.fund_code) \
                   AND q.rowid <> p.rowid \
               ) \
           )",
        [],
    )?;
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
    // P3：既有库 transactions 表无 related_tx_id / 外键 / CHECK（旧 schema 由上面 IF NOT EXISTS
    // 兜底创建，不会自动升级），此处守卫式重建：仅当 related_tx_id 列缺失时执行，可重跑。
    {
        let has_related: bool = conn
            .query_row(
                "SELECT 1 FROM pragma_table_info('transactions') WHERE name = 'related_tx_id'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !has_related {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS transactions_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER NOT NULL DEFAULT 1,
                    txn_type TEXT NOT NULL
                        CHECK (txn_type IN ('buy','sell','dividend','reinvest_dividend','deposit','withdraw')),
                    fund_code TEXT REFERENCES funds(code) ON DELETE RESTRICT,
                    related_tx_id INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
                    shares REAL,
                    amount REAL NOT NULL,
                    price REAL,
                    txn_date TEXT NOT NULL,
                    txn_time TEXT,
                    note TEXT,
                    source TEXT NOT NULL DEFAULT 'manual',
                    source_ref TEXT,
                    platform TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO transactions_new(id,account_id,txn_type,fund_code,shares,amount,price,
                        txn_date,txn_time,note,source,source_ref,platform,created_at)
                    SELECT id,account_id,txn_type,fund_code,shares,amount,price,
                        txn_date,txn_time,note,source,source_ref,platform,created_at
                        FROM transactions;
                DROP TABLE transactions;
                ALTER TABLE transactions_new RENAME TO transactions;
                "#,
            )?;
        }
    }
    // P4：txn_type 增加 'adjust'（手工调整/盘点单）。SQLite 改 CHECK 需重建表：
    // 仅当建表 SQL 不含 'adjust' 时执行（幂等可重跑）。
    {
        let ddl: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='transactions'",
                [],
                |r| r.get(0),
            )
            .ok();
        let need_adjust = ddl.as_deref().map_or(false, |s| !s.contains("'adjust'"));
        if need_adjust {
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS transactions_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id INTEGER NOT NULL DEFAULT 1,
                    txn_type TEXT NOT NULL
                        CHECK (txn_type IN ('buy','sell','dividend','reinvest_dividend','deposit','withdraw','adjust')),
                    fund_code TEXT REFERENCES funds(code) ON DELETE RESTRICT,
                    related_tx_id INTEGER REFERENCES transactions(id) ON DELETE SET NULL,
                    shares REAL,
                    amount REAL NOT NULL,
                    price REAL,
                    txn_date TEXT NOT NULL,
                    txn_time TEXT,
                    note TEXT,
                    source TEXT NOT NULL DEFAULT 'manual',
                    source_ref TEXT,
                    platform TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO transactions_new(id,account_id,txn_type,fund_code,shares,amount,price,
                        txn_date,txn_time,note,source,source_ref,platform,created_at)
                    SELECT id,account_id,txn_type,fund_code,shares,amount,price,
                        txn_date,txn_time,note,source,source_ref,platform,created_at
                        FROM transactions;
                DROP TABLE transactions;
                ALTER TABLE transactions_new RENAME TO transactions;
                "#,
            )?;
        }
    }
    // v8：snapshots 增加当日估算收益/估算市值列（日报/周报/月报/年报的估算统计数据源）。
    // 旧库历史快照该两列默认为 0（估算统计自启用起累积），幂等加列。
    ensure_column(&conn, "snapshots", "day_pnl_est", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "snapshots", "est_market_value", "REAL NOT NULL DEFAULT 0")?;
    // snapshots 结构兜底：基线建表已改 IF NOT EXISTS（不再每次 DROP 重建），
    // 旧库若缺 P3 增厚列（platform/total_return_pct/max_drawdown_pct）在此幂等补齐，
    // 保证老库升级后 record_snapshot/list_snapshots 正常（2026-08-25 修复）。
    ensure_column(&conn, "snapshots", "platform", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&conn, "snapshots", "total_return_pct", "REAL NOT NULL DEFAULT 0")?;
    ensure_column(&conn, "snapshots", "max_drawdown_pct", "REAL NOT NULL DEFAULT 0")?;
    *guard = Some(conn);
    // 已持有 DB 锁：下方直接用 guard 内的 conn，绝不能再调 with_conn（非可重入锁→自死锁）。
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

    // 【持仓模型 v9（2026-09-06）】positions 为权威、流水为纯账本，去掉「流水重放重建持仓」：
    // - 不再把存量持仓回填为 1970 合成基线流水（避免污染 FundVal 等外部系统镜像导入的流水账本，
    //   以及「截图导入不产生流水、改持仓不产生流水」的新语义）；
    // - 不再启动时重放流水重建 positions（旧 recompute 会以流水净额覆盖镜像持仓，
    //   曾把 alipay 153 组镜像打成 152 组——本次改造的直接诱因）。
    // 启动仅回填「待净值」流水份额（OCR 金额导入且此前本地无确认日净值），使账本展示口径最终一致；
    // 回填成功后会把对应增量应用到持仓（见 backfill_pending_txn_shares_conn）；回填失败不阻塞启动。
    let _ = backfill_pending_txn_shares_conn(c, 1);
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

/// 多期披露持仓存储：去重 + 唯一索引 + 期次索引（幂等，绝不 DROP 活数据表）。
///
/// 唯一索引使「重复抓取同一期次」幂等（同一基金+期次+股票只保留一行），是多期共存的前提。
/// 但存量库若已有重复行，`CREATE UNIQUE INDEX` 会直接失败，故建索引前先按
/// (fund_code, report_period, stock_code) 仅保留 id 最小的一行；去重只在索引缺失时执行一次。
fn ensure_disclosure_history_schema(conn: &Connection) -> SqlResult<()> {
    let has_uq: bool = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'index'
             AND name = 'uq_disclosures_fund_period_stock'",
            [],
            |_| Ok(true),
        )
        .unwrap_or(false);
    if !has_uq {
        conn.execute(
            "DELETE FROM disclosures WHERE id NOT IN (
                 SELECT MIN(id) FROM disclosures GROUP BY fund_code, report_period, stock_code
             )",
            [],
        )?;
    }
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_disclosures_fund_period_stock
             ON disclosures(fund_code, report_period, stock_code)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_disclosures_fund_period
             ON disclosures(fund_code, report_period)",
        [],
    )?;
    Ok(())
}

pub fn with_conn<F, T>(f: F) -> SqlResult<T>
where
    F: FnOnce(&Connection) -> SqlResult<T>,
{
    let guard = DB.lock().unwrap();
    let conn = match guard.as_ref() {
        Some(c) => c,
        None => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("数据库未初始化，请先调用 init_db".into()),
            ))
        }
    };
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
    /// 真实跟踪指数行情符号（如 "hkHSHCI" / "sh000300"）。优先于从名称瞎猜，用于「指数代理」估值。
    /// 导入路径暂未填充（恒为空），此时由 data::resolve_tracked_index 按名称/类型推断。
    pub track_index: String,
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
            "SELECT code, name, platform, official_nav, report_period, disclosure_type, fund_type, track_index, valuation_applicable FROM funds ORDER BY code",
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
                track_index: r.get::<usize, Option<String>>(7)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(8).unwrap_or(1) != 0,
            })
        })?;
        rows.collect()
    })
}

/// 单只基金的「官方净值是否已取」状态（供批量刷新官方净值判断）。
pub struct FundNavStatus {
    pub code: String,
    /// 官方净值发布日期（YYYY-MM-DD）；未取到过为 None
    pub nav_date: Option<String>,
    /// 基金类型码（缺失则为空串）
    pub fund_type: String,
}

/// 列出全部基金及其官方净值日期与类型（供 refresh_official_nav 判断「今日是否已取到」）。
/// 仅取刷新所需的三个字段，避免加载完整 FundRow。
pub fn list_funds_with_nav_date() -> SqlResult<Vec<FundNavStatus>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT code, nav_date, COALESCE(fund_type,'') FROM funds ORDER BY code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FundNavStatus {
                code: r.get(0)?,
                nav_date: r.get::<usize, Option<String>>(1)?,
                fund_type: r.get::<usize, Option<String>>(2)?.unwrap_or_default(),
            })
        })?;
        rows.collect()
    })
}

/// 仅写入/更新基金元数据（不写持仓）。【v9】positions 为权威：由 set_baseline / update_position_inplace /
/// 交易流水增量 直接维护，流水为纯账本（不再重放派生持仓）。
/// 注意：必须使用 ON CONFLICT DO UPDATE 而非 INSERT OR REPLACE——开启外键后，REPLACE 会先 DELETE
/// 旧行再 INSERT，触发 positions.fund_code 的 ON DELETE CASCADE 把该基金的全部持仓连带删除（数据丢失）。
/// platform 仅在既有值为空时才被覆盖（COALESCE(NULLIF(...))），避免无条件覆盖 funds.platform
/// （P3：funds.platform 降级为非权威提示，持仓/总览/统计一律以 positions.platform 为准）。
pub fn insert_fund(f: &FundRow) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO funds(code,name,platform,official_nav,report_period,disclosure_type,fund_type,track_index,valuation_applicable)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(code) DO UPDATE SET
               name=excluded.name,
               official_nav=excluded.official_nav,
               report_period=excluded.report_period,
               disclosure_type=excluded.disclosure_type,
               fund_type=excluded.fund_type,
               track_index=excluded.track_index,
               valuation_applicable=excluded.valuation_applicable,
               platform=COALESCE(NULLIF(platform,''), excluded.platform)",
            rusqlite::params![
                f.code, f.name, f.platform, f.official_nav, f.report_period, f.disclosure_type,
                f.fund_type, f.track_index, f.valuation_applicable
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

/// 更新官方净值、基金类型与估值适用性，并按 nav_date 推进维护「上一交易日净值 prev_nav」。
///
/// prev_nav 语义（业界口径）：始终等于「当前 official_nav 对应交易日的前一个交易日的官方净值」，
/// 即当日收益 = 份额 ×(official_nav − prev_nav)。维护规则：
/// - 新 nav_date 严格大于已存 nav_date（说明跨到了更新的一个净值日）→ 旧 official_nav 滑落为 prev_nav，
///   新 official_nav = nav，nav_date = 新日期。
/// - 同日期重复刷新（盘中官方净值尚未发布，nav_date 不变）或日期为空/更早 → 不滑动，仅刷新
///   official_nav / fund_type / applicable / nav_date（nav_date 为空时保留旧值），prev_nav 维持不变。
/// - 首次写入（prev_nav 仍为 0）且无跨日 → prev_nav 保持 0，由上层用 est_cache.prev_nav（gsz 的 dwjz）
///   兜底作为盘中基准。
/// 更新官方净值、基金类型与估值适用性，并按 nav_date 推进维护「上一交易日净值 prev_nav」。
/// 调用方若已从接口显式拿到昨日净值（`prev_nav`），可传入非 None 值直接覆盖；否则按旧 nav_date 滑动规则维护。
pub fn update_fund_nav(
    code: &str,
    nav: f64,
    fund_type: &str,
    applicable: bool,
    nav_date: &str,
    prev_nav: Option<f64>,
) -> SqlResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        let prev: (f64, f64, Option<String>) = tx
            .query_row(
                "SELECT official_nav, COALESCE(prev_nav,0), nav_date FROM funds WHERE code=?1",
                [code],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap_or((nav, 0.0, None));
        let (old_nav, old_prev, old_date) = prev;
        let (new_nav, new_prev, new_date) = match old_date {
            Some(d) if !d.is_empty() && !nav_date.is_empty() && nav_date > d.as_str() => {
                // 跨到更新的净值日：旧净值滑落为 prev_nav
                (nav, old_nav, nav_date.to_string())
            }
            _ => {
                // 同日期/空日期/更早：不滑动；nav_date 为空则保留旧值
                let date = if nav_date.is_empty() {
                    old_date.unwrap_or_default()
                } else {
                    nav_date.to_string()
                };
                (nav, old_prev, date)
            }
        };
        // 调用方显式提供昨日净值（来自接口 pageSize=2）时直接覆盖，保证官方接口可用时 prev_nav 为真实昨收。
        let new_prev = prev_nav.filter(|p| *p > 0.0).unwrap_or(new_prev);
        tx.execute(
            "UPDATE funds SET official_nav=?2, fund_type=?3, valuation_applicable=?4, prev_nav=?5, nav_date=?6 WHERE code=?1",
            rusqlite::params![code, new_nav, fund_type, applicable, new_prev, new_date],
        )?;
        tx.commit()?;
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

/// 写入单条披露持仓。同 (基金, 期次, 股票) 已存在时更新占比与名称——真正的 UPSERT，
/// 保证重复抓取同一期次不会叠加出重复行（依赖 uq_disclosures_fund_period_stock 唯一索引）。
/// 保留旧期次不动，是多期历史共存的基础。
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
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(fund_code,report_period,stock_code)
             DO UPDATE SET weight = excluded.weight,
                           stock_name = excluded.stock_name,
                           disclosure_type = excluded.disclosure_type,
                           fetched_at = datetime('now')",
            rusqlite::params![fund_code, stock_code, stock_name, weight, report_period, disclosure_type],
        )?;
        Ok(())
    })
}

/// 用一批持仓整体替换某基金的**指定期次**（事务内：先删该期次，再写入）。
/// 与 delete_disclosures（清空全部期次）区分：本函数只动目标期次，历史期次完整保留。
pub fn replace_disclosure_period(
    fund_code: &str,
    report_period: &str,
    holdings: &[crate::valuation::DisclosedHolding],
) -> SqlResult<usize> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM disclosures WHERE fund_code = ?1 AND report_period = ?2",
            rusqlite::params![fund_code, report_period],
        )?;
        for h in holdings {
            tx.execute(
                "INSERT INTO disclosures(fund_code,stock_code,stock_name,weight,report_period,disclosure_type)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![
                    fund_code,
                    h.stock_code,
                    h.stock_name,
                    h.weight,
                    report_period,
                    h.disclosure_type
                ],
            )?;
        }
        tx.commit()?;
        Ok(holdings.len())
    })
}

// ===================== 披露持仓：多期历史 + 期次归一化 =====================

/// 报告期字符串 → 可排序整数（year * 10 + quarter）。
/// 兼容库里并存的两种写法：东财标题解析出的「2026年2季度」与回退格式「2026Q2」。
/// 无法解析时返回 i64::MIN，排序自然沉底，不参与「最新期」判定。
pub fn period_key(period: &str) -> i64 {
    let chars: Vec<char> = period.chars().collect();
    let mut year: Option<i64> = None;
    if chars.len() >= 4 {
        for i in 0..=(chars.len() - 4) {
            if chars[i..i + 4].iter().all(|c| c.is_ascii_digit()) {
                if let Ok(y) = chars[i..i + 4].iter().collect::<String>().parse::<i64>() {
                    year = Some(y);
                    break;
                }
            }
        }
    }
    match (year, quarter_of_period(&chars)) {
        (Some(y), Some(q)) => y * 10 + q,
        // 只有年份没有季度（如「2026年」）时季度记 0，保证同一年内排在最旧
        (Some(y), None) => y * 10,
        (None, _) => i64::MIN,
    }
}

/// 从期次串解析季度（1~4）：优先 `Qn` 写法，其次在「季」字前回看至多 3 个字符（数字或中文数字）。
fn quarter_of_period(chars: &[char]) -> Option<i64> {
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i] == 'Q' || chars[i] == 'q' {
            if let Some(d) = chars[i + 1].to_digit(10) {
                if (1..=4).contains(&d) {
                    return Some(d as i64);
                }
            }
        }
    }
    if let Some(pos) = chars.iter().position(|c| *c == '季') {
        for c in chars[pos.saturating_sub(3)..pos].iter().rev() {
            if let Some(d) = c.to_digit(10) {
                if (1..=4).contains(&d) {
                    return Some(d as i64);
                }
            }
            let cn = match c {
                '一' => Some(1),
                '二' => Some(2),
                '三' => Some(3),
                '四' => Some(4),
                _ => None,
            };
            if let Some(q) = cn {
                return Some(q);
            }
        }
    }
    None
}

/// 从若干期次串中挑出最新一期（按 period_key 降序，同键时按字符串序保证结果稳定）。
fn pick_latest_period<'a>(periods: &[&'a str]) -> Option<&'a str> {
    periods
        .iter()
        .max_by(|a, b| period_key(a).cmp(&period_key(b)).then_with(|| a.cmp(b)))
        .copied()
}

/// 列出某基金已入库的全部报告期，按从旧到新排序。
pub fn list_disclosure_periods(fund_code: &str) -> SqlResult<Vec<String>> {
    with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT DISTINCT report_period FROM disclosures WHERE fund_code = ?1")?;
        let mut periods: Vec<String> = stmt
            .query_map([fund_code], |r| r.get::<usize, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        periods.sort_by(|a, b| period_key(a).cmp(&period_key(b)).then_with(|| a.cmp(b)));
        Ok(periods)
    })
}

/// 取某基金指定报告期的持仓（按占净值降序）。用于历史期次查看与「较上期」对比。
pub fn list_disclosures_of_period(
    fund_code: &str,
    period: &str,
) -> SqlResult<Vec<crate::valuation::DisclosedHolding>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT stock_code, stock_name, weight, report_period, disclosure_type FROM disclosures
             WHERE fund_code = ?1 AND report_period = ?2 ORDER BY weight DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![fund_code, period], |r| {
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

/// 取某基金**最新报告期**的持仓（估值唯一入口）。
///
/// 语义变更（多期历史改造）：disclosures 表改为保留多期历史后，估值只能吃最新一期——
/// 若把各期叠加，覆盖度会远超 100%、估算净值彻底错乱。所有估值入口都走本函数；
/// 历史期次仅用于「较上期」对比展示，不参与估值。
pub fn list_disclosures(fund_code: &str) -> SqlResult<Vec<crate::valuation::DisclosedHolding>> {
    let periods = list_disclosure_periods(fund_code)?;
    let refs: Vec<&str> = periods.iter().map(|s| s.as_str()).collect();
    match pick_latest_period(&refs) {
        Some(p) => list_disclosures_of_period(fund_code, p),
        None => Ok(Vec::new()),
    }
}

/// 一次性批量拉取全部基金的**最新报告期**持仓，按 (fund_code, 持仓) 返回。
/// 用于持仓总览：避免对每只基金分别 list_disclosures 造成的 N 次 DB 往返。
/// 与 list_disclosures 同口径：每只基金只取其最新期次。
pub fn list_disclosures_batch() -> SqlResult<Vec<(String, crate::valuation::DisclosedHolding)>> {
    let all = with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fund_code, stock_code, stock_name, weight, report_period, disclosure_type \
             FROM disclosures",
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
        rows.collect::<Result<Vec<_>, _>>()
    })?;

    // 按基金分组 → 只保留最新期次 → 按 (基金升序, 占比降序) 输出，保持原有顺序契约。
    let mut by_fund: std::collections::HashMap<String, Vec<crate::valuation::DisclosedHolding>> =
        std::collections::HashMap::new();
    for (code, h) in all {
        by_fund.entry(code).or_default().push(h);
    }
    let mut out: Vec<(String, crate::valuation::DisclosedHolding)> = Vec::new();
    for (code, hs) in by_fund {
        // 先算出最新期次并收进内层作用域，让对 hs 的借用在此结束，之后才能安全地 move hs
        let latest: String = {
            let refs: Vec<&str> = hs.iter().map(|h| h.report_period.as_str()).collect();
            match pick_latest_period(&refs) {
                Some(p) => p.to_string(),
                None => continue,
            }
        };
        for h in hs.into_iter().filter(|h| h.report_period == latest) {
            out.push((code.clone(), h));
        }
    }
    out.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            b.1.weight
                .partial_cmp(&a.1.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(out)
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

/// 一笔基金盘中实时估值的持久化形态（与 data::FundEstimate 对应的盘中估算字段 + fetched_at）。
/// 注意：P0 起 est_cache 不再持久化 prev_nav（昨收基准）——该值只由 funds.prev_nav（权威，来自官方
/// 接口显式昨收）单一存储，缺失时回退到 nav_history 派生，杜绝 est_cache.prev_nav 污染基准（006503 案例）。
#[derive(Clone)]
pub struct CachedEst {
    pub est_nav: f64,
    pub est_change_pct: f64,
    pub gztime: String,
    pub fetched_at: i64,
}

impl CachedEst {
    /// 还原为上游估值结构（供估值引擎使用）。prev_nav 字段保留为 0（est_cache 已不再存储，落库时被忽略），
    /// 估值基准统一走 funds.prev_nav / nav_history 派生路径。
    pub fn to_estimate(&self) -> crate::data::FundEstimate {
        crate::data::FundEstimate {
            est_nav: self.est_nav,
            est_change_pct: self.est_change_pct,
            prev_nav: 0.0,
            gztime: self.gztime.clone(),
        }
    }
}

/// 批量读取估值缓存（仅返回存在且无过期判定由调用方做；此处直接取所有命中行）。
pub fn load_est_cache(codes: &[String]) -> SqlResult<std::collections::HashMap<String, CachedEst>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fund_code, est_nav, est_change_pct, gztime, fetched_at \
             FROM est_cache WHERE fund_code = ?1",
        )?;
        let mut map: std::collections::HashMap<String, CachedEst> = std::collections::HashMap::new();
        for c in codes {
            let mut rows = stmt.query_map([c], |r| {
                Ok(CachedEst {
                    est_nav: r.get(1)?,
                    est_change_pct: r.get(2)?,
                    gztime: r.get(3)?,
                    fetched_at: r.get(4)?,
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
/// 仅持久化盘中估算字段（est_nav/est_change_pct/gztime/fetched_at）；prev_nav 不再落库。
pub fn save_est_cache(
    items: &[(String, crate::data::FundEstimate, i64)],
) -> SqlResult<()> {
    with_conn(|conn| {
        let tx = conn.unchecked_transaction()?;
        for (code, est, fetched_at) in items {
            tx.execute(
                "INSERT INTO est_cache(fund_code, est_nav, est_change_pct, gztime, fetched_at) \
                 VALUES(?1,?2,?3,?4,?5) \
                 ON CONFLICT(fund_code) DO UPDATE SET \
                   est_nav=?2, est_change_pct=?3, gztime=?4, fetched_at=?5",
                rusqlite::params![
                    code,
                    est.est_nav,
                    est.est_change_pct,
                    est.gztime,
                    fetched_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    })
}

/// 从 nav_history 取 code 在 ref_date 之前最近一个交易日的净值，作为其 prev_nav（前一交易日收盘净值）。
/// 无历史则返回 None，上层保留 funds.prev_nav 种子（来自官方接口显式昨收）。这是 P0 消除 est_cache.prev_nav
/// 双源后的唯一回退来源，保证基准只来自「官方净值 + 历史净值」两路权威数据，不被估算接口污染。
pub fn prev_nav_from_history(conn: &Connection, code: &str, ref_date: &str) -> Option<f64> {
    conn.query_row(
        "SELECT nav FROM nav_history
          WHERE fund_code=?1 AND nav_date < ?2
          ORDER BY nav_date DESC LIMIT 1",
        rusqlite::params![code, ref_date],
        |r| r.get::<_, f64>(0),
    )
    .ok()
}

/// 便捷封装：在全局连接上取历史派生前一交易日净值（供 commands 层无 conn 时调用）。
pub fn prev_nav_from_history_code(code: &str, ref_date: &str) -> Option<f64> {
    with_conn(|conn| Ok(prev_nav_from_history(conn, code, ref_date)))
        .ok()
        .flatten()
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

// ===================== 数据库备份 / 恢复（SPEC §F5 隐私架构：SQLite 可导出备份） =====================
//
// 采用 SQLite 官方在线备份 API（rusqlite::backup），相比「直接拷贝文件」有两个关键优势：
// 1) 在线一致：即使数据库正处于写入/有未提交事务，也能产出一份事务一致的快照，不会拷到半截文件；
// 2) 导出产物是干净的单文件（无 WAL/-journal 残留），可直接作为恢复源。
// 导出 = 把活动库在线备份到目标文件；导入 = 把备份文件在线恢复到活动库（活动连接保持有效）。

/// 导出当前活动数据库为一份独立备份文件（在线一致快照）。
/// dest 已存在则先删除，避免把数据备份进一个陈旧的/损坏的文件。
pub fn export_db_backup(dest: &std::path::Path) -> SqlResult<()> {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(dest);
    // 在线一致：底层 sqlite3_backup 即使数据库正处于写入，也能产出事务一致的快照，
    // 且产物是干净单文件（无 WAL/-journal 残留）。dest 由 backup 内部打开为目标库。
    let rc = with_conn(|live| live.backup(rusqlite::DatabaseName::Main, dest, None));
    // 备份产物同为金融隐私数据，同样收紧为仅本人可读写。
    harden_db_perms(dest);
    rc
}

/// 从备份文件在线恢复当前活动数据库（整体覆盖活动库内容）。
/// restore 需要可变借用活动连接；此处直接锁定全局 DB 以获取 &mut Connection。
/// 备份文件由 restore 内部以只读方式打开做基础校验，随后整个覆盖活动库。
pub fn import_db_backup(src: &std::path::Path) -> SqlResult<()> {
    let mut guard = DB.lock().unwrap();
    let live = match guard.as_mut() {
        Some(c) => c,
        None => {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("数据库未初始化，请先调用 init_db".into()),
            ))
        }
    };
    // 在线恢复：把备份文件整体覆盖进活动库（活动连接保持有效，后续查询读到恢复后的数据）。
    live.restore(
        rusqlite::DatabaseName::Main,
        src,
        None as Option<fn(rusqlite::backup::Progress)>,
    )
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
    pub txn_type: String, // buy / sell / dividend / reinvest_dividend / deposit / withdraw
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
    pub prev_nav: f64,
    pub nav_date: String,
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
    pub platform: String,
    pub snapshot_date: String,
    pub total_market_value: f64,
    pub total_cost: f64,
    pub total_pnl: f64,
    pub day_pnl: f64,
    pub total_return_pct: f64,
    pub max_drawdown_pct: f64,
    /// 当日估算收益（快照日盘中的本地自算/实时估值投影；历史快照缺省为 0）
    pub day_pnl_est: f64,
    /// 当日估算市值（按估算净值口径的组合市值；历史快照缺省为 0）
    pub est_market_value: f64,
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

/// 导入交易记录项（买卖/分红/红利再投增量导入）。txn_type 规范为 buy/sell/dividend/reinvest_dividend。
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
    // P2-14：金额统一取整到「分」。前端录入已按 份额×价格 取整到分（LedgerPage），
    // 若后端以未取整的 f64 直接入库，多次重放/展示会产生分位累计差（存储与显示不一致）。
    // 交易金额是真实货币值，取整到分不损失有效精度。
    let amount = (amount * 100.0).round() / 100.0;
    with_conn(|conn| {
        // 外键已开启：写入交易前确保关联基金存在，否则 fund_code 引用缺失会触发约束失败。
        if let Some(code) = &fund_code {
            conn.execute(
                "INSERT OR IGNORE INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable)
                 VALUES(?1,?2,?3,0,'',1)",
                rusqlite::params![code, code, platform],
            )?;
        }
        conn.execute(
            "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform, source)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'manual_txn')",
            rusqlite::params![account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform],
        )?;
        let id = conn.last_insert_rowid();
        // 【v9】流水只记录、持仓增量更新：不再全量重放。按交易类型/份额对当前持仓增量应用
        //（buy 加份额成本 / sell 按均价扣减 / dividend 成本还原 / reinvest_dividend 加份额；
        // 缺份额的买入/卖出留待净值回填后应用，deposit/withdraw 不影响个基）。
        if let Some(code) = &fund_code {
            apply_txn_to_position_conn(conn, account_id, txn_type, code, platform, shares, amount, false)?;
        }
        Ok(id)
    })
}

pub fn delete_transaction(id: i64) -> SqlResult<()> {
    with_conn(|conn| {
        // 【v9】先读出行（类型/基金/份额/金额/平台），删除后按反向规则撤销其持仓效果
        //（删 buy 扣回份额成本 / 删 sell 按当前均价回补 / 删 dividend 成本回补；deposit/adjust 等不触碰持仓）。
        let (account_id, txn_type, fund_code, shares, amount, platform): (
            i64,
            String,
            Option<String>,
            Option<f64>,
            f64,
            String,
        ) = conn.query_row(
            "SELECT account_id, txn_type, fund_code, shares, amount, COALESCE(platform,'') FROM transactions WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )?;
        conn.execute("DELETE FROM transactions WHERE id = ?1", [id])?;
        if let Some(code) = fund_code {
            apply_txn_to_position_conn(conn, account_id, &txn_type, &code, &platform, shares, amount, true)?;
        }
        Ok(())
    })
}

/// 设置某基金在某账户的「基线持仓」（截图导入或手动增改）。
/// 【v9 持仓模型】positions 为权威、流水为纯账本：基线只直写 positions（含支付宝展示字段），
/// **不产生任何流水**（历史版本曾写 1970-01-01 合成基线 buy / 删 import/manual_set 流水，
/// 已随「去掉流水重放」一并移除）。source 参数仅为兼容历史调用方，现忽略。
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
    let _ = source;
    with_conn(|conn| {
        write_position_conn(
            conn, account_id, code, platform, shares, cost_amount,
            holding_amount, holding_profit, yesterday_profit, profit_rate,
        )
    })
}

/// 手工调整持仓（盘点单/改份额成本）：**【v9】直接 UPDATE positions，不产生任何流水**。
/// 历史版本（2026-08-21 起）曾追加「手工修改」adjust 流水再由重放覆盖实现盘点留痕；
/// 用户定稿（2026-09-06）：改持仓不产生流水、positions 为权威，故改为就地覆盖份额/成本，
/// 保留既有展示字段（holding_amount/holding_profit/...）。shares<=0 视为清仓（删除持仓行）。
pub fn update_position_inplace(
    account_id: i64,
    code: &str,
    shares: f64,
    cost_amount: f64,
    platform: &str,
) -> SqlResult<()> {
    with_conn(|conn| {
        // 确保基金元数据存在（positions 外键指向 funds）：名称先用代码占位，
        // 后续刷新行情/导入会修正（与 write_position_conn 一致）。
        conn.execute(
            "INSERT OR IGNORE INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable) \
             VALUES(?1,?2,'',0,'',1)",
            rusqlite::params![code, code],
        )?;
        if shares > 0.0 {
            // 只覆盖份额与成本；holding_amount 等展示字段保留原值（计算口径见 compute_position_metrics）
            conn.execute(
                "INSERT INTO positions(account_id, fund_code, platform, shares, cost_amount) \
                 VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(account_id, fund_code, platform) DO UPDATE SET
                   shares=excluded.shares, cost_amount=excluded.cost_amount",
                rusqlite::params![account_id, code, platform, shares, cost_amount],
            )?;
        } else {
            // 清仓盘点：份额归 0 → 删除持仓行
            conn.execute(
                "DELETE FROM positions WHERE account_id=?1 AND fund_code=?2 AND platform=?3",
                rusqlite::params![account_id, code, platform],
            )?;
        }
        Ok(())
    })
}

/// 修改持仓成本价（单位成本）：目标持仓成本 = 当前份额 × 成本价。
/// 【v9 持仓模型】positions 为权威：直接更新 positions.cost_amount，**不产生流水、不改任何账本行**
///（历史版本会就地改写 adjust/1970 基线流水使重放后成本恰为目标值——机制已随重放移除而不再需要）。
pub fn update_position_cost(account_id: i64, code: &str, cost_price: f64, platform: &str) -> SqlResult<()> {
    with_conn(|conn| {
        let shares: f64 = conn.query_row(
            "SELECT shares FROM positions WHERE account_id=?1 AND fund_code=?2 AND platform=?3",
            rusqlite::params![account_id, code, platform],
            |r| r.get(0),
        )?;
        let target_basis = cost_price * shares;
        conn.execute(
            "UPDATE positions SET cost_amount=?1 WHERE account_id=?2 AND fund_code=?3 AND platform=?4",
            rusqlite::params![target_basis, account_id, code, platform],
        )?;
        Ok(())
    })
}

/// 解析某基金在某账户下既有持仓的平台（手动改仓未显式指定平台时回退用）。
/// 非空平台优先；同基金跨多平台持有时取首个非空平台；无任何持仓返回 None。
pub fn resolve_position_platform(account_id: i64, code: &str) -> Option<String> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT platform FROM positions WHERE account_id=?1 AND fund_code=?2 \
             ORDER BY (platform='') LIMIT 1",
        )?;
        let mut rows = stmt.query(rusqlite::params![account_id, code])?;
        if let Some(r) = rows.next()? {
            Ok(Some(r.get::<usize, String>(0)?))
        } else {
            Ok(None)
        }
    })
    .ok()
    .flatten()
}

/// 截图导入批次：逐基金**直写 positions 并写入基金元数据**（不产生任何流水，逐条即时生效，无需收尾重算）。
/// 【v9 持仓模型】用户定稿：持仓截图导入「已有的基金持仓直接更新持仓、不产生流水」——
/// 由 write_position_conn 以导入值覆盖同 (基金, 平台) 持仓行（含支付宝展示字段）。
pub fn import_positions_batch(account_id: i64, items: &[ImportHolding]) -> SqlResult<()> {
    with_conn(|conn| {
        for it in items {
            conn.execute(
                "INSERT INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable)
                 VALUES(?1,?2,?3,?4,'',1)
                 ON CONFLICT(code) DO UPDATE SET name=?2, official_nav=?4",
                rusqlite::params![it.code, it.name, it.platform, it.nav],
            )?;
            // 支付宝风格截图导入（shares=0、holding_amount>0）：按「导入当时净值」反推份额，
            // 使份额>0 走实时自算路径；成本基数取「持仓金额−持有收益」(导入时成本)，保留历史累计收益。
            // 份额型(京东/理财通)保持原有 shares×nav 成本口径不变。
            let (imp_shares, imp_cost) = if it.shares > 0.0 {
                (it.shares, it.shares * it.nav)
            } else if it.holding_amount > 0.0 && it.nav > 0.0 {
                let s = it.holding_amount / it.nav;
                let c = (it.holding_amount - it.holding_profit).max(0.0);
                (s, c)
            } else {
                (0.0, 0.0)
            };
            write_position_conn(
                conn,
                account_id,
                &it.code,
                &it.platform,
                imp_shares,
                imp_cost,
                it.holding_amount,
                it.holding_profit,
                it.yesterday_profit,
                it.profit_rate,
            )?;
        }
        Ok(())
    })
}

/// 增量导入交易记录（买卖/分红）。
/// 设计要点（扩展自 delete_disclosures 的「前置清空避免叠加」模式）：
///  - 若提供 source_ref：先删除本账户下同源批次(import_txn + 同 ref)的旧记录，再插入 → 同批次重复导入幂等、不叠加。
///  - 不同批次 / 手动流水(manual_txn) 互不干扰，实现「流水增量合并」。
///  - 【v9 持仓模型】流水只记录、持仓增量更新：批内每删/每改/每增一行流水，都先对持仓做一次
///    反向撤销（被替换/删除的旧行）或正向应用（新行），**不再收尾全量重放**。因此任何时刻
///    positions 都等于「镜像/手动基线 + 已录入且未被删除的全部流水」的逐笔增量结果。
///  - 同基金真实流水导入不再移除截图/手动基线流水（v9 基线本就不产生流水，二者天然无冲突）。
/// 精确取 nav_history 中 code 在指定日期（YYYY-MM-DD）的净值。无则 None。
fn nav_on_date(conn: &Connection, code: &str, date: &str) -> Option<f64> {
    conn.query_row(
        "SELECT nav FROM nav_history WHERE fund_code=?1 AND nav_date=?2",
        rusqlite::params![code, date],
        |r| r.get::<_, f64>(0),
    )
    .ok()
}

/// 基金交易「确认净值日期」：以 A 股 15:00 为界（申购/赎回按此规则确认净值）。
/// - txn_time 为空或 < "15:00" → 确认日 = txn_date（当日）
/// - txn_time >= "15:00" → 确认日 = 下一 A 股交易日（当日 15:00 后下单按次日净值确认）
fn resolve_txn_confirm_date(txn_date: &str, txn_time: &str) -> Option<String> {
    let date = chrono::NaiveDate::parse_from_str(txn_date, "%Y-%m-%d").ok()?;
    let t = txn_time.trim();
    let after_cutoff = if t.is_empty() {
        false
    } else {
        t.get(..5).map_or(false, |hhmm| hhmm >= "15:00")
    };
    let d = if after_cutoff {
        next_trading_day(date)
    } else {
        date
    };
    Some(d.format("%Y-%m-%d").to_string())
}

/// txn_date 之后第一个 A 股交易日（含跳过周末/法定休市）。
/// 用只读缓存版判断，避免在持有 DB 连接锁的路径（with_conn / init_db guard）内
/// 触发 ensure_loaded_offline 的 DB 加载造成嵌套锁死锁。
fn next_trading_day(date: chrono::NaiveDate) -> chrono::NaiveDate {
    let mut d = date + chrono::Duration::days(1);
    while !crate::data::is_trading_day_cached(d) {
        d += chrono::Duration::days(1);
    }
    d
}

pub fn import_transactions(
    account_id: i64,
    items: &[ImportTxn],
    source_ref: Option<String>,
) -> SqlResult<usize> {
    with_conn(|conn| {
        // 1) 同批次幂等：先「反向撤销」旧批各行对持仓的效果，再删除旧行 → 同批次重复导入幂等、不叠加。
        if let Some(ref_) = &source_ref {
            let mut stmt = conn.prepare(
                "SELECT id, txn_type, fund_code, shares, amount, COALESCE(platform,'') FROM transactions \
                 WHERE account_id=?1 AND source='import_txn' AND source_ref=?2",
            )?;
            let old_rows: Vec<(i64, String, Option<String>, Option<f64>, f64, String)> = stmt
                .query_map(rusqlite::params![account_id, ref_], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
                })?
                .collect::<Result<_, _>>()?;
            for (oid, otype, ocode, oshares, oamount, oplat) in old_rows {
                if let Some(oc) = &ocode {
                    apply_txn_to_position_conn(conn, account_id, &otype, oc, &oplat, oshares, oamount, true)?;
                }
                conn.execute("DELETE FROM transactions WHERE id=?1", [oid])?;
            }
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
            // 3) 【v9】真实流水与持仓镜像天然解耦：不再删除该基金任何合成基线/镜像持仓流水
            //    （v9 下截图导入/手动基线本就不产生流水；FundVal 镜像导入的流水独立保留为账本）。
            // 4) 份额缺失的买入/卖出 → 按「交易日(15:00 分界)确认净值」从本地 nav_history 自动反推；
            //    本地无该确认日净值 → shares/price 保持 NULL（流水先落账、不动持仓），
            //    由 backfill_pending_txn_shares 在净值到位后自动回填份额并把增量应用到持仓。
            let mut shares_val = it.shares;
            let mut price_val = it.price;
            if matches!(it.txn_type.as_str(), "buy" | "sell") && shares_val.map_or(true, |s| s <= 0.0) {
                if let Some(cd) = resolve_txn_confirm_date(&it.txn_date, &it.txn_time) {
                    if let Some(nav) = nav_on_date(conn, &it.fund_code, &cd) {
                        if nav > 0.0 && it.amount > 0.0 {
                            shares_val = Some((it.amount / nav * 100.0).round() / 100.0);
                            price_val = Some(nav);
                        }
                    }
                }
            }
            // 5) 写入/更新导入流水（import_txn + source_ref）
            //    幂等键 = 「交易时间 + 基金代码 + 持仓平台」（2026-08-21 用户定稿原则）：
            //    同一账户内已存在同基金、同平台、同交易日(含时间)的 import_txn 流水时，
            //    视为同一笔交易 → 整体更新（类型/份额/金额/价格/备注以最新导入为准），
            //    避免重复识别同一截图产生多条记录。不再把 txn_type / amount 纳入键，
            //    同一笔交易在多次识别中类型/金额略有差异时仍归并为一条。
            //    与上方按 source_ref 整批替换互不冲突：有批次标签时先删旧批再插入；无标签时靠此自然键去重。
            //    COALESCE(txn_time,'') 归一化空串与 NULL（历史数据可能存 NULL）。
            //    【v9】命中的旧行先反向撤销持仓效果，再用新值更新并正向应用；
            //    新插入行写入后直接正向应用 → positions 恒与账本一致。
            let existing: Option<(i64, String, Option<f64>, f64)> = conn
                .query_row(
                    "SELECT id, txn_type, shares, amount FROM transactions WHERE account_id=?1 AND source='import_txn' \
                     AND fund_code=?2 AND platform=?3 AND txn_date=?4 AND COALESCE(txn_time,'')=?5 LIMIT 1",
                    rusqlite::params![account_id, it.fund_code, it.platform, it.txn_date, it.txn_time],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .ok();
            if let Some((eid, old_type, old_shares, old_amount)) = existing {
                // 先撤销旧行的持仓效果，再以最新值覆盖更新并正向应用
                apply_txn_to_position_conn(
                    conn, account_id, &old_type, &it.fund_code, &it.platform,
                    old_shares, old_amount, true,
                )?;
                conn.execute(
                    "UPDATE transactions SET txn_type=?1, shares=?2, amount=?3, price=?4, txn_time=?5, note=?6 WHERE id=?7",
                    rusqlite::params![
                        it.txn_type,
                        shares_val,
                        it.amount,
                        price_val,
                        it.txn_time,
                        it.note,
                        eid
                    ],
                )?;
                apply_txn_to_position_conn(
                    conn, account_id, &it.txn_type, &it.fund_code, &it.platform,
                    shares_val, it.amount, false,
                )?;
            } else {
                conn.execute(
                    "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, price, txn_date, txn_time, note, platform, source, source_ref) \
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'import_txn',?11)",
                    rusqlite::params![
                        account_id,
                        it.txn_type,
                        it.fund_code,
                        shares_val,
                        it.amount,
                        price_val,
                        it.txn_date,
                        it.txn_time,
                        it.note,
                        it.platform,
                        source_ref
                    ],
                )?;
                apply_txn_to_position_conn(
                    conn, account_id, &it.txn_type, &it.fund_code, &it.platform,
                    shares_val, it.amount, false,
                )?;
            }
            count += 1;
        }
        Ok(count)
    })
}

/// 回填「待净值」流水：扫描 buy/sell 且份额缺失/为 0 且 price IS NULL 的流水
/// （OCR 金额导入时本地无确认日净值所致），若 nav_history 已有所需确认日净值，
/// 则按 金额÷净值 反推份额与价格，并把该笔交易的增量**应用到持仓**。
/// 返回回填条数（= 本次应用持仓的行数）；无需再全量重放。
pub fn backfill_pending_txn_shares_conn(conn: &Connection, account_id: i64) -> SqlResult<usize> {
    let mut stmt = conn.prepare(
        "SELECT id, txn_type, fund_code, platform, txn_date, COALESCE(txn_time,''), amount FROM transactions
         WHERE account_id=?1 AND txn_type IN ('buy','sell')
           AND (shares IS NULL OR shares <= 0) AND price IS NULL",
    )?;
    let rows: Vec<(i64, String, String, String, String, String, f64)> = stmt
        .query_map([account_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut n = 0usize;
    for (id, txn_type, code, platform, date, time, amount) in rows {
        if let Some(cd) = resolve_txn_confirm_date(&date, &time) {
            if let Some(nav) = nav_on_date(conn, &code, &cd) {
                if nav > 0.0 && amount > 0.0 {
                    let s = (amount / nav * 100.0).round() / 100.0;
                    conn.execute(
                        "UPDATE transactions SET shares=?1, price=?2 WHERE id=?3",
                        rusqlite::params![s, nav, id],
                    )?;
                    // 【v9】份额到位后把该笔 buy/sell 的增量应用到持仓（此前缺份额未应用）
                    apply_txn_to_position_conn(
                        conn, account_id, &txn_type, &code, &platform,
                        Some(s), amount, false,
                    )?;
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// 对外包装：在全局连接上回填待补流水（供 commands 层调用）。
pub fn backfill_pending_txn_shares(account_id: i64) -> SqlResult<usize> {
    with_conn(|conn| backfill_pending_txn_shares_conn(conn, account_id))
}

/// 基线写入（同一 with_conn 闭包内调用，避免嵌套加锁死锁）：**只直写 positions，不产生任何流水**。
/// 【v9 持仓模型】截图导入/手动建基线/改持仓 = 直接覆盖权威持仓；流水为纯账本不再重放，
/// 故不再写 1970-01-01 合成基线 buy、也不再删旧的 import/manual_set 合成流水。
fn write_position_conn(
    conn: &Connection,
    account_id: i64,
    code: &str,
    platform: &str,
    shares: f64,
    cost_amount: f64,
    holding_amount: f64,
    holding_profit: f64,
    yesterday_profit: f64,
    profit_rate: f64,
) -> SqlResult<()> {
    // 确保基金元数据存在（positions 外键指向 funds），名称先用代码占位，后续刷新行情/导入会修正
    conn.execute(
        "INSERT OR IGNORE INTO funds(code,name,platform,official_nav,fund_type,valuation_applicable) \
         VALUES(?1,?2,'',0,'',1)",
        rusqlite::params![code, code],
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

/// 把一笔交易流水「增量应用」到当前持仓（或反向撤销）——**v9 持仓模型的核心**。
///
/// 背景：positions 为权威、流水为纯账本，不再全量重放流水重建持仓。录入交易时，
/// 只按「交易份额/类型 + 当前持仓」做增量更新（与原重放的平均成本法规则逐笔等价，
/// 但只作用于当前持仓行，天然支持 FundVal 等「流水≠持仓镜像」的外部导入数据）。
///
/// 规则（forward，reverse=true 时按逆运算还原，用于删除/替换流水时撤销其持仓效果）：
/// - buy（shares>0）：份额 +s、成本 +amount；reverse 则扣回（不足扣到 0 视为清仓）。
/// - sell（shares>0）：按当前均价成比例扣减成本 s×basis/shares，份额 -s，全清则清零；
///   reverse 按当前均价回补（刚卖出立即撤销场景精确还原，跨交易顺序撤销为有界近似）。
/// - dividend（现金分红）：成本还原法 basis -amount（份额不变）；reverse +amount。
/// - reinvest_dividend（红利再投）：份额 +s，成本不变；reverse -s。
/// - buy/sell 缺份额（shares None/<=0，多为「待净值」待回填流水）：**不动持仓**——
///   待回填后由 backfill_pending_txn_shares_conn 补齐份额并在此应用。
/// - 其他类型（deposit/withdraw/legacy adjust）：账本只读类型，不动持仓（adjust 历史上
///   由重放「覆盖」实现，v9 已不再产生也不消费，删除旧 adjust 流水时同样跳过）。
///
/// 持仓行不存在时按「从 0 开始」应用（buy/reinvest 建行；sell/dividend 跳过）。
fn apply_txn_to_position_conn(
    conn: &Connection,
    account_id: i64,
    txn_type: &str,
    code: &str,
    platform: &str,
    shares: Option<f64>,
    amount: f64,
    reverse: bool,
) -> SqlResult<()> {
    // 读当前持仓（份额/成本）；无持仓按 0 处理
    let cur: Option<(f64, f64, f64)> = conn
        .query_row(
            "SELECT shares, cost_amount, holding_amount FROM positions \
             WHERE account_id=?1 AND fund_code=?2 AND platform=?3",
            rusqlite::params![account_id, code, platform],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let (mut s, mut b, h) = cur.unwrap_or((0.0, 0.0, 0.0));
    let sh = shares.unwrap_or(0.0);

    match txn_type {
        "buy" => {
            if sh > 0.0 {
                if !reverse {
                    s += sh;
                    b += amount;
                } else {
                    s -= sh;
                    b -= amount;
                    if s < 0.0 {
                        s = 0.0;
                        b = 0.0;
                    }
                }
            }
            // 缺份额的 buy 不动持仓（待净值回填后生效）
        }
        "sell" => {
            if sh > 0.0 {
                if !reverse {
                    if s > 0.0 {
                        let sell_basis = sh * b / s;
                        b -= sell_basis;
                        s -= sh;
                        if s <= 1e-9 {
                            s = 0.0;
                            b = 0.0;
                        }
                    }
                } else {
                    // 反向撤销卖出：份额回补；成本按「当前均价」回补（撤销紧邻卖出时精确，
                    // 中间夹有其他交易时为有界近似）。s==0 时回补份额但成本无法溯源（取 0）。
                    if s > 0.0 {
                        b += sh * b / s;
                    }
                    s += sh;
                }
            }
        }
        "dividend" => {
            if s > 0.0 {
                if !reverse {
                    b -= amount; // 允许为负（收回全部成本后分红即净收益）
                } else {
                    b += amount;
                }
            }
        }
        "reinvest_dividend" => {
            if sh > 0.0 {
                if !reverse {
                    s += sh;
                } else {
                    s -= sh;
                    if s < 0.0 {
                        s = 0.0;
                        b = 0.0;
                    }
                }
            }
        }
        // deposit / withdraw / legacy adjust 等：账本只读类型，不触碰持仓
        _ => return Ok(()),
    }

    if s <= 1e-9 && b.abs() < 1e-9 && h <= 0.0 {
        // 份额与成本均清零且无金额兜底 → 删除持仓行（清仓）
        conn.execute(
            "DELETE FROM positions WHERE account_id=?1 AND fund_code=?2 AND platform=?3",
            rusqlite::params![account_id, code, platform],
        )?;
    } else if cur.is_some() || s > 1e-9 {
        // 有行 → 就地更新份额/成本；无行但买入建仓 → 插入（h 保留为 0）
        conn.execute(
            "INSERT INTO positions(account_id, fund_code, platform, shares, cost_amount) \
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(account_id, fund_code, platform) DO UPDATE SET
               shares=excluded.shares, cost_amount=excluded.cost_amount",
            rusqlite::params![account_id, code, platform, s, b],
        )?;
    }
    Ok(())
}

// ---- 持仓视图（按账户过滤） ----

pub fn list_holdings(account_id: Option<i64>) -> SqlResult<Vec<HoldingRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.account_id, f.code, f.name, p.platform, f.official_nav, f.prev_nav, f.nav_date,
                    f.report_period, f.disclosure_type, f.fund_type, f.valuation_applicable,
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
                prev_nav: r.get::<usize, Option<f64>>(5)?.unwrap_or(0.0),
                nav_date: r.get::<usize, Option<String>>(6)?.unwrap_or_default(),
                report_period: r.get(7)?,
                disclosure_type: r.get(8)?,
                fund_type: r.get::<usize, Option<String>>(9)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(10).unwrap_or(1) != 0,
                shares: r.get(11)?,
                cost_amount: r.get(12)?,
                holding_amount: r.get(13)?,
                holding_profit: r.get(14)?,
                yesterday_profit: r.get(15)?,
                profit_rate: r.get(16)?,
            })
        })?;
        rows.collect()
    })
}

pub fn get_holding(code: &str, account_id: i64) -> SqlResult<Option<HoldingRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT p.account_id, f.code, f.name, p.platform, f.official_nav, f.prev_nav, f.nav_date,
                    f.report_period, f.disclosure_type, f.fund_type, f.valuation_applicable,
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
                prev_nav: r.get::<usize, Option<f64>>(5)?.unwrap_or(0.0),
                nav_date: r.get::<usize, Option<String>>(6)?.unwrap_or_default(),
                report_period: r.get(7)?,
                disclosure_type: r.get(8)?,
                fund_type: r.get::<usize, Option<String>>(9)?.unwrap_or_default(),
                valuation_applicable: r.get::<usize, i64>(10).unwrap_or(1) != 0,
                shares: r.get(11)?,
                cost_amount: r.get(12)?,
                holding_amount: r.get(13)?,
                holding_profit: r.get(14)?,
                yesterday_profit: r.get(15)?,
                profit_rate: r.get(16)?,
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
    platform: &str,
    total_market_value: f64,
    total_cost: f64,
    total_pnl: f64,
    day_pnl: f64,
    total_return_pct: f64,
    max_drawdown_pct: f64,
    day_pnl_est: f64,
    est_market_value: f64,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO snapshots(account_id, platform, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl, total_return_pct, max_drawdown_pct, day_pnl_est, est_market_value)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(account_id, platform, snapshot_date) DO UPDATE SET
               total_market_value=excluded.total_market_value, total_cost=excluded.total_cost,
               total_pnl=excluded.total_pnl, day_pnl=excluded.day_pnl,
               total_return_pct=excluded.total_return_pct, max_drawdown_pct=excluded.max_drawdown_pct,
               day_pnl_est=excluded.day_pnl_est, est_market_value=excluded.est_market_value",
            rusqlite::params![
                account_id, platform, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl,
                total_return_pct, max_drawdown_pct, day_pnl_est, est_market_value
            ],
        )?;
        Ok(())
    })
}

pub fn list_snapshots(account_id: i64) -> SqlResult<Vec<SnapshotRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, account_id, platform, snapshot_date, total_market_value, total_cost, total_pnl, day_pnl, total_return_pct, max_drawdown_pct, day_pnl_est, est_market_value
             FROM snapshots WHERE account_id = ?1 ORDER BY snapshot_date ASC",
        )?;
        let rows = stmt.query_map([account_id], |r| {
            Ok(SnapshotRow {
                id: r.get(0)?,
                account_id: r.get(1)?,
                platform: r.get::<usize, Option<String>>(2)?.unwrap_or_default(),
                snapshot_date: r.get(3)?,
                total_market_value: r.get(4)?,
                total_cost: r.get(5)?,
                total_pnl: r.get(6)?,
                day_pnl: r.get(7)?,
                total_return_pct: r.get::<usize, Option<f64>>(8)?.unwrap_or(0.0),
                max_drawdown_pct: r.get::<usize, Option<f64>>(9)?.unwrap_or(0.0),
                day_pnl_est: r.get::<usize, Option<f64>>(10)?.unwrap_or(0.0),
                est_market_value: r.get::<usize, Option<f64>>(11)?.unwrap_or(0.0),
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

/// 逐仓逐日估值物化行（position_daily）。成本曲线与逐日估值共用此表。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PositionDailyRow {
    pub position_id: i64,
    pub nav_date: String,
    pub shares: f64,
    pub avg_cost: f64,
    pub cost_amount: f64,
    pub official_nav: f64,
    pub est_nav: f64,
    pub reference_nav: f64,
    pub market_value: f64,
    pub day_pnl_act: f64,
    pub day_pnl_est: f64,
    pub day_pnl_pct_act: f64,
    pub day_pnl_pct_est: f64,
    pub is_estimated: bool,
}

/// 日终对单条持仓 upsert 一行逐日估值（主键 (position_id, nav_date) 幂等）。
/// 指标由 compute_position_metrics 预先算好后传入；写入时机为日终重算持仓之后。
pub fn upsert_position_daily(
    position_id: i64,
    nav_date: &str,
    shares: f64,
    avg_cost: f64,
    cost_amount: f64,
    official_nav: f64,
    est_nav: f64,
    reference_nav: f64,
    market_value: f64,
    day_pnl_act: f64,
    day_pnl_est: f64,
    day_pnl_pct_act: f64,
    day_pnl_pct_est: f64,
    is_estimated: bool,
) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO position_daily(position_id, nav_date, shares, avg_cost, cost_amount, official_nav, est_nav, reference_nav, market_value, day_pnl_act, day_pnl_est, day_pnl_pct_act, day_pnl_pct_est, is_estimated)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(position_id, nav_date) DO UPDATE SET
               shares=excluded.shares, avg_cost=excluded.avg_cost, cost_amount=excluded.cost_amount,
               official_nav=excluded.official_nav, est_nav=excluded.est_nav, reference_nav=excluded.reference_nav,
               market_value=excluded.market_value, day_pnl_act=excluded.day_pnl_act, day_pnl_est=excluded.day_pnl_est,
               day_pnl_pct_act=excluded.day_pnl_pct_act, day_pnl_pct_est=excluded.day_pnl_pct_est, is_estimated=excluded.is_estimated",
            rusqlite::params![
                position_id, nav_date, shares, avg_cost, cost_amount, official_nav, est_nav, reference_nav,
                market_value, day_pnl_act, day_pnl_est, day_pnl_pct_act, day_pnl_pct_est, is_estimated as i64
            ],
        )?;
        Ok(())
    })
}

/// 读取某持仓的全部逐日估值序列（按日期升序），供成本曲线 / 盈亏日历复用。
pub fn get_position_daily(position_id: i64) -> SqlResult<Vec<PositionDailyRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT position_id, nav_date, shares, avg_cost, cost_amount, official_nav, est_nav, reference_nav, market_value, day_pnl_act, day_pnl_est, day_pnl_pct_act, day_pnl_pct_est, is_estimated
             FROM position_daily WHERE position_id = ?1 ORDER BY nav_date ASC",
        )?;
        let rows = stmt.query_map([position_id], |r| {
            Ok(PositionDailyRow {
                position_id: r.get(0)?,
                nav_date: r.get(1)?,
                shares: r.get(2)?,
                avg_cost: r.get(3)?,
                cost_amount: r.get(4)?,
                official_nav: r.get::<usize, Option<f64>>(5)?.unwrap_or(0.0),
                est_nav: r.get::<usize, Option<f64>>(6)?.unwrap_or(0.0),
                reference_nav: r.get::<usize, Option<f64>>(7)?.unwrap_or(0.0),
                market_value: r.get(8)?,
                day_pnl_act: r.get(9)?,
                day_pnl_est: r.get(10)?,
                day_pnl_pct_act: r.get(11)?,
                day_pnl_pct_est: r.get(12)?,
                is_estimated: r.get::<usize, i64>(13)? != 0,
            })
        })?;
        rows.collect()
    })
}

/// 读取某基金在某账户的「权威持仓」份额与成本基数（positions 表）。
/// 【v9 持仓模型】成本线不再由流水重放（流水≠镜像时会得出与持仓不一致的曲线），
/// 统一以 positions 为准：走势图取该值画「当前持仓均价」水平参考线。
/// 无持仓返回 None。
pub fn get_position_basis(code: &str, account_id: i64) -> SqlResult<Option<(f64, f64)>> {
    with_conn(|conn| {
        let row = conn.query_row(
            "SELECT shares, cost_amount FROM positions WHERE account_id=?1 AND fund_code=?2",
            rusqlite::params![account_id, code],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
        )?;
        Ok(Some(row))
    })
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

/// 单个交易标记（供走势图叠加买入/卖出/分红/红利再投点）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TxnMarker {
    pub date: String,
    pub txn_type: String, // buy / sell / dividend / reinvest_dividend
    pub shares: f64,
    pub amount: f64,
}

/// 取某基金在某账户下的真实买卖/分红/红利再投交易标记（不含 1970 合成基线）。
pub fn get_txn_markers(code: &str, account_id: i64) -> SqlResult<Vec<TxnMarker>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT txn_type, COALESCE(shares,0), amount, txn_date FROM transactions
             WHERE account_id = ?1 AND fund_code = ?2 AND txn_date != '1970-01-01'
               AND txn_type IN ('buy','sell','dividend','reinvest_dividend')
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

// ============================================================
// 策略信号层（valuation_grid 移植）—— grid_* 表访问
// 铁律：strategy 只读 positions/transactions，写仅限 grid_* 新表（见 init_db）。
// ============================================================

/// 策略基金配置行
#[derive(Debug, Clone, serde::Serialize)]
pub struct GridFundCfg {
    pub fund_code: String,
    pub enabled: i64,
    pub max_position: Option<f64>,
    pub vol_sensitivity: Option<f64>,
    pub cooldown_sell_date: Option<String>,
    pub peak_nav: Option<f64>,
}

/// 启用/更新策略基金；首次启用且 peak_nav 为空时以当时 nav_history max 初始化（OD-4）。
pub fn grid_upsert_fund(fund_code: &str, enabled: i64, max_position: Option<f64>) -> SqlResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO grid_funds(fund_code, enabled, max_position)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(fund_code) DO UPDATE SET
               enabled = excluded.enabled,
               max_position = excluded.max_position,
               updated_at = datetime('now')",
            rusqlite::params![fund_code, enabled, max_position],
        )?;
        if enabled == 1 {
            conn.execute(
                "UPDATE grid_funds
                 SET peak_nav = COALESCE(peak_nav, (SELECT MAX(nav) FROM nav_history
                                                   WHERE fund_code=?1 AND nav>0)),
                     updated_at = datetime('now')
                 WHERE fund_code = ?1 AND peak_nav IS NULL",
                rusqlite::params![fund_code],
            )?;
        }
        Ok(())
    })
}

pub fn grid_list_config() -> SqlResult<Vec<GridFundCfg>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT fund_code, enabled, max_position, vol_sensitivity, cooldown_sell_date, peak_nav
             FROM grid_funds ORDER BY fund_code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(GridFundCfg {
                fund_code: r.get(0)?,
                enabled: r.get(1)?,
                max_position: r.get(2)?,
                vol_sensitivity: r.get(3)?,
                cooldown_sell_date: r.get(4)?,
                peak_nav: r.get(5)?,
            })
        })?;
        rows.collect()
    })
}

pub fn grid_get_enabled_codes() -> SqlResult<Vec<String>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT fund_code FROM grid_funds WHERE enabled=1 ORDER BY fund_code")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect()
    })
}

/// 账本事件行（批次投影输入：buy/reinvest 增、sell 按 FIFO 扣）
#[derive(Debug, Clone)]
pub struct LotEventRow {
    pub id: i64,
    pub txn_date: String,
    pub txn_type: String,
    pub shares: f64,
    pub amount: f64,
    pub price: f64,
}

/// 拉取某基金全部份额型账本事件（按日期/序号升序，供内存投影批次视图）。
pub fn list_lot_events(fund_code: &str) -> SqlResult<Vec<LotEventRow>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, txn_date, txn_type, COALESCE(shares,0), amount, COALESCE(price,0)
             FROM transactions
             WHERE fund_code = ?1 AND txn_type IN ('buy','sell','reinvest_dividend')
             ORDER BY txn_date ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![fund_code], |r| {
            Ok(LotEventRow {
                id: r.get(0)?,
                txn_date: r.get(1)?,
                txn_type: r.get(2)?,
                shares: r.get(3)?,
                amount: r.get(4)?,
                price: r.get(5)?,
            })
        })?;
        rows.collect()
    })
}

/// 某基金净值历史（降序，最新在前）
pub fn list_nav_history_code(fund_code: &str, limit: Option<u32>) -> SqlResult<Vec<(String, f64)>> {
    with_conn(|conn| {
        let mut sql = String::from(
            "SELECT nav_date, nav FROM nav_history WHERE fund_code=?1 AND nav>0 ORDER BY nav_date DESC",
        );
        if let Some(l) = limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![fund_code], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;
        rows.collect()
    })
}

/// 写当日信号 + 历史（同码同日同源覆盖）
pub fn grid_upsert_signal_and_history(
    s: &crate::strategy::model::GridSignal,
) -> SqlResult<()> {
    let fifo_json = s.fifo_plan.as_ref().map(|f| serde_json::to_string(f).unwrap_or_default());
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO grid_signal(fund_code, signal_date, source, signal_name, action,
                     priority, sub_priority, amount, sell_pct, est_change_pct, current_nav,
                     total_profit_pct, confidence, reason, alert, fifo_plan)
             VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,?8,?9,?10,?11,?12,?13,?14,?15)
             ON CONFLICT(fund_code, signal_date, source) DO UPDATE SET
               signal_name=excluded.signal_name, action=excluded.action, priority=excluded.priority,
               amount=excluded.amount, sell_pct=excluded.sell_pct, est_change_pct=excluded.est_change_pct,
               current_nav=excluded.current_nav, total_profit_pct=excluded.total_profit_pct,
               confidence=excluded.confidence, reason=excluded.reason, alert=excluded.alert,
               fifo_plan=excluded.fifo_plan",
            rusqlite::params![
                s.fund_code, s.signal_date, s.source, s.signal_name, s.action,
                s.priority as f64, s.amount, s.sell_pct, s.est_change_pct, s.current_nav,
                s.total_profit_pct, s.confidence, s.reason, s.alert as i64, fifo_json
            ],
        )?;
        conn.execute(
            "INSERT INTO grid_signal_history(fund_code, signal_date, source, signal_name, action,
                     priority, reason, amount, sell_pct, today_change, current_nav, nav_at_signal,
                     total_profit_pct)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
             ON CONFLICT(fund_code, signal_date, source) DO UPDATE SET
               signal_name=excluded.signal_name, action=excluded.action, priority=excluded.priority,
               reason=excluded.reason, amount=excluded.amount, sell_pct=excluded.sell_pct,
               today_change=excluded.today_change, current_nav=excluded.current_nav,
               nav_at_signal=excluded.nav_at_signal, total_profit_pct=excluded.total_profit_pct",
            rusqlite::params![
                s.fund_code, s.signal_date, s.source, s.signal_name, s.action,
                s.priority as f64, s.reason, s.amount, s.sell_pct, s.est_change_pct,
                s.current_nav, s.current_nav, s.total_profit_pct
            ],
        )?;
        Ok(())
    })
}

/// 当日信号读出行（join funds 取名称/类型）
#[derive(Debug, Clone, serde::Serialize)]
pub struct GridSignalRowDb {
    pub id: i64,
    pub fund_code: String,
    pub signal_date: String,
    pub source: String,
    pub signal_name: Option<String>,
    pub action: String,
    pub priority: Option<f64>,
    pub amount: Option<f64>,
    pub sell_pct: Option<f64>,
    pub est_change_pct: Option<f64>,
    pub current_nav: Option<f64>,
    pub total_profit_pct: Option<f64>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    pub alert: i64,
    pub fifo_plan: Option<String>,
    pub fund_name: Option<String>,
    pub fund_type: Option<String>,
}

pub fn grid_list_signals_today(signal_date: Option<&str>) -> SqlResult<Vec<GridSignalRowDb>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT g.id, g.fund_code, g.signal_date, g.source, g.signal_name, g.action,
                    g.priority, g.amount, g.sell_pct, g.est_change_pct, g.current_nav,
                    g.total_profit_pct, g.confidence, g.reason, g.alert, g.fifo_plan,
                    f.name, f.fund_type
             FROM grid_signal g LEFT JOIN funds f ON f.code = g.fund_code
             WHERE g.signal_date = ?1
             ORDER BY g.fund_code",
        )?;
        let rows = stmt.query_map(rusqlite::params![signal_date.unwrap_or("")], |r| {
            Ok(GridSignalRowDb {
                id: r.get(0)?,
                fund_code: r.get(1)?,
                signal_date: r.get(2)?,
                source: r.get(3)?,
                signal_name: r.get(4)?,
                action: r.get(5)?,
                priority: r.get(6)?,
                amount: r.get(7)?,
                sell_pct: r.get(8)?,
                est_change_pct: r.get(9)?,
                current_nav: r.get(10)?,
                total_profit_pct: r.get(11)?,
                confidence: r.get(12)?,
                reason: r.get(13)?,
                alert: r.get(14)?,
                fifo_plan: r.get(15)?,
                fund_name: r.get(16)?,
                fund_type: r.get(17)?,
            })
        })?;
        rows.collect()
    })
}

/// 信号历史读出行
#[derive(Debug, Clone, serde::Serialize)]
pub struct GridSignalHistoryRowDb {
    pub id: i64,
    pub fund_code: String,
    pub signal_date: String,
    pub source: String,
    pub signal_name: Option<String>,
    pub action: Option<String>,
    pub priority: Option<f64>,
    pub reason: Option<String>,
    pub amount: Option<f64>,
    pub sell_pct: Option<f64>,
    pub today_change: Option<f64>,
    pub current_nav: Option<f64>,
    pub nav_at_signal: Option<f64>,
    pub total_profit_pct: Option<f64>,
    pub outcome_t3: Option<f64>,
    pub outcome_t5: Option<f64>,
    pub outcome_t10: Option<f64>,
    pub executed: i64,
    pub fund_name: Option<String>,
}

pub fn grid_list_history(fund_code: Option<&str>, limit: i64) -> SqlResult<Vec<GridSignalHistoryRowDb>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT h.id, h.fund_code, h.signal_date, h.source, h.signal_name, h.action,
                    h.priority, h.reason, h.amount, h.sell_pct, h.today_change, h.current_nav,
                    h.nav_at_signal, h.total_profit_pct, h.outcome_t3, h.outcome_t5,
                    h.outcome_t10, h.executed, f.name
             FROM grid_signal_history h LEFT JOIN funds f ON f.code = h.fund_code
             WHERE (?1 IS NULL OR h.fund_code = ?1)
             ORDER BY h.signal_date DESC, h.id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![fund_code, limit],
            |r| {
                Ok(GridSignalHistoryRowDb {
                    id: r.get(0)?,
                    fund_code: r.get(1)?,
                    signal_date: r.get(2)?,
                    source: r.get(3)?,
                    signal_name: r.get(4)?,
                    action: r.get(5)?,
                    priority: r.get(6)?,
                    reason: r.get(7)?,
                    amount: r.get(8)?,
                    sell_pct: r.get(9)?,
                    today_change: r.get(10)?,
                    current_nav: r.get(11)?,
                    nav_at_signal: r.get(12)?,
                    total_profit_pct: r.get(13)?,
                    outcome_t3: r.get(14)?,
                    outcome_t5: r.get(15)?,
                    outcome_t10: r.get(16)?,
                    executed: r.get(17)?,
                    fund_name: r.get(18)?,
                })
            },
        )?;
        rows.collect()
    })
}

pub fn grid_settings_get(key: &str) -> SqlResult<Option<String>> {
    with_conn(|conn| {
        let v = conn
            .query_row("SELECT v FROM grid_settings WHERE k=?1", rusqlite::params![key], |r| {
                r.get::<_, String>(0)
            })
            .ok();
        Ok(v)
    })
}

pub fn grid_settings_set(key: &str, val: Option<&str>) -> SqlResult<()> {
    with_conn(|conn| {
        match val {
            Some(v) => {
                conn.execute(
                    "INSERT INTO grid_settings(k, v) VALUES(?1, ?2)
                     ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                    rusqlite::params![key, v],
                )?;
            }
            None => {
                conn.execute("DELETE FROM grid_settings WHERE k=?1", rusqlite::params![key])?;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // 所有 DB 测试共用全局连接（DB 全局单例）与生产库初始化路径，必须串行执行：
    // 否则并行执行的测试会互相重置全局连接，导致一个测试的查询被重定向到另一个测试的库
    // （表现：偶发“数据库未初始化”或断言计数不符）。串行化 + 每测试唯一临时目录，彻底消除竞态与数据污染。
    static DB_TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    pub(crate) fn lock_db_tests() -> std::sync::MutexGuard<'static, ()> {
        // 对中毒容错：某测试持锁 panic 不应让后续测试连环 PoisonError 失败。
        DB_TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn init_temp_db() {
        use std::sync::atomic::{AtomicU64, Ordering};
        // 每次调用使用唯一临时目录，并强制重置全局连接，避免测试之间共享同一个数据库文件
        // 导致数据相互污染（此前所有 DB 测试共用 process 级临时库，数据会跨测试累积）。
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("fundlens_test_{}_{}", std::process::id(), n));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("FUNDLENS_DATA_DIR", dir.to_string_lossy().to_string());
        {
            let mut guard = DB.lock().unwrap();
            *guard = None;
        }
        let _ = init_db(None);
    }

    #[test]
    fn account_and_txn_recompute() {
        let _g = lock_db_tests();
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
        record_snapshot(acc, "2026-01-02", "", 660.0, 600.0, 60.0, 60.0, 0.0, 0.0, 58.0, 658.0).unwrap();
        let snaps = list_snapshots(acc).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].total_market_value, 660.0);
        // v8：估算列已落库可读
        assert!((snaps[0].day_pnl_est - 58.0).abs() < 1e-9);
        assert!((snaps[0].est_market_value - 658.0).abs() < 1e-9);
    }

    #[test]
    fn import_txn_incremental_and_dividend() {
        let _g = lock_db_tests();
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
    fn update_position_inplace_no_flow_semantics() {
        // 【v9】手工调整 = 直接覆盖权威 positions，**不产生任何流水**（不再追加 adjust 流水/重放覆盖）
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("盘点账户", "").unwrap();
        // 先有持仓：基线 100 份 / 成本 1000（基线同样不产生流水）
        set_baseline(acc, "000012", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        assert_eq!(list_holdings(Some(acc)).unwrap()[0].shares, 100.0);
        // 基线不产生流水：账本为空
        assert!(list_transactions(Some(acc), None).unwrap().is_empty(), "基线不得产生流水");

        // 盘点 1：改为 150 份 / 成本 1500 → 持仓直接覆盖为 150，且不产生流水
        update_position_inplace(acc, "000012", 150.0, 1500.0, "alipay").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 150.0);
        assert!((hs[0].cost_amount - 1500.0).abs() < 1e-6);
        assert!(list_transactions(Some(acc), None).unwrap().is_empty(), "改持仓不得产生流水");

        // 盘点 2：改为 80 份 / 成本 640 → 以后一次盘点为准
        update_position_inplace(acc, "000012", 80.0, 640.0, "alipay").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 80.0);
        assert!((hs[0].cost_amount - 640.0).abs() < 1e-6);

        // 盘点后的新交易叠加：再买入 20 份 → 100 份（80 + 20）/ 成本 840
        add_transaction(acc, "buy", Some("000012".to_string()), Some(20.0), 200.0, None, "2026-02-01", "", None, "alipay").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 840.0).abs() < 1e-6);
        // 仅买入交易产生 1 条流水（改持仓/基线均不产生）
        let txns = list_transactions(Some(acc), None).unwrap();
        assert_eq!(txns.len(), 1);
        assert_eq!(txns[0].txn_type, "buy");

        // 另一基金：清仓盘点（shares=0）→ 持仓行被移除（无后续交易）
        set_baseline(acc, "000013", 50.0, 500.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        update_position_inplace(acc, "000013", 0.0, 0.0, "alipay").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert!(!hs.iter().any(|h| h.code == "000013"), "清仓盘点后应无该基金持仓");
    }

    #[test]
    fn import_txn_idempotency_by_time_fund_platform() {
        // 幂等键 = 交易时间(日期+时间) + 基金代码 + 持仓平台（用户定稿原则）：
        // 同键命中 → 整体更新（类型/金额/备注以最新为准），不新增。
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("幂等账户", "").unwrap();
        let mk = |ttype: &str, shares: Option<f64>, amount: f64, time: &str, note: Option<&str>| ImportTxn {
            fund_code: "000009".to_string(),
            fund_name: None,
            txn_type: ttype.to_string(),
            shares,
            amount,
            price: Some(10.0),
            txn_date: "2026-08-21".to_string(),
            txn_time: time.to_string(),
            note: note.map(|s| s.to_string()),
            platform: "alipay".to_string(),
        };

        // ① 同键（同基金/同平台/08-21 10:30）先 buy 后 sell → 更新为 1 条，值以最新为准
        import_transactions(acc, &[mk("buy", Some(100.0), 1000.0, "10:30", None)], None).unwrap();
        import_transactions(acc, &[mk("sell", Some(50.0), 500.0, "10:30", Some("覆盖"))], None).unwrap();
        let txns = list_transactions(Some(acc), None).unwrap();
        assert_eq!(txns.len(), 1, "同键应更新而非新增");
        assert_eq!(txns[0].txn_type, "sell");
        assert_eq!(txns[0].note.as_deref(), Some("覆盖"));
        assert!((txns[0].amount - 500.0).abs() < 1e-6);

        // ② 不同交易时间 → 新增（08-21 14:00）
        import_transactions(acc, &[mk("buy", Some(10.0), 100.0, "14:00", None)], None).unwrap();
        let txns = list_transactions(Some(acc), None).unwrap();
        assert_eq!(txns.len(), 2, "不同交易时间应新增");

        // ③ 不同基金 / 不同平台 → 新增
        let other_fund = ImportTxn {
            fund_code: "000010".to_string(),
            ..mk("buy", Some(1.0), 10.0, "10:30", None)
        };
        import_transactions(acc, &[other_fund], None).unwrap();
        let other_plat = ImportTxn {
            platform: "jd_finance".to_string(),
            ..mk("buy", Some(1.0), 10.0, "10:30", None)
        };
        import_transactions(acc, &[other_plat], None).unwrap();
        let txns = list_transactions(Some(acc), None).unwrap();
        assert_eq!(txns.len(), 4, "不同基金/不同平台应新增");

        // ④ txn_time 空串与历史 NULL 归一化：先正常导入一条空时间流水（建基金+记录），
        //    再将其 txn_time 置 NULL 模拟历史数据，最后导入空串同键 → 应命中 COALESCE 判重更新（不新增）
        let null_init = ImportTxn {
            fund_code: "000011".to_string(),
            fund_name: None,
            txn_type: "buy".to_string(),
            shares: Some(1.0),
            amount: 10.0,
            price: Some(10.0),
            txn_date: "2026-08-21".to_string(),
            txn_time: String::new(),
            note: None,
            platform: "alipay".to_string(),
        };
        import_transactions(acc, &[null_init], None).unwrap();
        with_conn(|c| {
            c.execute(
                "UPDATE transactions SET txn_time=NULL WHERE fund_code='000011' AND platform='alipay'",
                [],
            )
        })
        .unwrap();
        let null_time = ImportTxn {
            fund_code: "000011".to_string(),
            fund_name: None,
            txn_type: "buy".to_string(),
            shares: Some(2.0),
            amount: 20.0,
            price: Some(10.0),
            txn_date: "2026-08-21".to_string(),
            txn_time: String::new(),
            note: None,
            platform: "alipay".to_string(),
        };
        import_transactions(acc, &[null_time], None).unwrap();
        let txns = list_transactions(Some(acc), Some("000011".to_string())).unwrap();
        assert_eq!(txns.len(), 1, "空串与 NULL 时间应归一化判重");
        assert!((txns[0].amount - 20.0).abs() < 1e-6);
    }

    #[test]
    fn txn_confirm_date_15h_cutoff() {
        // 纯函数：15:00 分界与跨交易日
        // 2026-08-21 是周五
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "").as_deref(), Some("2026-08-21")); // 无时间 → 当日
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "09:30").as_deref(), Some("2026-08-21")); // 15 点前 → 当日
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "14:59").as_deref(), Some("2026-08-21"));
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "15:00").as_deref(), Some("2026-08-24")); // 15 点整 → 下一交易日(下周一)
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "15:30").as_deref(), Some("2026-08-24")); // 15 点后 → 下周一
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "23:59").as_deref(), Some("2026-08-24"));
        assert_eq!(resolve_txn_confirm_date("2026-08-21", "7").as_deref(), Some("2026-08-21")); // 时间格式不完整 → 保守按当日
    }

    #[test]
    fn import_txn_shares_backfill_from_local_nav() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("反推账户", "").unwrap();
        // 预置 001614 净值：08-19/08-20（用于反推），08-24 稍后补（用于验证「待补回填」）
        upsert_nav_history(
            "001614",
            &[
                crate::data::NavPoint { date: "2026-08-19".to_string(), nav: 1.1703, acc_nav: 0.0 },
                crate::data::NavPoint { date: "2026-08-20".to_string(), nav: 1.1661, acc_nav: 0.0 },
            ],
        )
        .unwrap();

        // ① 08-19 10:30 买入 200 元：15:00 前 → 当日(08-19)净值 1.1703 反推 170.90
        let buy_am = ImportTxn {
            fund_code: "001614".to_string(),
            fund_name: None,
            txn_type: "buy".to_string(),
            shares: None,
            amount: 200.0,
            price: None,
            txn_date: "2026-08-19".to_string(),
            txn_time: "10:30".to_string(),
            note: None,
            platform: String::new(),
        };
        import_transactions(acc, &[buy_am], Some("B1".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert!((hs[0].shares - 200.0 / 1.1703).abs() < 1e-2, "15点前应按当日净值反推, got {}", hs[0].shares);

        // ② 08-19 15:30 买入 200 元：15:00 后 → 下一交易日(08-20)净值 1.1661 反推
        let buy_pm = ImportTxn {
            fund_code: "001614".to_string(),
            fund_name: None,
            txn_type: "buy".to_string(),
            shares: None,
            amount: 200.0,
            price: None,
            txn_date: "2026-08-19".to_string(),
            txn_time: "15:30".to_string(),
            note: None,
            platform: String::new(),
        };
        import_transactions(acc, &[buy_pm], Some("B2".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert!((hs[0].shares - (200.0 / 1.1703 + 200.0 / 1.1661)).abs() < 1e-2, "15点后应按下一交易日净值反推, got {}", hs[0].shares);

        // ③ 08-20 10:00 卖出 100 元（在买入之后，15:00 前 → 08-20 净值 1.1661 反推并扣减）
        let sell_am = ImportTxn {
            fund_code: "001614".to_string(),
            fund_name: None,
            txn_type: "sell".to_string(),
            shares: None,
            amount: 100.0,
            price: None,
            txn_date: "2026-08-20".to_string(),
            txn_time: "10:00".to_string(),
            note: None,
            platform: String::new(),
        };
        import_transactions(acc, &[sell_am], Some("B3".to_string())).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        let after_sell = 200.0 / 1.1703 + 200.0 / 1.1661 - 100.0 / 1.1661;
        assert!((hs[0].shares - after_sell).abs() < 1e-2, "卖出应扣减, got {}", hs[0].shares);

        // ④ 08-21 15:30 卖出 50 元：确认日 = 下一交易日 08-24，nav_history 暂无 08-24 净值
        //    → shares/price 保持 NULL 待补（recompute 跳过，持仓不变）
        let sell_pending = ImportTxn {
            fund_code: "001614".to_string(),
            fund_name: None,
            txn_type: "sell".to_string(),
            shares: None,
            amount: 50.0,
            price: None,
            txn_date: "2026-08-21".to_string(),
            txn_time: "15:30".to_string(),
            note: None,
            platform: String::new(),
        };
        import_transactions(acc, &[sell_pending], Some("B4".to_string())).unwrap();
        let (s, p) = with_conn(|c| {
            c.query_row(
                "SELECT shares, price FROM transactions WHERE fund_code='001614' AND source_ref='B4'",
                [],
                |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?)),
            )
        })
        .unwrap();
        assert!(s.is_none() && p.is_none(), "无净值应保持 NULL 待补, got shares={s:?} price={p:?}");
        let hs = list_holdings(Some(acc)).unwrap();
        assert!((hs[0].shares - after_sell).abs() < 1e-2, "待补卖出不应改变持仓, got {}", hs[0].shares);

        // ⑤ 补 08-24 净值 → backfill 回填 → recompute 后卖出生效
        upsert_nav_history(
            "001614",
            &[crate::data::NavPoint { date: "2026-08-24".to_string(), nav: 1.1000, acc_nav: 0.0 }],
        )
        .unwrap();
        let n = backfill_pending_txn_shares(acc).unwrap();
        assert_eq!(n, 1, "应回填 1 条待补流水");
        // 【v9】backfill 内部已把该笔卖出应用到持仓（不再需要全量重放）
        let hs = list_holdings(Some(acc)).unwrap();
        let expect = after_sell - 50.0 / 1.1000;
        assert!((hs[0].shares - expect).abs() < 1e-2, "回填后卖出应扣减, got {}", hs[0].shares);
    }

    #[test]
    fn cost_basis_and_markers() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("成本账户", "").unwrap();
        set_baseline(acc, "000003", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        // 【v9】交易按 (基金,平台) 增量更新持仓：卖/分红的 platform 必须与持仓平台一致才生效
        add_transaction(acc, "sell", Some("000003".to_string()), Some(20.0), 220.0, None, "2026-02-01", "", None, "alipay").unwrap();
        add_transaction(acc, "dividend", Some("000003".to_string()), None, 30.0, None, "2026-03-01", "", None, "alipay").unwrap();
        // 【v9】成本线读「权威持仓」：卖出 20 份（均价 10）→ 剩 80 份 / 800，现金分红 30 → 成本 770
        let (shares, basis) = get_position_basis("000003", acc).unwrap().expect("应有持仓");
        assert!((shares - 80.0).abs() < 1e-6);
        assert!((basis - 770.0).abs() < 1e-6);
        // 标记：sell + dividend 共 2 条
        let markers = get_txn_markers("000003", acc).unwrap();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].txn_type, "sell");
        assert_eq!(markers[1].txn_type, "dividend");
        // 无持仓基金 → None
        assert!(get_position_basis("999999", acc).unwrap().is_none());
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
        let _g = lock_db_tests();
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
        // 重新导入支付宝基线，不应破坏京东金融持仓（验证基线按 (基金,平台) 独立 upsert）
        set_baseline(acc, "003095", 150.0, 1500.0, 0.0, 0.0, 0.0, 0.0, "alipay", "import").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 2);
        let jd = hs.iter().find(|h| h.platform == "jd_finance").expect("京东金融持仓被误删");
        assert!((jd.shares - 200.0).abs() < 1e-6);
    }

    // ---- 多期披露持仓（2026-09-02）----

    #[test]
    fn period_key_parses_both_formats_and_orders_chronologically() {
        // 库里两种写法并存：东财标题解析的「2026年2季度」与回退格式「2026Q2」，必须归一到同一可排序键
        assert_eq!(period_key("2026Q2"), 20262);
        assert_eq!(period_key("2026年2季度"), 20262);
        assert_eq!(period_key("2026年第2季度"), 20262);
        assert_eq!(period_key("2026年第二季度"), 20262);
        assert_eq!(period_key("2025Q4"), 20254);
        // 时间序：2025Q4 < 2026Q1 < 2026Q2
        assert!(period_key("2025Q4") < period_key("2026Q1"));
        assert!(period_key("2026Q1") < period_key("2026Q2"));
        // 无法解析时沉底，不参与「最新期」判定
        assert_eq!(period_key("未知期次"), i64::MIN);
    }

    #[test]
    fn disclosures_keep_history_and_valuation_uses_latest_only() {
        let _g = lock_db_tests();
        init_temp_db();
        insert_fund(&FundRow {
            code: "000001".into(),
            name: "测试基金".into(),
            platform: "alipay".into(),
            official_nav: 1.0,
            report_period: None,
            disclosure_type: None,
            fund_type: "001".into(),
            track_index: String::new(),
            valuation_applicable: true,
        })
        .expect("插入测试基金失败");
        let h = |code: &str, name: &str, w: f64| crate::valuation::DisclosedHolding {
            stock_code: code.into(),
            stock_name: name.into(),
            weight: w,
            report_period: String::new(),
            disclosure_type: "top10".into(),
        };
        // 上期：两只股票
        replace_disclosure_period(
            "000001",
            "2026Q1",
            &[h("600519", "贵州茅台", 0.10), h("000858", "五粮液", 0.05)],
        )
        .unwrap();
        // 本期：茅台加仓、五粮液退出、平安新增
        replace_disclosure_period(
            "000001",
            "2026Q2",
            &[h("600519", "贵州茅台", 0.12), h("601318", "中国平安", 0.08)],
        )
        .unwrap();

        // 两期历史都在，且按从旧到新返回
        let periods = list_disclosure_periods("000001").unwrap();
        assert_eq!(periods, vec!["2026Q1".to_string(), "2026Q2".to_string()]);

        // 核心不变量：估值入口只返回最新一期。若把两期叠加，覆盖度会变成
        // 0.10+0.05+0.12+0.08 = 0.35，远超真实值并让估算净值彻底错乱。
        let latest = list_disclosures("000001").unwrap();
        assert_eq!(latest.len(), 2);
        assert!(latest.iter().all(|x| x.report_period == "2026Q2"));
        assert!((latest.iter().map(|x| x.weight).sum::<f64>() - 0.20).abs() < 1e-9);

        // 批量接口（总览估值走这条）同口径：每只基金只取最新期
        let batch = list_disclosures_batch().unwrap();
        let mine: Vec<_> = batch.iter().filter(|(c, _)| c == "000001").collect();
        assert_eq!(mine.len(), 2, "批量接口把多期叠加了，覆盖度会爆表");
        assert!(mine.iter().all(|(_, x)| x.report_period == "2026Q2"));

        // 可按指定期次取历史（供「较上期」对比）
        let prev = list_disclosures_of_period("000001", "2026Q1").unwrap();
        assert_eq!(prev.len(), 2);
        assert!(prev.iter().all(|x| x.report_period == "2026Q1"));

        // 重复写入同一期次幂等：不新增行、不产生第三期
        replace_disclosure_period(
            "000001",
            "2026Q1",
            &[h("600519", "贵州茅台", 0.10), h("000858", "五粮液", 0.05)],
        )
        .unwrap();
        assert_eq!(list_disclosures_of_period("000001", "2026Q1").unwrap().len(), 2);
        assert_eq!(list_disclosure_periods("000001").unwrap().len(), 2);
    }

    #[test]
    fn import_positions_batch_converts_alipay_shares() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("折算账户", "").unwrap();
        // 支付宝风格：shares=0、holding_amount=10000、导入净值 nav=2.0、持有收益 +500
        // 期望折算 shares = 10000/2 = 5000，成本基数 = 10000 - 500 = 9500（保留历史累计收益）
        let items = vec![ImportHolding {
            code: "000004".to_string(),
            name: "折算测试基金".to_string(),
            platform: "alipay".to_string(),
            nav: 2.0,
            shares: 0.0,
            holding_amount: 10000.0,
            holding_profit: 500.0,
            yesterday_profit: 30.0,
            profit_rate: 5.0,
        }];
        import_positions_batch(acc, &items).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 1);
        assert!((hs[0].shares - 5000.0).abs() < 1e-6, "shares={}", hs[0].shares);
        assert!((hs[0].cost_amount - 9500.0).abs() < 1e-6, "cost={}", hs[0].cost_amount);

        // 份额型（京东）走原有 shares×nav 口径，不被折算逻辑影响
        let items2 = vec![ImportHolding {
            code: "000005".to_string(),
            name: "份额型基金".to_string(),
            platform: "jd_finance".to_string(),
            nav: 1.5,
            shares: 100.0,
            holding_amount: 0.0,
            holding_profit: 0.0,
            yesterday_profit: 0.0,
            profit_rate: 0.0,
        }];
        import_positions_batch(acc, &items2).unwrap();
        let jd = list_holdings(Some(acc))
            .unwrap()
            .into_iter()
            .find(|h| h.code == "000005")
            .expect("份额型基金缺失");
        assert!((jd.shares - 100.0).abs() < 1e-6);
        assert!((jd.cost_amount - 150.0).abs() < 1e-6); // 100 * 1.5
    }

    #[test]
    fn update_position_resolves_existing_platform() {
        let _g = lock_db_tests();
        init_temp_db();
        // 先在 alipay 平台建立基线
        set_baseline(1, "000001", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        // 未指定平台时应解析到既有 alipay，而非落到 '' 行
        let p = resolve_position_platform(1, "000001").expect("应解析到既有平台");
        assert_eq!(p, "alipay");
        // 模拟 update_position 省略平台：应在 alipay 行上更新份额，不产生 '' 幻影行
        set_baseline(1, "000001", 200.0, 2000.0, 0.0, 0.0, 0.0, 0.0, &p, "manual_set").unwrap();
        let shares = with_conn(|conn| {
            conn.query_row(
                "SELECT shares FROM positions WHERE account_id=1 AND fund_code='000001' AND platform='alipay'",
                [],
                |r| r.get::<usize, f64>(0),
            )
        })
        .unwrap();
        assert!((shares - 200.0).abs() < 1e-6);
        // 确认没有 '' 幻影行
        let phantom = with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT shares FROM positions WHERE account_id=1 AND fund_code='000001' AND platform=''",
                [],
                |r| r.get::<usize, f64>(0),
            )
            .ok())
        })
        .unwrap();
        assert!(phantom.is_none(), "不应产生空平台幻影行");
    }

    #[test]
    fn transactions_constraints_and_snapshot_columns() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("约束账户", "").unwrap();
        set_baseline(acc, "000010", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();

        // 1) FK：fund_code → funds(code) ON DELETE RESTRICT。直接引用不存在的基金应被约束拒绝。
        //    （注意：add_transaction 会自动创建关联基金，故此处绕过它直接 INSERT 以验证约束本身。）
        let fk_violation = with_conn(|conn| {
            conn.execute(
                "INSERT INTO transactions(account_id, txn_type, fund_code, amount, txn_date, source)
                 VALUES(?1,'buy','999999',100.0,'2026-05-01','manual_txn')",
                [acc],
            )
        });
        assert!(fk_violation.is_err(), "引用不存在的基金应触发外键约束失败");

        // 2) CHECK：txn_type 仅允许 7 种合法值（含手工调整 adjust）；非法值应被 CHECK 拒绝。
        let check_violation = with_conn(|conn| {
            conn.execute(
                "INSERT INTO transactions(account_id, txn_type, amount, txn_date, source)
                 VALUES(?1,'illegal',100.0,'2026-05-01','manual_txn')",
                [acc],
            )
        });
        assert!(check_violation.is_err(), "非法 txn_type 应触发 CHECK 约束失败");

        // 3) related_tx_id 自引用外键列存在且可回填（配对分红→红利再投）。
        let buy_id = add_transaction(
            acc, "buy", Some("000010".to_string()), Some(10.0), 100.0, None,
            "2026-05-02", "", None, "alipay",
        ).unwrap();
        with_conn(|conn| {
            conn.execute(
                "INSERT INTO transactions(account_id, txn_type, fund_code, shares, amount, txn_date, source, related_tx_id)
                 VALUES(?1,'dividend','000010',NULL,5.0,'2026-05-03','manual_txn',?2)",
                [acc, buy_id],
            )
        }).unwrap();
        let related: Option<i64> = with_conn(|conn| {
            conn.query_row(
                "SELECT related_tx_id FROM transactions WHERE txn_type='dividend'",
                [],
                |r| r.get(0),
            )
        }).unwrap();
        assert_eq!(related, Some(buy_id));

        // 4) snapshots 已增厚 platform / total_return_pct / max_drawdown_pct / 估算列。
        record_snapshot(acc, "2026-05-03", "alipay", 1100.0, 1000.0, 100.0, 10.0, 0.1, -0.02, 9.5, 1099.5).unwrap();
        let snaps = list_snapshots(acc).unwrap();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].platform, "alipay");
        assert!((snaps[0].total_return_pct - 0.1).abs() < 1e-9);
        assert!((snaps[0].max_drawdown_pct - (-0.02)).abs() < 1e-9);

        // 5) 外键清单应包含两条（fund_code→funds、related_tx_id→transactions）。
        let fk_count: i64 = with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_list('transactions')",
                [],
                |r| r.get(0),
            )
        }).unwrap();
        assert_eq!(fk_count, 2, "transactions 应含 2 条外键");
    }

    #[test]
    fn position_daily_table_indexes_and_roundtrip() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("逐日账户", "").unwrap();
        set_baseline(acc, "000020", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        // 取 positions.id 用于 position_daily 外键
        let pos_id: i64 = with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM positions WHERE account_id=?1 AND fund_code='000020'",
                [acc],
                |r| r.get(0),
            )
        })
        .unwrap();

        // 1) 三个新索引均存在
        let idx_count: i64 = with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name IN ('idx_disclosures_fund','idx_positions_fund','idx_nav_history_date','idx_position_daily_date')",
                [],
                |r| r.get(0),
            )
        })
        .unwrap();
        assert_eq!(idx_count, 4, "应存在 4 个新增索引");

        // 2) position_daily 写入/读取往返
        upsert_position_daily(
            pos_id, "2026-06-01", 100.0, 10.0, 1000.0, 1.05, 1.04, 1.045, 105.0,
            5.0, 4.0, 0.05, 0.04, false,
        )
        .unwrap();
        upsert_position_daily(
            pos_id, "2026-06-02", 100.0, 10.0, 1000.0, 1.07, 1.06, 1.065, 107.0,
            7.0, 6.0, 0.07, 0.06, false,
        )
        .unwrap();
        let rows = get_position_daily(pos_id).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].nav_date, "2026-06-01");
        assert!((rows[0].market_value - 105.0).abs() < 1e-9);
        assert!((rows[1].day_pnl_act - 7.0).abs() < 1e-9);

        // 3) 删除持仓应级联清空 position_daily（ON DELETE CASCADE）
        with_conn(|conn| conn.execute("DELETE FROM positions WHERE id=?1", [pos_id])).unwrap();
        let after: i64 = with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM position_daily WHERE position_id=?1",
                [pos_id],
                |r| r.get(0),
            )
        })
        .unwrap();
        assert_eq!(after, 0, "持仓删除应级联清空逐日估值");
    }

    #[test]
    fn est_cache_roundtrip_and_refresh() {
        let _g = lock_db_tests();
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

    #[test]
    fn backup_export_and_restore_roundtrip() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("备份账户", "").unwrap();
        set_baseline(acc, "000009", 10.0, 100.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 1);

        // 导出当前活动库为独立备份文件
        let dest = std::env::temp_dir().join(format!("fundlens_backup_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        export_db_backup(&dest).unwrap();
        assert!(dest.is_file(), "备份文件应已生成");

        // 备份文件可独立打开并含数据（验证导出方向：live -> dest）
        let check = rusqlite::Connection::open_with_flags(
            &dest,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let n: i64 = check
            .query_row("SELECT COUNT(*) FROM positions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "备份文件应含 1 条持仓");

        // 破坏活动库数据，随后从备份恢复
        delete_fund("000009").unwrap();
        assert!(list_holdings(Some(acc)).unwrap().is_empty(), "删除后应为空");

        import_db_backup(&dest).unwrap();
        let restored = list_holdings(Some(acc)).unwrap();
        assert_eq!(restored.len(), 1, "恢复后应重新出现 1 条持仓");
        assert!((restored[0].shares - 10.0).abs() < 1e-6);

        let _ = std::fs::remove_file(&dest);
    }

    /// 【v9】删除/撤销流水 = 反向撤销其对持仓的增量效果（不再全量重放）。
    /// 紧邻撤销精确还原（本测试覆盖）；跨交易顺序撤销为有界近似。
    #[test]
    fn delete_transaction_reverses_position_incrementally() {
        let _g = lock_db_tests();
        init_temp_db();
        let acc = create_account("撤销账户", "").unwrap();
        // 基线 100 份 / 1000（不产生流水）
        set_baseline(acc, "000021", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        // 买入 50 份 / 500 → 150 份 / 1500
        let buy_id = add_transaction(acc, "buy", Some("000021".to_string()), Some(50.0), 500.0, None, "2026-01-01", "", None, "alipay").unwrap();
        // 卖出 30 份 → 按均价 10 扣成本 300 → 120 份 / 1200
        let sell_id = add_transaction(acc, "sell", Some("000021".to_string()), Some(30.0), 360.0, None, "2026-02-01", "", None, "alipay").unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 120.0);
        assert!((hs[0].cost_amount - 1200.0).abs() < 1e-6);

        // 撤销卖出（紧邻）→ 精确还原 150 份 / 1500
        delete_transaction(sell_id).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 150.0);
        assert!((hs[0].cost_amount - 1500.0).abs() < 1e-6);
        // 撤销买入 → 还原基线 100 份 / 1000
        delete_transaction(buy_id).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        assert_eq!(hs[0].shares, 100.0);
        assert!((hs[0].cost_amount - 1000.0).abs() < 1e-6);
        assert!(list_transactions(Some(acc), None).unwrap().is_empty(), "撤销后账本应为空");

        // 全部卖出清仓后删除该卖出 → 份额回补（成本无法溯源故为 0，记录有界近似的边界）
        let sell_all = add_transaction(acc, "sell", Some("000021".to_string()), Some(100.0), 1100.0, None, "2026-03-01", "", None, "alipay").unwrap();
        assert!(list_holdings(Some(acc)).unwrap().iter().all(|h| h.code != "000021"), "清仓后无持仓");
        delete_transaction(sell_all).unwrap();
        let hs = list_holdings(Some(acc)).unwrap();
        let h = hs.iter().find(|h| h.code == "000021").expect("撤销清仓卖出后应重建持仓");
        assert!((h.shares - 100.0).abs() < 1e-6);
    }
}

