// 免费公开数据源（v0.1）
// - 个股实时行情：腾讯财经 qt.gtimg.cn / 东方财富 push2（A 股，红涨绿跌）
// - 基金披露持仓：天天基金/东方财富 F10（按当前可获取的最新报告期）
// 详见 SPEC.md 第 4 节。本模块只做最小可用 HTTP 拉取 + 解析，失败安全返回 None。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Datelike;
use chrono::NaiveDate;
use chrono::Timelike;

use crate::valuation::{DisclosedHolding, StockQuote};

/// 东财接口返回 GBK 编码，reqwest 默认按 UTF-8 解码会乱码；统一用 encoding_rs 解码。
fn decode_gbk(bytes: &[u8]) -> String {
    let (cow, _, _) = encoding_rs::GBK.decode(bytes);
    cow.into_owned()
}

/// 智能解码：优先按 UTF-8，失败（如腾讯行情的 GBK）再回退 GBK。
/// 适用「内容编码不确定」的接口（如东财 F10 披露持仓接口当前返回 UTF-8）。
fn decode_body(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_gbk(bytes),
    }
}

// ============ 全局出站请求节流（避免被腾讯/东财限流）============
// 所有公开数据源（腾讯 qt.gtimg.cn、东财 push2/eastmoney/tenorfun）共享同一节奏：
// 任意两次出站请求「发起时刻」间隔 ≥ MIN_REQ_INTERVAL，杜绝瞬时并发洪泛触发限流。
// 实现要点：仅在持锁瞬间「预约下一时隙」（不持锁 sleep），释放锁后再 sleep，
// 因而并行线程会被串行化到各自时隙，但 HTTP 本身仍可有限重叠（受单次超时约束）。
static LAST_REQ: OnceLock<Mutex<Instant>> = OnceLock::new();
const MIN_REQ_INTERVAL: Duration = Duration::from_millis(500);

fn throttle_wait() {
    let wait = {
        let last = LAST_REQ.get_or_init(|| Mutex::new(Instant::now()));
        let mut guard = last.lock().unwrap();
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(*guard);
        let w = if elapsed < MIN_REQ_INTERVAL {
            MIN_REQ_INTERVAL - elapsed
        } else {
            Duration::ZERO
        };
        *guard = now + w; // 预约下一时隙
        w
    };
    if wait > Duration::ZERO {
        std::thread::sleep(wait);
    }
}

// ============ A 股交易日历 ============
// 仅用 weekday<5 会漏掉春节/国庆/清明等法定节假日（即便周一到周五也休市）。
// 这里内置近年休市日兜底，并在启动时从上交所口径的公开源（holiday-cn，国务院放假安排）
// 拉取完整交易日历写入 SQLite（trading_calendar 表），识别「交易日」= 周一至周五 且 非休市日。
// 调休补班日落在周末，本就不开市，已被 weekend 短路覆盖，无需特殊处理。

/// 内置 A 股休市日（法定节假日，国务院放假安排中的 isOffDay=true 日期）。
/// 来源 holiday-cn，数据截至 2026 年；2027 及以后由启动时远程拉取补全。
/// 作为离线兜底：即使联网失败也能正确识别春节/国庆等长假的休市。
const BUILTIN_OFF_DAYS: &[&str] = &[
    // —— 2024 ——
    "2024-01-01", "2024-02-10", "2024-02-11", "2024-02-12", "2024-02-13", "2024-02-14",
    "2024-02-15", "2024-02-16", "2024-02-17", "2024-04-04", "2024-04-05", "2024-04-06",
    "2024-05-01", "2024-05-02", "2024-05-03", "2024-05-04", "2024-05-05", "2024-06-10",
    "2024-09-15", "2024-09-16", "2024-09-17", "2024-10-01", "2024-10-02", "2024-10-03",
    "2024-10-04", "2024-10-05", "2024-10-06", "2024-10-07",
    // —— 2025 ——
    "2025-01-01", "2025-01-28", "2025-01-29", "2025-01-30", "2025-01-31", "2025-02-01",
    "2025-02-02", "2025-02-03", "2025-02-04", "2025-04-04", "2025-04-05", "2025-04-06",
    "2025-05-01", "2025-05-02", "2025-05-03", "2025-05-04", "2025-05-05", "2025-05-31",
    "2025-06-01", "2025-06-02", "2025-10-01", "2025-10-02", "2025-10-03", "2025-10-04",
    "2025-10-05", "2025-10-06", "2025-10-07", "2025-10-08",
    // —— 2026 ——
    "2026-01-01", "2026-01-02", "2026-01-03", "2026-02-15", "2026-02-16", "2026-02-17",
    "2026-02-18", "2026-02-19", "2026-02-20", "2026-02-21", "2026-02-22", "2026-02-23",
    "2026-04-04", "2026-04-05", "2026-04-06", "2026-05-01", "2026-05-02", "2026-05-03",
    "2026-05-04", "2026-05-05", "2026-06-19", "2026-06-20", "2026-06-21", "2026-09-25",
    "2026-09-26", "2026-09-27", "2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04",
    "2026-10-05", "2026-10-06", "2026-10-07",
];

/// 内存缓存：cal_date -> is_open，避免每次请求都查 DB。启动时由 DB/远程预热。
static CAL_CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
/// 已离线加载（DB + 内置）的年份集合，避免重复预热。
static CAL_LOADED: OnceLock<Mutex<std::collections::HashSet<i32>>> = OnceLock::new();

fn cal_cache() -> &'static Mutex<HashMap<String, bool>> {
    CAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn cal_loaded() -> &'static Mutex<std::collections::HashSet<i32>> {
    CAL_LOADED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 离线兜底预热：DB 已有数据优先，缺失处用内置休市日补全。不联网，保证 is_trading_day 永远廉价可用。
fn ensure_loaded_offline(year: i32) {
    if cal_loaded().lock().unwrap().contains(&year) {
        return;
    }
    if crate::db::db_ready() {
        if let Ok(rows) = crate::db::load_calendar_year_from_db(year) {
            let mut cache = cal_cache().lock().unwrap();
            for (d, open) in rows {
                cache.entry(d).or_insert(open); // DB（含历史远程）优先，不覆盖
            }
        }
    }
    let prefix = format!("{year}-");
    let mut cache = cal_cache().lock().unwrap();
    for d in BUILTIN_OFF_DAYS.iter().filter(|s| s.starts_with(&prefix)) {
        cache.entry((*d).to_string()).or_insert(false); // 内置仅为兜底补全
    }
    cal_loaded().lock().unwrap().insert(year);
}

#[derive(serde::Deserialize)]
struct HDay {
    date: String,
    #[serde(rename = "isOffDay")]
    is_off_day: bool,
}
#[derive(serde::Deserialize)]
struct HYear {
    days: Vec<HDay>,
}

/// 从上交所口径公开源（holiday-cn，国务院放假安排）拉取某年交易日历，覆盖刷新缓存与 DB。
/// 失败安全：网络异常/超时/解析失败一律返回 false，绝不影响主流程（内置兜底仍在）。
fn load_year_remote(year: i32) -> bool {
    let url = format!(
        "https://cdn.jsdelivr.net/gh/NateScarlet/holiday-cn@master/{year}.json"
    );
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[calendar] {year} 客户端构建失败: {e}");
            return false;
        }
    };
    throttle_wait(); // 复用全局出站节流，避免瞬时洪泛
    let resp = match client
        .get(&url)
        .header("User-Agent", "FundLens/0.1")
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[calendar] {year} 交易日历拉取失败: {e}");
            return false;
        }
    };
    let body = match resp.text() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[calendar] {year} 交易日历读取失败: {e}");
            return false;
        }
    };
    let parsed: HYear = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[calendar] {year} 交易日历解析失败: {e}");
            return false;
        }
    };
    let mut batch: Vec<(String, bool, &'static str)> = Vec::new();
    {
        let mut cache = cal_cache().lock().unwrap();
        for d in &parsed.days {
            let is_open = !d.is_off_day;
            cache.insert(d.date.clone(), is_open); // 远程覆盖一切
            batch.push((d.date.clone(), is_open, "remote"));
        }
    }
    if crate::db::db_ready() {
        let _ = crate::db::upsert_calendar_days(&batch); // 持久化，下次启动无需联网
    }
    true
}

/// 启动时（非阻塞线程）调用：加载当前年与下一年的交易日历，远程覆盖、内置兜底。
pub fn refresh_calendar() {
    let yr = chrono::Local::now().year();
    for y in [yr, yr + 1] {
        ensure_loaded_offline(y);
        load_year_remote(y);
    }
}

/// 指定日期是否为 A 股交易日（周一至周五 且 非法定休市日）。
/// 周末与春节/国庆/清明等长假一律休市；调休补班日落在周末，已被 weekend 短路覆盖。
/// 命中缓存直接返回；未知工作日（本地无数据）宁可视为开市，避免误杀估值，待远程刷新纠偏。
pub fn is_trading_day(date: NaiveDate) -> bool {
    let wd = date.weekday();
    if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
        return false;
    }
    ensure_loaded_offline(date.year());
    let key = date.format("%Y-%m-%d").to_string();
    match cal_cache().lock().unwrap().get(&key) {
        Some(&v) => v,
        None => true,
    }
}

/// 只读缓存版交易日判断：仅用已加载的交易日历内存判断，**不触发 DB 加载**。
/// 供「已持有 DB 连接锁」的路径（如 import_transactions / init_db 内的份额反推）调用，
/// 避免嵌套加锁死锁（ensure_loaded_offline → load_calendar_year_from_db 会再拿 DB 锁）。
/// 周末一定休市；法定休市日在已加载缓存中则跳过；缓存未命中的年份按开市处理（保守）。
pub fn is_trading_day_cached(date: NaiveDate) -> bool {
    let wd = date.weekday();
    if wd == chrono::Weekday::Sat || wd == chrono::Weekday::Sun {
        return false;
    }
    let key = date.format("%Y-%m-%d").to_string();
    match cal_cache().lock().unwrap().get(&key) {
        Some(&v) => v,
        None => true,
    }
}

/// 今天是否为 A 股交易日（用于判断平台是否已发布当日 gsz/净值更新）。
pub fn is_trading_day_now() -> bool {
    is_trading_day(chrono::Local::now().date_naive())
}

/// 当前是否处于 A 股交易时段（9:30-11:30, 13:00-15:00，且当日为交易日）。
pub fn is_trading_now() -> bool {
    if !is_trading_day_now() {
        return false;
    }
    let now = chrono::Local::now();
    let secs = now.num_seconds_from_midnight();
    let morning = secs >= 9 * 3600 + 30 * 60 && secs <= 11 * 3600 + 30 * 60;
    let afternoon = secs >= 13 * 3600 && secs <= 15 * 3600;
    morning || afternoon
}

/// 市场时段三态，用于头条口径切换：
/// - `intraday`   盘中（当日为交易日且处于 9:30-11:30 / 11:30-13:00 午休 / 13:00-15:00）：
///                平台 gsz 实时估算有效，头条用「当日估算」。午休虽无交易，但仍属当日盘中暂停，
///                不应与盘后混为一谈。
/// - `post_close` 交易日下午 15:00 之后（或盘前 9:30 之前）：当日交易已结束，官方净值尚未/已发布，
///                头条优先用「当日实际官方净值」。
/// - `closed`     非交易日（周末/节假日）：没有当日净值变动，头条展示上一交易日的实际值。
///
/// 注意：官方净值通常在盘后 18:00-22:00 才发布，因此在 `post_close` 早期可能尚无「当日」官方净值；
/// 上层应进一步用 `nav_date` 与今日比较来区分「今日实际」与「上一交易日实际」，本函数只负责时段粗分。
pub fn market_phase() -> &'static str {
    if !is_trading_day_now() {
        return "closed";
    }
    if is_trading_now() {
        return "intraday";
    }
    // 午休（11:30-13:00）与盘前/盘后区分：午休视为盘中暂停，仍展示当日估算
    let now = chrono::Local::now();
    let secs = now.num_seconds_from_midnight();
    let morning_end = 11 * 3600 + 30 * 60;
    let afternoon_start = 13 * 3600;
    if secs > morning_end && secs < afternoon_start {
        return "intraday";
    }
    "post_close"
}

/// 穿透估值用的基准指数：按「基金类型 + 基金名称」识别标的指数，
/// 替换原先「所有权益类统一用沪深300」的粗放做法。
///
/// 为什么更准：指数/ETF联接基金的未披露部分（现金+其余成分）本就是其跟踪指数的成分，
/// 用真实标的指数近似在数学上远比沪深300贴近；债券基金未披露部分多为债券，用国债指数比沪深300准。
///
/// 返回 (gtimg_symbol, digit_code, 中文名)。失败安全：网络异常时调用方退化为零波动近似。
pub fn pick_benchmark(fund_type: &str, fund_name: &str) -> (String, String, String) {
    let name = fund_name.to_lowercase();
    // 0) 港股/恒生类指数：腾讯 gtimg 不覆盖港股行业指数（如恒生医疗保健 hkHSHCI 返回 none_match），
    //    故统一用新浪兜底符号（hk 前缀），在估值链路里按前缀分流到 fetch_hk_index_quotes。
    //    此分支必须放在通用「医疗/医药」等规则之前，否则「恒生医疗保健」会被误判为「中证医疗」。
    let hk_rules: &[(&str, &str, &str, &str)] = &[
        ("恒生医疗保健", "hkHSHCI", "HSHCI", "恒生医疗保健指数"),
        ("恒生医疗", "hkHSHCI", "HSHCI", "恒生医疗保健指数"),
        ("香港医疗", "hkHSHCI", "HSHCI", "恒生医疗保健指数"),
        ("恒生科技", "hkHSTECH", "HSTECH", "恒生科技指数"),
        ("恒生指数", "hkHSI", "HSI", "恒生指数"),
        ("恒生国企", "hkHSCEI", "HSCEI", "国企指数"),
        ("国企指数", "hkHSCEI", "HSCEI", "国企指数"),
    ];
    for (kw, sym, code, cn) in hk_rules {
        if name.contains(&kw.to_lowercase()) {
            return (sym.to_string(), code.to_string(), cn.to_string());
        }
    }
    // 1) 名称关键字优先匹配具体标的指数（覆盖绝大多数宽基/热门指数基金、ETF 与行业/主题基金）
    //    (关键字, gtimg符号, 数字代码, 中文名)
    let name_rules: &[(&str, &str, &str, &str)] = &[
        // —— 宽基 / 热门指数（既有）——
        ("沪深300", "sh000300", "000300", "沪深300"),
        ("中证500", "sh000905", "000905", "中证500"),
        ("中证1000", "sh000852", "000852", "中证1000"),
        ("创业板", "sz399006", "399006", "创业板指"),
        ("科创50", "sh000688", "000688", "科创50"),
        ("上证50", "sh000016", "000016", "上证50"),
        ("中证红利", "sh000922", "000922", "中证红利"),
        ("红利低波", "sh000922", "000922", "中证红利"),
        ("深证成指", "sz399001", "399001", "深证成指"),
        // —— 行业 / 主题指数（未披露部分用对应行业指数近似，而非笼统沪深300）——
        // 新能源车 / 光伏 / 新能源（顺序：新能源车 必须在 新能源 之前）
        ("新能源车", "sz399976", "399976", "CS新能车"),
        ("光伏", "sz399808", "399808", "中证新能"),
        ("新能源", "sz399808", "399808", "中证新能"),
        // 白酒 / 酒（顺序：必须在 消费 之前）
        ("白酒", "sz399997", "399997", "中证白酒"),
        ("酒", "sz399997", "399997", "中证白酒"),
        // 医疗 / 医药（顺序：医疗 必须在 医药 之前）
        ("医疗", "sz399989", "399989", "中证医疗"),
        ("医药", "sh000933", "000933", "中证医药"),
        // 金融地产
        ("券商", "sz399975", "399975", "证券公司"),
        ("证券", "sz399975", "399975", "证券公司"),
        ("银行", "sz399986", "399986", "中证银行"),
        ("地产", "sz399393", "399393", "国证地产"),
        // 科技制造
        ("军工", "sz399967", "399967", "中证军工"),
        ("芯片", "sz980017", "980017", "国证芯片"),
        ("半导体", "sz980017", "980017", "国证芯片"),
        ("电子", "sz399811", "399811", "CSSW电子"),
        ("传媒", "sz399971", "399971", "中证传媒"),
        // 周期与资源
        ("煤炭", "sz399998", "399998", "中证煤炭"),
        ("钢铁", "sz399440", "399440", "国证钢铁"),
        ("有色", "sz399395", "399395", "国证有色"),
        ("基建", "sz399995", "399995", "基建工程"),
        // 其他主题
        ("消费", "sh000932", "000932", "中证消费"),
        ("农业", "sh000949", "000949", "中证农业"),
        ("环保", "sh000827", "000827", "中证环保"),
    ];
    for (kw, sym, code, cn) in name_rules {
        if name.contains(&kw.to_lowercase()) {
            return (sym.to_string(), code.to_string(), cn.to_string());
        }
    }
    // 2) 按类型兜底：债券→国债指数；其余权益类（含指数/ETF联接/QDII/货币）→沪深300 宽基代理
    match fund_type {
        "004" => ("sh000012".to_string(), "000012".to_string(), "上证国债".to_string()),
        _ => ("sh000300".to_string(), "000300".to_string(), "沪深300".to_string()),
    }
}

/// 是否为被动指数型基金：类型码 008(指数型)/009(ETF联接)/006(分级)，
/// 或名称含 指数 / ETF / 联接 / 指数增强。
///
/// 指数 ETF 跟踪误差极小，盘中涨跌≈跟踪指数，适用「指数实时估值优先」：
/// 头条估值直接采用跟踪指数当日涨跌，成分股穿透降为参考口径。
/// 主动管理 / 债基 / 货基 / QDII 返回 false，走通用本地穿透自算（含基准默认近似）。
pub fn is_index_fund(fund_type: &str, fund_name: &str) -> bool {
    match fund_type {
        "008" | "009" | "006" => return true,
        _ => {}
    }
    let name = fund_name.to_lowercase();
    name.contains("指数") || name.contains("etf") || name.contains("联接") || name.contains("指数增强")
}

/// 是否为「纯被动指数型基金」（适用「指数实时估值优先」头条路径）：
/// 类型码 008(指数型)/009(ETF联接)/006(分级母基金)，或名称含 指数/ETF/联接（**不含** 指数增强）。
///
/// 与 `is_index_fund` 的区别：指数增强有主动超额、跟踪误差不可忽略，不应直接用纯指数涨跌
/// 代理净值，改走「本地持仓穿透自算」口径（披露成分穿透 + 未披露部分按跟踪指数近似）。
/// 但指数增强仍属指数型基金（is_index_fund=true），以便未披露部分按跟踪指数近似。
///
/// 这是「ETF联接/被动指数直接用指数实时涨跌代理净值」原则的精确落点：仅纯被动才取纯指数头条。
pub fn is_pure_index_fund(fund_type: &str, fund_name: &str) -> bool {
    if fund_name.to_lowercase().contains("指数增强") {
        return false; // 增强型：不纳入纯指数头条，改走穿透口径
    }
    is_index_fund(fund_type, fund_name)
}

/// 解析基金「真实跟踪指数」行情符号（供指数代理路径使用）：
/// 优先用库里存储的 `track_index`（如 "hkHSHCI" / "sh000300"），否则按名称/类型推断（pick_benchmark）。
/// 仅对指数/ETF联接类基金返回 Some；主动/债/货基返回 None（无「指数代理」可言）。
///
/// 这是竞品「ETF联接直接用所跟踪指数实时涨跌代理净值」原则的落点：优先读库里真实 track_index，
/// 而非从名称瞎猜（名称瞎猜会误把「恒生医疗保健」判成「中证医疗」）。
pub fn resolve_tracked_index(fund_type: &str, fund_name: &str, stored: &str) -> Option<(String, String, String)> {
    let s = stored.trim();
    if !s.is_empty() {
        // 库里存的是行情符号（gtimg/sina 通用）；港股用 hk 前缀，A 股用 sh/sz 前缀。
        let code: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
        return Some((s.to_string(), code, s.to_string()));
    }
    if is_index_fund(fund_type, fund_name) {
        let (sym, code, cn) = pick_benchmark(fund_type, fund_name);
        return Some((sym, code, cn));
    }
    None
}

/// 解析新浪港股指数行情行：
/// `var hq_str_hkHSHCI="HSHCI,恒生医疗保健指数,今开,昨收,最高,最低,现价,涨跌,涨跌幅%,..."`
/// 返回 StockQuote（price=现价[6]，prev_close=昨收[3]）。解析失败返回 None。
///
/// 与 A 股 gtimg 格式不同：新浪港股指数用 `,` 分隔、字段布局为 [0]代码 [1]名称 [3]昨收 [6]现价，
/// 故单独解析，便于单测（传入固定字符串，无需真实网络）。
pub fn parse_sina_index_quote(line: &str, sym: &str) -> Option<StockQuote> {
    let marker = format!("hq_str_{}=\"", sym);
    let start = line.find(&marker)? + marker.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    let payload = &rest[..end];
    let parts: Vec<&str> = payload.split(',').collect();
    // 港股指数：[0]=代码 [1]=名称 [3]=昨收 [6]=现价
    if parts.len() <= 6 {
        return None;
    }
    let price: f64 = parts[6].parse().ok()?;
    let prev_close: f64 = parts[3].parse().ok()?;
    let name = if parts.len() > 1 {
        parts[1].to_string()
    } else {
        String::new()
    };
    Some(StockQuote {
        stock_code: sym.trim_start_matches("hk").to_string(),
        name,
        price,
        prev_close,
    })
}

/// 拉取港股指数行情（新浪 hq.sinajs.cn）。
/// 腾讯 gtimg 不覆盖港股行业指数（如 hkHSHCI 返回 v_pv_none_match），故用新浪兜底。
/// 返回 key = 完整符号（如 "hkHSHCI"），与 A 股指数按数字代码建索引不同。
/// 需带 Referer，否则新浪返回空。网络异常失败安全：返回空集。
pub fn fetch_hk_index_quotes(symbols: &[String]) -> HashMap<String, StockQuote> {
    let mut out: HashMap<String, StockQuote> = HashMap::new();
    if symbols.is_empty() {
        return out;
    }
    throttle_wait(); // 新浪同样需要节流，避免频繁请求
    let joined = symbols.join(",");
    let url = format!("https://hq.sinajs.cn/list={joined}");
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
    {
        Ok(c) => c,
        Err(_) => return out,
    };
    let resp = match client
        .get(&url)
        .header("Referer", "https://finance.sina.com.cn")
        .send()
    {
        Ok(r) => r,
        Err(_) => return out,
    };
    let bytes = match resp.bytes() {
        Ok(b) => b,
        Err(_) => return out,
    };
    let body = decode_gbk(&bytes);
    for sym in symbols {
        if let Some(q) = parse_sina_index_quote(&body, sym) {
            out.insert(sym.clone(), q);
        }
    }
    out
}

/// 拉取一批实时行情（腾讯 qt.gtimg.cn 格式，GBK 编码）
/// codes: 如 ["sh600519", "sz000858", "hk00700"]
/// 返回 key = 纯数字代码（如 "600519"/"00700"），与披露持仓 stock_code 对齐。
pub fn fetch_quotes(codes: &[String]) -> Option<HashMap<String, StockQuote>> {
    if codes.is_empty() {
        return Some(HashMap::new());
    }
    throttle_wait(); // 腾讯 qt.gtimg.cn：发起前节流
    let joined = codes.join(",");
    let url = format!("https://qt.gtimg.cn/q={joined}");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    let bytes = resp.bytes().ok()?;
    let body = decode_gbk(&bytes);

    let mut map = HashMap::new();
    for line in body.lines() {
        let line = line.trim();
        // 仅处理 v_xxx="..." 形式；v_xxx 即变量名，含交易所前缀与代码
        if !line.starts_with("v_") {
            continue;
        }
        // 格式：v_hk00700="100~腾讯控股~00700~441.000~461.600~..."
        let eq = match line.find('=') {
            Some(i) => i,
            None => continue,
        };
        // 变量名中的数字即为真实代码（去掉 v_ 前缀后取数字）
        let stock_code: String = line[..eq]
            .trim_start_matches("v_")
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect();
        if stock_code.is_empty() {
            continue;
        }
        // 取值部分（两引号之间）
        let rest = &line[eq + 1..];
        let vstart = match rest.find('"') {
            Some(i) => i + 1,
            None => continue,
        };
        let vend = match rest[vstart..].find('"') {
            Some(i) => vstart + i,
            None => continue,
        };
        let payload = &rest[vstart..vend];
        let parts: Vec<&str> = payload.split('~').collect();
        // parts: [状态, 名称, 代码, 当前价, 昨收, ...]
        if parts.len() < 5 {
            continue;
        }
        let price: f64 = parts[3].parse().unwrap_or(0.0);
        let prev_close: f64 = parts[4].parse().unwrap_or(0.0);
        let name = if parts.len() > 1 {
            parts[1].to_string()
        } else {
            String::new()
        };
        map.insert(
            stock_code.clone(),
            StockQuote {
                stock_code,
                name,
                price,
                prev_close,
            },
        );
    }
    Some(map)
}

/// 拉取基金披露持仓（东方财富 F10，按最新报告期）
/// 返回 (report_period, disclosure_type, holdings)
/// 注：真实环境需按"财务数据时效性原则"动态选择最新披露期（季报/中报/年报）。
/// 报告期结束日（用于时效排序与「是否已发生」判断）。
/// 季末：一季报 3/31、中报 6/30、三季报 9/30、年报 12/31。
fn period_end(year: i32, season: u32) -> chrono::NaiveDate {
    match season {
        1 => chrono::NaiveDate::from_ymd_opt(year, 3, 31).unwrap_or(chrono::NaiveDate::MAX),
        2 => chrono::NaiveDate::from_ymd_opt(year, 6, 30).unwrap_or(chrono::NaiveDate::MAX),
        3 => chrono::NaiveDate::from_ymd_opt(year, 9, 30).unwrap_or(chrono::NaiveDate::MAX),
        4 => chrono::NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(chrono::NaiveDate::MAX),
        _ => chrono::NaiveDate::MAX,
    }
}

/// 根据当前日期动态选取「最新可获取报告期」的抓取候选列表。
///
/// 财务数据时效性原则（A 股）：一季报(季末 3/31，4 月底前披露)、中报(6/30，8 月底前)、
/// 三季报(9/30，10 月底前)、年报(12/31，次年 4 月底前)。故「此刻能拉到的最新一期」
/// = 报告期结束日（period_end）已过、处于披露窗口内的那一期；该期若尚未披露则逐期回退。
///
/// 返回按报告期结束日【从新到旧】排序的候选 (year, season, disclosure_type)：
/// - 中报(2)/年报(4) 披露全持仓 → "full"
/// - 一季报(1)/三季报(3) 仅披露前十大 → "top10"
/// 调用方按顺序尝试，首个返回非空持仓的即为准——天然优先最新期次，且对「该期尚未披露」
/// 的基金自动回退到上一期（如 8 月初部分基金中报未出 → 回退到当年一季报）。
fn candidate_periods(now: chrono::NaiveDate) -> Vec<(i32, u32, &'static str)> {
    let mut all: Vec<(i32, u32, &'static str)> = Vec::new();
    // 覆盖当前年及往前 3 年（足够回退），不前瞻未发生的财年
    for year in (now.year() - 3)..=now.year() {
        all.push((year, 1, "top10")); // 一季报
        all.push((year, 2, "full")); // 中报（全持仓）
        all.push((year, 3, "top10")); // 三季报
        all.push((year, 4, "full")); // 年报（全持仓）
    }
    // 仅保留「报告期已结束」的期次（period_end <= now），避免抓取尚未发生的季度
    all.retain(|(y, s, _)| period_end(*y, *s) <= now);
    // 按报告期结束日从新到旧排序
    all.sort_by(|a, b| period_end(b.0, b.1).cmp(&period_end(a.0, a.1)));
    // 限制最多尝试 12 期，避免极端情况下过多网络请求
    all.truncate(12);
    all
}

pub fn fetch_disclosure(fund_code: &str) -> Option<(String, String, Vec<DisclosedHolding>)> {
    // 动态选取最新可获取报告期候选（按当前日期），优先尝试最新期次，逐期回退。
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let now = chrono::Local::now().naive_local().date();
    for (year, season, dtype) in candidate_periods(now) {
        throttle_wait(); // 东财 F10 披露接口：每次候选期请求节流
        let url = format!(
            "https://fundf10.eastmoney.com/FundArchivesDatas.aspx?type=jjcc&code={}&topline=10&year={}&season={}",
            fund_code, year, season
        );
        let resp = match client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .header("Referer", "https://fundf10.eastmoney.com/")
            .send()
        {
            Ok(r) => r,
            Err(_) => continue,
        };
        let status = resp.status();
        let bytes = match resp.bytes() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let body = decode_body(&bytes);
        let mut rows = parse_holding_table(&body);
        // 用当前报告期/口径覆盖（解析器只抽字段，不判断口径）
        let report_period =
            extract_curyear(&body).unwrap_or_else(|| format!("{}Q{}", year, season));
        for h in &mut rows {
            h.disclosure_type = dtype.to_string();
            h.report_period = report_period.clone();
        }
        #[cfg(debug_assertions)]
        eprintln!(
            "[FundLens][dev] disclosure year={year} season={season} status={} rows={}",
            status,
            rows.len()
        );
        if !rows.is_empty() {
            return Some((report_period, dtype.to_string(), rows));
        }
    }
    None
}

fn extract_curyear(body: &str) -> Option<String> {
    // 报告期标题形如「2026年2季度」「2026年第2季度」「2026年第二季度」。
    // 从「YYYY年N季」提取（N∈{1,2,3,4}，支持数字或中文数字一二三四，容忍「第」字），
    // 避免误命中正文日期（如「2026年08月13日」不含『季』字）。
    // 东财会把同一基金多季度各渲染一张表，只取第一张 <tbody>（最新报告期），故首个匹配即最新期。
    let quarter_of = |token: char| -> Option<u32> {
        match token {
            '1' | '一' => Some(1),
            '2' | '二' => Some(2),
            '3' | '三' => Some(3),
            '4' | '四' => Some(4),
            _ => None,
        }
    };
    // 关键修正：原始实现用字节偏移 body[abs-4..abs] 回退取年份，当中文字符（3 字节）出现在
    // 「年」前 4 字节内时，abs-4 会落在某个汉字的中间，触发
    // "start byte index is not a char boundary" panic 并令整个 app 崩溃。
    // 这里改用字符向量索引，保证 year 始终由完整字符组成，绝不会越字符边界。
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        if chars[i] != '年' {
            i += 1;
            continue;
        }
        // 「年」前须有 4 位年份数字（字符维度，天然对齐边界）
        if i < 4 {
            i += 1;
            continue;
        }
        let year: String = chars[i - 4..i].iter().collect();
        if !year.chars().all(|c| c.is_ascii_digit()) {
            i += 1;
            continue;
        }
        // 取「年」之后最多 16 个字符作为报告期标题窗口（按字符截取，避免字节越界）
        let end = std::cmp::min(n, i + 1 + 16);
        let rest: String = chars[i + 1..end].iter().collect();
        // 窗口内须含「季」字（日期如「08月」不含『季』，从而排除）；取其前最近的有效季度标记
        if let Some(p) = rest.find('季') {
            let before: String = rest[..p].chars().collect();
            if let Some(last) = before.chars().last() {
                if let Some(qn) = quarter_of(last) {
                    return Some(format!("{}Q{}", year, qn));
                }
            }
        }
        i += 1;
    }
    // 回退：curyear: 标记（部分接口直接给出年份）
    if let Some(idx) = body.find("curyear:") {
        let rest = &body[idx + "curyear:".len()..];
        let year: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if year.len() == 4 {
            return Some(format!("{}Q?", year));
        }
    }
    None
}

// 从东财 jjcc 的 HTML 表格行提取「股票代码 / 股票名称 / 占净值比例」。
// 注意：表格在 名称 与 占净值 之间有一列「股吧/行情」链接列，列序不固定，
// 因此不依赖固定下标，而是按「5~6 位纯数字=代码」「以 % 结尾的数字=占比」定位。
// 解析策略：先以 </td> 作为单元格分隔符（替换为 |），再整体剥离 HTML 标签，
// 这样单元格边界保留、标签残留被清除——避免「取最后一个 > 之后」在
// 「>00700</a>」此类结构下取到空串的问题。
fn parse_holding_table(body: &str) -> Vec<DisclosedHolding> {
    // 东财会把同一基金的多个季度快照（如 2025Q1~Q4）各渲染一张表。
    // 只取第一张 <tbody>（最新报告期）的行，避免跨季度持仓被合并成「曾持有」集合。
    let region = if let Some(s) = body.find("<tbody") {
        let after = &body[s..];
        if let Some(e) = after.find("</tbody>") {
            &after[..e]
        } else {
            body
        }
    } else {
        body
    };
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for tr in region.split("</tr>") {
        let delimited = tr.replace("</td>", "|").replace("</th>", "|");
        let plain = strip_tags(&delimited);
        let cells: Vec<String> = plain
            .split('|')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();

        // 股票代码：5~6 位纯数字（A 股 6 位，如 600519；港股 5 位带前导零，如 00700）
        let code_idx = cells.iter().position(|c| {
            (5..=6).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_digit())
        });
        let code_idx = match code_idx {
            Some(i) => i,
            None => continue,
        };
        let code = cells[code_idx].clone();

        // 同接口常把同一张持仓表重复返回多次，按代码去重，只保留首次出现
        if !seen.insert(code.clone()) {
            continue;
        }

        // 占净值比例：以 '%' 结尾的正数（如 9.96%）
        let weight = cells.iter().find_map(|c| {
            let trimmed = c.trim();
            if trimmed.ends_with('%') {
                let v = trimmed.trim_end_matches('%').replace(',', "").parse::<f64>().ok()?;
                if v > 0.0 {
                    return Some(v);
                }
            }
            None
        });
        let weight = match weight {
            Some(w) => w,
            None => continue,
        };

        // 名称：代码后一列
        let name = cells.get(code_idx + 1).cloned().unwrap_or_default();

        out.push(DisclosedHolding {
            stock_code: code,
            stock_name: name,
            weight: weight / 100.0,
            report_period: "latest".into(),
            disclosure_type: "top10".into(),
        });
    }
    out
}

/// 剥离 HTML 标签，保留标签之间的纯文本
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// 官方净值基线（替代已失效的 fundgz.1234567.com.cn）
/// 端点：东财 F10 历史净值 lsjz API（JSON，需 UA + Referer）
/// 返回 DWJZ(单位净值) / FSRQ(净值日期) / FundType(类型码)
#[derive(Debug, Clone)]
pub struct OfficialNav {
    pub nav: f64,
    pub nav_date: String,
    pub fund_type: String,
}

pub fn fetch_official_nav(fund_code: &str) -> Option<OfficialNav> {
    // 东财 F10 官方净值为主源；不收录的基金（如香港互认基金 968072，lsjz 返回空列表）
    // 回退到腾讯基金行情（qt.gtimg.cn q=jj{code}，提供官方单位净值 + 净值日期 + 日涨跌幅）。
    fetch_official_nav_eastmoney(fund_code).or_else(|| fetch_official_nav_tencent(fund_code))
}

/// 东财 F10 官方净值（主源）。对香港互认基金等不收录的代码返回 None（LSJZList 为空）。
fn fetch_official_nav_eastmoney(fund_code: &str) -> Option<OfficialNav> {
    let url = format!(
        "https://api.fund.eastmoney.com/f10/lsjz?fundCode={}&pageIndex=1&pageSize=1",
        fund_code
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let data = v.get("Data")?;
    let list = data.get("LSJZList")?.as_array()?;
    let first = list.first()?;
    let nav: f64 = first.get("DWJZ")?.as_str()?.parse().ok()?;
    let nav_date = first.get("FSRQ").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let fund_type = data.get("FundType").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(OfficialNav { nav, nav_date, fund_type })
}

/// 腾讯基金行情净值兜底：qt.gtimg.cn/q=jj{code}（GBK）。
/// 仅用于东财 F10 不收录的基金（香港互认基金等）；解析失败返回 None。
fn fetch_official_nav_tencent(fund_code: &str) -> Option<OfficialNav> {
    throttle_wait(); // 与实时行情共享节流节奏
    let url = format!("https://qt.gtimg.cn/q=jj{}", fund_code);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    let bytes = resp.bytes().ok()?;
    let body = decode_gbk(&bytes);
    let (nav, nav_date, _pct) = parse_tencent_fund_nav(&body)?;
    Some(OfficialNav { nav, nav_date, fund_type: String::new() })
}

/// 解析腾讯基金净值行情文本（纯函数，便于离线单测）。
/// 期望形如：v_jj968072="968072~摩根亚洲增长PRC人民币对冲累计~0.0000~0.0000~~17.7900~17.7900~-0.7808~2026-08-18~";
/// 字段（~ 分隔）：[0]代码 [1]名称 [5]单位净值 [7]日涨跌幅% [8]净值日期。
/// 返回 (单位净值, 净值日期, 日涨跌幅%)。
fn parse_tencent_fund_nav(body: &str) -> Option<(f64, String, f64)> {
    let line = body.lines().find(|l| l.trim_start().starts_with("v_jj"))?;
    let eq = line.find('=')?;
    let rest = &line[eq + 1..];
    let vstart = rest.find('"')? + 1;
    let vend = rest[vstart..].find('"')? + vstart;
    let payload = &rest[vstart..vend];
    let parts: Vec<&str> = payload.split('~').collect();
    if parts.len() < 9 {
        return None;
    }
    let nav: f64 = parts[5].parse().ok()?;
    if nav <= 0.0 {
        return None;
    }
    let nav_date = parts[8].trim().to_string();
    if nav_date.is_empty() {
        return None;
    }
    let pct: f64 = parts[7].parse().unwrap_or(0.0);
    Some((nav, nav_date, pct))
}

/// 同时拉取基金「最新」与「上一交易日」官方净值（pageSize=2）。
/// 返回值：`(latest, previous)`，previous 在接口只返回一条时可能为 None。
/// 用于刷新今日净值时一并维护 funds.prev_nav，避免官方接口可用时仍使用被污染的 est_cache 基准。
/// 东财 F10 不收录的基金（香港互认基金）回退腾讯 qt.gtimg.cn，昨收由日涨跌幅反推。
pub fn fetch_official_nav_with_prev(fund_code: &str) -> Option<(OfficialNav, Option<OfficialNav>)> {
    if let Some(res) = fetch_official_nav_eastmoney_with_prev(fund_code) {
        return Some(res);
    }
    // 腾讯兜底：单条最新净值 + 日涨跌幅，昨收 = nav/(1+pct/100)
    throttle_wait();
    let url = format!("https://qt.gtimg.cn/q=jj{}", fund_code);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client.get(&url).send().ok()?;
    let bytes = resp.bytes().ok()?;
    let body = decode_gbk(&bytes);
    let (nav, nav_date, pct) = parse_tencent_fund_nav(&body)?;
    let latest = OfficialNav {
        nav,
        nav_date,
        fund_type: String::new(),
    };
    let prev = if pct != 0.0 && nav > 0.0 {
        let prev_nav = nav / (1.0 + pct / 100.0);
        if prev_nav > 0.0 {
            Some(OfficialNav {
                nav: prev_nav,
                nav_date: String::new(), // 昨收日期未知，置空由调用方按需处理
                fund_type: String::new(),
            })
        } else {
            None
        }
    } else {
        None
    };
    Some((latest, prev))
}

/// 东财 F10 官方净值 + 上一交易日（主源）。对不收录的代码返回 None。
fn fetch_official_nav_eastmoney_with_prev(fund_code: &str) -> Option<(OfficialNav, Option<OfficialNav>)> {
    let url = format!(
        "https://api.fund.eastmoney.com/f10/lsjz?fundCode={}&pageIndex=1&pageSize=2",
        fund_code
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let data = v.get("Data")?;
    let list = data.get("LSJZList")?.as_array()?;
    if list.is_empty() {
        return None;
    }
    let parse = |item: &serde_json::Value| -> Option<OfficialNav> {
        let nav: f64 = item.get("DWJZ")?.as_str()?.parse().ok()?;
        let nav_date = item.get("FSRQ").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let fund_type = data.get("FundType").and_then(|x| x.as_str()).unwrap_or("").to_string();
        Some(OfficialNav { nav, nav_date, fund_type })
    };
    let latest = parse(list.first()?)?;
    let previous = list.get(1).and_then(parse);
    Some((latest, previous))
}

// ============ 历史净值（东财 F10 lsjz 历史净值接口）============
// 端点：api.fund.eastmoney.com/f10/lsjz（JSON，需 UA + Referer）
// 返回 LSJZList：FSRQ=净值日期(YYYY-MM-DD) / DWJZ=单位净值 / LJJZ=累计净值。
// 用于「基金净值走势图」：自动拉取 + 本地 nav_history 缓存（见 db.rs）。

/// 单个历史净值点（按日期升序排列后供图表使用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NavPoint {
    pub date: String, // FSRQ，YYYY-MM-DD
    pub nav: f64,     // DWJZ 单位净值
    pub acc_nav: f64, // LJJZ 累计净值（缺失记为 0）
}

/// 拉取基金历史净值（失败安全：超时/解析失败返回 None）。
/// `months` = 期望覆盖的月数（用于估算 pageSize）；传 0 表示全量（约 10000 条，覆盖数十年）。
/// 接口默认按日期降序返回，返回前会在 parse 内排序为升序。
pub fn fetch_nav_history(code: &str, months: u32) -> Option<Vec<NavPoint>> {
    let page_size = if months == 0 {
        10000
    } else {
        (months.saturating_mul(22).saturating_add(10)).min(10000)
    };
    let url = format!(
        "https://api.fund.eastmoney.com/f10/lsjz?fundCode={}&pageIndex=1&pageSize={}",
        code, page_size
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_nav_history(&body)
}

/// 将东财 lsjz 返回的 JSON 解析为升序历史净值序列（纯函数，便于离线单测）。
/// 跳过 DWJZ 为空（分红除息日等无净值）的记录；LJJZ 缺失记为 0。
pub fn parse_nav_history(body: &str) -> Option<Vec<NavPoint>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let data = v.get("Data")?;
    let list = data.get("LSJZList")?.as_array()?;
    let mut out: Vec<NavPoint> = Vec::new();
    for item in list {
        // FSRQ 可能为 "2026-08-13" 形式；不足 10 位视为无效
        let date = item.get("FSRQ").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if date.len() < 10 {
            continue;
        }
        // DWJZ 可能以字符串或数字形式出现；统一转字符串再解析
        let raw = match item.get("DWJZ") {
            Some(x) => x
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| x.as_f64().map(|n| n.to_string()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let nav: f64 = match raw.trim().parse::<f64>() {
            Ok(n) if n > 0.0 => n,
            _ => continue, // 无净值（如分红除息日）跳过
        };
        let acc_raw = match item.get("LJJZ") {
            Some(x) => x
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| x.as_f64().map(|n| n.to_string()))
                .unwrap_or_default(),
            None => String::new(),
        };
        let acc_nav: f64 = acc_raw.trim().parse::<f64>().unwrap_or(0.0);
        out.push(NavPoint { date, nav, acc_nav });
    }
    if out.is_empty() {
        return None;
    }
    // FSRQ 为 ISO 格式，字典序即时间序 → 升序排列便于图表从左到右时间递增
    out.sort_by(|a, b| a.date.cmp(&b.date));
    Some(out)
}

// ============ 盘中实时估值（天天基金 gsz 估算净值）============
// 端点：fundgz.tenorfun.com/js/{code}.js（JSONP，返回 jsonpgz({...})）
// 提供交易时段内每只开放式基金的「估算净值 gsz」与「单位净值 dwjz」，
// 由此可得盘中实时涨跌幅 = gsz/dwjz - 1。该值由平台直接计算，
// 覆盖面广（股票/混合/指数/ETF联接/债券等多数开放式基金），
// 优于仅依赖披露持仓的本地自算估值；故作为「优先来源」。

/// 盘中实时估值结果
#[derive(Debug, Clone)]
pub struct FundEstimate {
    pub est_nav: f64,        // 估算净值 gsz
    pub est_change_pct: f64, // 估算涨跌幅 = gsz/dwjz - 1
    pub prev_nav: f64,       // 单位净值 dwjz（上一交易日），用于计算当日实际收益 = gsz - dwjz
    pub gztime: String,      // 估值时间，如 "2026-08-15 14:30"
}

/// 解析天天基金 JSONP：jsonpgz({...}) → FundEstimate。纯函数，便于离线单测。
pub fn parse_fund_estimate(body: &str) -> Option<FundEstimate> {
    let s = body.trim();
    let s = s.strip_prefix("jsonpgz(")?;
    let s = s.strip_suffix(';').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s);
    let s = s.trim();
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let gsz: f64 = v.get("gsz")?.as_str()?.parse().ok()?;
    let dwjz: f64 = v.get("dwjz")?.as_str()?.parse().ok()?;
    if dwjz <= 0.0 {
        return None;
    }
    let est_change_pct = gsz / dwjz - 1.0;
    let gztime = v.get("gztime").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Some(FundEstimate {
        est_nav: gsz,
        est_change_pct,
        prev_nav: dwjz,
        gztime,
    })
}

/// 联网拉取单只基金盘中实时估值（失败安全：超时/解析失败返回 None）。
/// 注意：本函数【不再】逐个调用 throttle_wait()——批量拉取由 fetch_fund_estimates
/// 在批次边界统一节流一次，随后各基金并行发起；否则 20 只基金的串行 500ms 间隔会让
/// 一次刷新耗时 ~10s（即便线程并行也被全局 LAST_REQ 互斥锁串行化）。
pub fn fetch_fund_estimate(code: &str) -> Option<FundEstimate> {
    let url = format!("https://fundgz.tenorfun.com/js/{code}.js");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fundf10.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_fund_estimate(&body)
}

/// 并发拉取多只基金的盘中实时估值（线程池并行，单只 4s 超时），
/// 返回成功解析的子集。用于总览页一次性获取全部基金估值，避免串行阻塞。
///
/// 节流策略：仅在【批次边界】调用一次 throttle_wait()，随后各基金并行发起
/// （fundgz.tenorfun.com 为 JSONP CDN 端点，本就按基金粒度被页面并发加载，
/// 20 只并行属正常用量）。若沿用旧实现「每基金各节流 500ms」，则 20 只会被
/// 全局 LAST_REQ 互斥锁串行化到 ~10s，正是总览刷新卡顿的根因。
pub fn fetch_fund_estimates(codes: &[String]) -> HashMap<String, FundEstimate> {
    use std::thread;
    let mut out = HashMap::new();
    if codes.is_empty() {
        return out;
    }
    // 批次边界统一节流一次（与上一批任意出站请求间隔 ≥ MIN_REQ_INTERVAL），
    // 避免对 upstream 造成瞬时洪泛；并行发起本身不再各自节流。
    throttle_wait();
    let results: Vec<(String, Option<FundEstimate>)> = thread::scope(|s| {
        let handles: Vec<_> = codes
            .iter()
            .map(|c| {
                let code = c.clone();
                s.spawn(move || (code.clone(), fetch_fund_estimate(&code)))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (c, est) in results {
        if let Some(e) = est {
            out.insert(c, e);
        }
    }
    out
}

/// 估值是否为「当日新鲜」：gztime 以今日日期开头，说明平台当前仍在更新。
/// 非交易时段 gztime 停留在上一交易日，应判定为不新鲜、不采用。
pub fn estimate_is_fresh(gztime: &str) -> bool {
    if gztime.len() < 10 {
        return false;
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    gztime.starts_with(&today)
}

/// 是否适用「浮动净值 + 实时估算」口径（基于东财 FundType 码）。
///
/// 业界口径：凡净值随市场波动、平台提供盘中估算(g sz)的开放式基金，均按此口径计算
/// 市值 / 当日收益 / 当日估算收益 / 累计收益率等；仅「净值恒定≈1、收益按日计提」的
/// 现金管理类基金走「持有收益」专用路径。
///
/// 适用（有浮动净值，纳入估算）：
///   001 股票型 / 003 QDII / 004 债券型 / 006 分级 / 007 混合型 / 008 指数型 / 009 ETF联接
/// 不适用（净值恒定，仅持有收益）：
///   002 货币型 / 005 理财型
pub fn is_estimable_fund(fund_type: &str) -> bool {
    !matches!(fund_type, "002" | "005")
}

/// 是否 QDII 基金（海外投资，净值 T+1/T+2 确认；境外未收盘时不应把平台 gsz 当作「当日收益」）。
pub fn is_qdii_fund(fund_type: &str) -> bool {
    fund_type == "003"
}

/// QDII 跟踪的海外市场区域（用于判定境外交易时段）：us=美股 / hk=港股 / other=其他（默认按美股时区处理）。
fn qdii_overseas_region(fund_name: &str) -> &'static str {
    if fund_name.contains("香港")
        || fund_name.contains("恒生")
        || fund_name.contains("港股")
        || fund_name.contains("H股")
        || fund_name.contains("沪港")
    {
        "hk"
    } else if fund_name.contains("美国")
        || fund_name.contains("纳斯达克")
        || fund_name.contains("标普")
        || fund_name.contains("道琼")
        || fund_name.contains("环球")
        || fund_name.contains("全球")
        || fund_name.contains("海外")
        || fund_name.contains("美元")
        || fund_name.contains("纳指")
    {
        "us"
    } else {
        // 未明确标识的 QDII 默认按美股时区处理（多数 QDII 主投美股）
        "other"
    }
}

/// 近似判断当前是否处于美股夏令时：3 月第 2 个周日 ~ 11 月第 1 个周日。
/// 修复原实现中 11 月分支 `(m == 11 && d < 1)` 恒为 false 的 bug（day 不可能小于 1）。
fn is_us_daylight(now: &chrono::DateTime<chrono::Local>) -> bool {
    let m = now.month();
    let d = now.day();
    let y = now.year();
    if m > 3 && m < 11 {
        return true;
    }
    if m == 3 {
        let second_sunday = nth_weekday_of_month(y, 3, chrono::Weekday::Sun, 2);
        d >= second_sunday
    } else if m == 11 {
        let first_sunday = nth_weekday_of_month(y, 11, chrono::Weekday::Sun, 1);
        d < first_sunday
    } else {
        false
    }
}

/// 求某年某月第 n 个指定星期几的日期（1-based）。用于计算美股夏令时起始/结束日。
fn nth_weekday_of_month(year: i32, month: u32, weekday: chrono::Weekday, n: u32) -> u32 {
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(chrono::NaiveDate::MAX);
    let offset = (weekday.num_days_from_monday() + 7 - first.weekday().num_days_from_monday()) % 7;
    1 + offset + (n - 1) * 7
}

/// 当前北京时间下，该 QDII 基金所跟踪的海外市场是否处于交易时段。
/// - 港股：09:30–16:00（北京，无时令）
/// - 美股：夏令时 21:30–04:00、冬令时 22:30–05:00（北京，跨午夜）
/// 用于「境外未收盘时不报当日」的 T+1 建模：海外交易中则平台 gsz 仍在形成、非终值，应抑制「当日收益」。
pub fn qdii_overseas_open(fund_name: &str) -> bool {
    let now = chrono::Local::now();
    let t = now.hour() as i32 * 60 + now.minute() as i32; // 北京分钟数（0~1439）
    match qdii_overseas_region(fund_name) {
        "hk" => t >= 9 * 60 + 30 && t < 16 * 60,
        _ => {
            let edt = is_us_daylight(&now);
            let (open, close) = if edt { (21 * 60 + 30, 4 * 60) } else { (22 * 60 + 30, 5 * 60) };
            if open > close {
                // 跨午夜：如 21:30–04:00
                t >= open || t < close
            } else {
                t >= open && t < close
            }
        }
    }
}

pub fn fund_type_label(fund_type: &str) -> &'static str {
    match fund_type {
        "001" => "股票型",
        "002" => "货币型",
        "003" => "QDII",
        "004" => "债券型",
        "005" => "理财型",
        "006" => "分级",
        "007" => "混合型",
        "008" => "指数型",
        "009" => "ETF联接",
        _ => "未知",
    }
}

/// 资产大类映射（用于「资产配置全景」聚合）。
/// 权益=股票/混合/指数/ETF联接/分级；固收=债券/理财；货币；QDII；未知归 other。
pub fn asset_category(fund_type: &str) -> &'static str {
    match fund_type {
        "001" | "007" | "008" | "009" | "006" => "equity",
        "004" | "005" => "fixed",
        "002" => "money",
        "003" => "qdii",
        _ => "other",
    }
}

pub fn asset_category_label(category: &str) -> &'static str {
    match category {
        "equity" => "权益类",
        "fixed" => "固收类",
        "money" => "货币类",
        "qdii" => "QDII",
        _ => "其他",
    }
}

// ============ 基金名称 → 代码 解析（供 OCR 截图导入补全真实代码）============

/// 内置「基金名称 → 代码」别名表（常见基金，可按需扩展）。
/// 作为联网搜索的兜底：当联网搜索不可用/超时，仍能解析已知基金的真实代码，
/// 避免把基金名称当作代码入库。代码均经公开资料核实。
const FUND_NAME_ALIASES: &[(&str, &str)] = &[
    ("鹏华酒指数C", "012043"),
    ("鹏华酒C", "012043"),
    ("中欧医疗健康混合A", "003095"),
    ("博时恒生医疗保健ETF联接", "014424"),
    ("建信高端装备股票A", "011506"),
    ("天弘恒生科技ETF联接(QDII)C", "012349"),
    ("天弘恒生科技ETF联接C", "012349"),
    ("富国高质量混合", "012255"),
    ("易方达中小盘混合", "110011"),
    // —— 京东持仓（用户 2026-08-15 提供，已核实代码）——
    ("中欧红利优享灵活配置混合C", "004815"),
    ("华富永鑫灵活配置混合C", "001467"),
    ("银华中证全指证券公司ETF发起式", "025193"),
    ("建信上海金ETF联接C", "009034"),
    ("大成高鑫股票A", "000628"),
    ("南方中证A500ETF联接C", "022435"),
];

/// 清洗用于匹配的字符串：仅保留中日韩汉字与 ASCII 字母数字，去除标点/空格/括号。
/// 例如「天弘恒生科技 ETF联接（QDII)C」→「天弘恒生科技ETF联接QDIIC」
fn clean_for_match(s: &str) -> String {
    s.chars()
        .filter(|c| (0x4E00..=0x9FFF).contains(&(*c as u32)) || c.is_ascii_alphanumeric())
        .collect()
}

/// 名称相似度评分（越高越匹配，0 表示不匹配）
fn match_name_score(q: &str, cand: &str) -> usize {
    if q == cand {
        return 1000 + q.len();
    }
    // 子串包含（要求较长一侧 ≥6 字符，避免短名误命中）
    if q.len() >= 6 && cand.contains(q) {
        return 500 + q.len();
    }
    if cand.len() >= 6 && q.contains(cand) {
        return 400 + cand.len();
    }
    0
}

/// 简易 URL 编码（按字节 percent-encode），用于拼接搜索关键词。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

/// 解析东方财富基金搜索接口返回的 JSON，提取与查询最匹配的 6 位基金代码。
/// 失败安全：解析异常或无人选返回 None。可被离线单测直接调用。
fn parse_fund_search(q: &str, body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let datas = v.get("Datas")?.as_array()?;
    let mut best: Option<(String, usize)> = None;
    for d in datas {
        let cand_name = d.get("NAME").and_then(|x| x.as_str()).unwrap_or("");
        let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let score = match_name_score(q, &clean_for_match(cand_name));
        if score > 0
            && best
                .as_ref()
                .map_or(true, |(_, bs)| score > *bs)
        {
            best = Some((code.to_string(), score));
        }
    }
    // 兜底：评分无人选命中，但接口本身已按相关度排序，取首条（仍需是 6 位纯数字）。
    // 避免 OCR 名称轻微噪声时「宁可错填成名称也不联网」的情况。
    if best.is_none() {
        if let Some(d) = datas.first() {
            let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
            if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
                best = Some((code.to_string(), 0));
            }
        }
    }
    best.map(|(code, _)| code)
}

/// 联网按名称搜索基金代码（东方财富基金搜索接口）。失败安全：任何异常返回 None。
pub fn search_fund_code(name: &str) -> Option<String> {
    let q = clean_for_match(name);
    if q.is_empty() {
        return None;
    }
    // 注意：旧接口 fundapi.eastmoney.com/fundtradenew/search 已失效（返回 404），
    // 改用 fundsuggest.eastmoney.com 的 FundSearchAPI，返回结构一致（Datas/CODE/NAME）。
    let url = format!(
        "https://fundsuggest.eastmoney.com/FundSearch/api/FundSearchAPI.ashx?m=1&key={}",
        urlencode(name)
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    parse_fund_search(&q, &body)
}

/// 按基金名称解析真实 6 位代码：先查本地别名表（精确），再联网搜索兜底。
/// 解析不到返回 None（调用方保留原名作为主键）。
pub fn resolve_fund_code(name: &str) -> Option<String> {
    let q = clean_for_match(name);
    if q.is_empty() {
        return None;
    }
    for (alias, code) in FUND_NAME_ALIASES {
        if clean_for_match(alias) == q {
            return Some(code.to_string());
        }
    }
    search_fund_code(name)
}

/// 取基金类型（可靠来源：东方财富基金搜索接口的 `FundBaseInfo.FTYPE` 描述字段，
/// 例如「混合型-偏股」「指数型-海外股票」「股票型」）。
/// 重要：该接口的 `FUNDTYPE`/`RSFUNDTYPE` 数字码实为「销售适用类型」，
/// 对多数权益基金误报为 002（货币型），不可用于估值口径判定；
/// 故以描述性的 `FTYPE` 映射回我们的类型码。
/// 失败安全：任何异常或解析不到返回 None。
pub fn fetch_fund_type(fund_code: &str) -> Option<String> {
    if fund_code.len() != 6 || !fund_code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let url = format!(
        "https://fundsuggest.eastmoney.com/FundSearch/api/FundSearchAPI.ashx?m=1&key={}",
        urlencode(fund_code)
    );
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .ok()?;
    let resp = client
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .ok()?;
    let body = resp.text().ok()?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    let datas = v.get("Datas")?.as_array()?;
    for d in datas {
        let code = d.get("CODE").and_then(|x| x.as_str()).unwrap_or("");
        if code == fund_code {
            if let Some(ftype) = d
                .get("FundBaseInfo")
                .and_then(|b| b.get("FTYPE"))
                .and_then(|x| x.as_str())
            {
                return Some(fund_type_code_from_ftype(ftype).to_string());
            }
            // 香港互认基金（如 968072 摩根亚洲增长）：FundBaseInfo 缺失但 CATEGORYDESC 标注「香港基金」。
            // 其净值 T+1/T+2 确认、无本地自算估值，口径等同 QDII（003）。
            if let Some(cat) = d.get("CATEGORYDESC").and_then(|x| x.as_str()) {
                if cat.contains("香港") || cat.contains("QDII") {
                    return Some("003".to_string());
                }
            }
        }
    }
    None
}

/// 将东方财富 FTYPE 描述映射回我们的类型码（供 fund_type_label / asset_category / is_estimable_fund 使用）。
/// 顺序很重要：先判「联接/ETF联接」「指数」，再判「股票」，否则「指数型-股票」会被误判为股票型。
fn fund_type_code_from_ftype(ftype: &str) -> &'static str {
    if ftype.contains("ETF联接") || ftype.contains("联接") {
        "009"
    } else if ftype.contains("指数") {
        "008"
    } else if ftype.contains("股票") {
        "001"
    } else if ftype.contains("混合") {
        "007"
    } else if ftype.contains("债券") {
        "004"
    } else if ftype.contains("货币") {
        "002"
    } else if ftype.contains("QDII") {
        "003"
    } else {
        "000"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn trading_day_weekend_closed() {
        // 2024-02-17 周六 / 02-18 周日（春节假期覆盖周末）
        assert!(!is_trading_day(d(2024, 2, 17)));
        assert!(!is_trading_day(d(2024, 2, 18)));
    }

    #[test]
    fn trading_day_statutory_holiday_closed() {
        assert!(!is_trading_day(d(2024, 1, 1))); // 元旦
        assert!(!is_trading_day(d(2024, 2, 10))); // 春节
        assert!(!is_trading_day(d(2024, 4, 4))); // 清明
        assert!(!is_trading_day(d(2024, 5, 1))); // 劳动节
        assert!(!is_trading_day(d(2024, 6, 10))); // 端午
        assert!(!is_trading_day(d(2024, 9, 17))); // 中秋
        assert!(!is_trading_day(d(2024, 10, 1))); // 国庆
        assert!(!is_trading_day(d(2025, 10, 1))); // 国庆中秋合并
        assert!(!is_trading_day(d(2026, 2, 15))); // 春节
        assert!(!is_trading_day(d(2026, 10, 7))); // 国庆末日
    }

    #[test]
    fn trading_day_makeup_workday_still_closed() {
        // 调休补班日落在周末，本就不开市（即便 isOffDay=false）
        assert!(!is_trading_day(d(2024, 2, 4))); // 春节调休补班（周日）
        assert!(!is_trading_day(d(2024, 5, 11))); // 劳动节调休补班（周六）
        assert!(!is_trading_day(d(2026, 2, 14))); // 春节调休补班（周六）
    }

    #[test]
    fn trading_day_normal_weekday_open() {
        // 普通工作日（非假期）应为交易日
        assert!(is_trading_day(d(2024, 3, 1))); // 周五
        assert!(is_trading_day(d(2024, 2, 19))); // 春节后首个周一（非假期）
        assert!(is_trading_day(d(2025, 2, 5))); // 春节后周三
        assert!(is_trading_day(d(2026, 10, 9))); // 国庆后首个周五
    }

    #[test]
    fn trading_day_unknown_year_fallback_open() {
        // 2099 年无内置/远程数据 → 普通工作日兜底视为开市（避免误杀估值）
        assert!(is_trading_day(d(2099, 3, 2))); // 周二
        // 但周末仍关闭
        assert!(!is_trading_day(d(2099, 3, 7))); // 周日
    }

    #[test]
    fn clean_for_match_strips_punctuation_and_spaces() {
        assert_eq!(
            clean_for_match("天弘恒生科技 ETF联接（QDII)C"),
            "天弘恒生科技ETF联接QDIIC"
        );
        assert_eq!(clean_for_match("博时恒生医疗 保健ETF联接..."), "博时恒生医疗保健ETF联接");
    }

    #[test]
    fn resolve_known_fund_by_alias() {
        assert_eq!(resolve_fund_code("鹏华酒指数C"), Some("012043".to_string()));
        assert_eq!(resolve_fund_code("中欧医疗健康混合A"), Some("003095".to_string()));
        assert_eq!(resolve_fund_code("博时恒生医疗保健ETF联接"), Some("014424".to_string()));
        assert_eq!(resolve_fund_code("建信高端装备股票A"), Some("011506".to_string()));
        assert_eq!(resolve_fund_code("天弘恒生科技ETF联接(QDII)C"), Some("012349".to_string()));
        assert_eq!(resolve_fund_code("富国高质量混合"), Some("012255".to_string()));
    }

    #[test]
    fn match_name_score_prefers_exact() {
        assert!(match_name_score("中欧医疗健康混合A", "中欧医疗健康混合A") > 0);
        // 含括号差异的清洗后应命中
        assert!(match_name_score("天弘恒生科技ETF联接QDIIC", &clean_for_match("天弘恒生科技ETF联接(QDII)C")) > 0);
        // 短名不应误命中
        assert_eq!(match_name_score("酒", "鹏华酒指数C"), 0);
    }

    #[test]
    fn parse_fund_search_picks_exact_match() {
        // 模拟东方财富 fundsuggest 接口的真实返回结构
        let body = r#"{"ErrCode":0,"ErrMsg":"fromes","Datas":[{"CODE":"000628","NAME":"大成高鑫股票A"},{"CODE":"000001","NAME":"华夏成长混合"}]}"#;
        assert_eq!(parse_fund_search("大成高鑫股票A", body), Some("000628".to_string()));
    }

    #[test]
    fn parse_fund_search_fallback_to_top_when_no_score() {
        // 名称轻微噪声导致评分未命中，仍应取接口相关度排序的首条
        let body = r#"{"ErrCode":0,"Datas":[{"CODE":"022435","NAME":"南方中证A500ETF联接C"}]}"#;
        assert_eq!(parse_fund_search("南方中证A500ETF联C", body), Some("022435".to_string()));
    }

    #[test]
    fn parse_fund_search_rejects_non6_code() {
        let body = r#"{"ErrCode":0,"Datas":[{"CODE":"ABC","NAME":"某基金"},{"CODE":"123","NAME":"短码基金"}]}"#;
        assert_eq!(parse_fund_search("某基金", body), None);
    }

    #[test]
    fn parse_fund_estimate_reads_gsz_and_dwjz() {
        // 模拟天天基金 JSONP 真实返回
        let body = r#"jsonpgz({"fundcode":"000628","name":"大成高鑫股票A","jzrq":"2026-08-14","dwjz":"3.4520","gsz":"3.4680","gztime":"2026-08-15 14:30","sx":"1"})"#;
        let e = parse_fund_estimate(body).unwrap();
        assert_eq!(e.est_nav, 3.4680);
        assert_eq!(e.prev_nav, 3.4520);
        // 估算涨跌幅 = 3.4680/3.4520 - 1 ≈ 0.004636
        assert!((e.est_change_pct - (3.4680 / 3.4520 - 1.0)).abs() < 1e-9);
        assert_eq!(e.gztime, "2026-08-15 14:30");
    }

    #[test]
    fn parse_fund_estimate_handles_trailing_semicolon() {
        let body = "jsonpgz({\"dwjz\":\"1.0000\",\"gsz\":\"1.0200\",\"gztime\":\"2026-08-15 09:35\"});";
        let e = parse_fund_estimate(body).unwrap();
        assert!((e.est_change_pct - 0.02).abs() < 1e-9);
    }

    #[test]
    fn parse_fund_estimate_rejects_bad_input() {
        assert!(parse_fund_estimate("jsonpgz({})").is_none()); // 缺 gsz/dwjz
        assert!(parse_fund_estimate("not json").is_none());
        assert!(parse_fund_estimate("jsonpgz({\"dwjz\":\"0\",\"gsz\":\"1\"})").is_none()); // dwjz<=0
    }

    #[test]
    fn pick_benchmark_detects_index_from_name() {
        // 指数/ETF联接基金：名称含宽基关键字 → 真实跟踪指数，而非笼统沪深300
        let (_, code, name) = pick_benchmark("008", "华夏沪深300ETF联接A");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");

        let (_, code, name) = pick_benchmark("009", "南方中证500ETF联接C");
        assert_eq!(code, "000905");
        assert_eq!(name, "中证500");

        let (_, code, name) = pick_benchmark("008", "易方达创业板ETF");
        assert_eq!(code, "399006");
        assert_eq!(name, "创业板指");

        let (_, code, name) = pick_benchmark("008", "工银科创50ETF联接");
        assert_eq!(code, "000688");
        assert_eq!(name, "科创50");

        let (_, code, name) = pick_benchmark("008", "富国中证红利指数");
        assert_eq!(code, "000922");
        assert_eq!(name, "中证红利");
    }

    #[test]
    fn pick_benchmark_detects_industry_from_name() {
        // 行业/主题基金：名称含行业关键字 → 对应行业指数，而非笼统沪深300
        let (_, code, name) = pick_benchmark("007", "中欧医疗健康混合A");
        assert_eq!(code, "399989");
        assert_eq!(name, "中证医疗");

        let (_, code, name) = pick_benchmark("008", "鹏华酒指数C");
        assert_eq!(code, "399997");
        assert_eq!(name, "中证白酒");

        let (_, code, name) = pick_benchmark("008", "国泰中证证券公司ETF联接");
        assert_eq!(code, "399975");
        assert_eq!(name, "证券公司");

        let (_, code, _name) = pick_benchmark("007", "招商中证白酒指数");
        assert_eq!(code, "399997");

        let (_, code, name) = pick_benchmark("008", "国联安中证半导体ETF");
        assert_eq!(code, "980017");
        assert_eq!(name, "国证芯片");

        // 新能源车 优先于 新能源
        let (_, code, name) = pick_benchmark("008", "天弘中证新能源车ETF联接");
        assert_eq!(code, "399976");
        assert_eq!(name, "CS新能车");

        // 光伏 → 中证新能（与新能源车/新能源不冲突）
        let (_, code, name) = pick_benchmark("008", "天弘中证光伏产业ETF");
        assert_eq!(code, "399808");
        assert_eq!(name, "中证新能");
    }

    #[test]
    fn pick_benchmark_falls_back_by_type() {
        // 债券型 → 国债指数
        let (_, code, name) = pick_benchmark("004", "易方达增强回报债券A");
        assert_eq!(code, "000012");
        assert_eq!(name, "上证国债");

        // 名称无关键字 + 股票/混合/货币/QDII 等 → 沪深300 宽基代理
        let (_, code, name) = pick_benchmark("001", "易方达蓝筹精选混合");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");

        // 名称无行业/主题关键字的混合基金 → 仍回落沪深300 宽基代理
        let (_, code, _name) = pick_benchmark("007", "兴全合宜灵活配置混合");
        assert_eq!(code, "000300");

        // 名称无关键字的指数基金（如普通指数A）也落到沪深300 宽基代理，不做错误猜测
        let (_, code, name) = pick_benchmark("008", "某宽基指数A");
        assert_eq!(code, "000300");
        assert_eq!(name, "沪深300");
    }

    #[test]
    fn is_qdii_fund_matches_type_code_003() {
        assert!(is_qdii_fund("003"));
        assert!(!is_qdii_fund("001"));
        assert!(!is_qdii_fund("007"));
    }

    #[test]
    fn is_pure_index_fund_excludes_enhanced() {
        // 纯被动指数 / ETF 联接：纯指数头条路径
        assert!(is_pure_index_fund("008", "富国中证红利指数"));
        assert!(is_pure_index_fund("009", "易方达沪深300ETF联接"));
        assert!(is_pure_index_fund("006", "某分级母基金"));
        assert!(is_pure_index_fund("001", "招商中证白酒指数"));
        // 指数增强：排除纯指数头条，改走穿透口径
        assert!(is_index_fund("008", "富国中证红利指数增强"));
        assert!(!is_pure_index_fund("008", "富国中证红利指数增强"));
        assert!(!is_pure_index_fund("001", "某沪深300指数增强"));
        // 主动 / 债基：既非指数也非纯指数
        assert!(!is_pure_index_fund("007", "兴全合宜灵活配置混合"));
        assert!(!is_pure_index_fund("004", "鹏华丰禄债券"));
    }

    #[test]
    fn pick_benchmark_maps_hk_indices_to_sina_symbol() {
        // 恒生医疗保健：必须走港股新浪符号 hkHSHCI，而非误判为「中证医疗 399989」
        let (sym, code, cn) = pick_benchmark("", "博时恒生医疗保健ETF联接");
        assert_eq!(sym, "hkHSHCI");
        assert_eq!(code, "HSHCI");
        assert_eq!(cn, "恒生医疗保健指数");
        // 其它港股指数
        assert_eq!(pick_benchmark("", "华夏恒生科技ETF联接").0, "hkHSTECH");
        assert_eq!(pick_benchmark("", "某恒生指数ETF").0, "hkHSI");
        // A 股医疗基金仍走中证医疗（不受影响）
        let (a, _, _) = pick_benchmark("", "某中证医疗ETF");
        assert_eq!(a, "sz399989");
    }

    #[test]
    fn resolve_tracked_index_prefers_stored_over_name() {
        // 库里存了真实 track_index，优先用它（即便名称也能推断）
        let (sym, _, _) = resolve_tracked_index("009", "某ETF联接", "hkHSHCI").unwrap();
        assert_eq!(sym, "hkHSHCI");
        // 未存（空）：回退到按名称/类型推断
        let (sym2, _, _) = resolve_tracked_index("009", "某ETF联接", "").unwrap();
        assert!(!sym2.is_empty());
        // 主动基金：无指数代理，返回 None
        assert!(resolve_tracked_index("007", "兴全合宜灵活配置混合", "").is_none());
    }

    #[test]
    fn parse_sina_index_quote_parses_hshci_line() {
        // 来自新浪 hq.sinajs.cn 的真实港股指数行（2026/08/17 收盘后快照）
        let line = "var hq_str_hkHSHCI=\"HSHCI,恒生医疗保健指数,3722.1700,3712.1700,3794.1600,3674.4400,3738.7900,26.6200,0.7171,,,14976080.3260,0,,,,,2026/08/17,16:08\";";
        let q = parse_sina_index_quote(line, "hkHSHCI").expect("应解析成功");
        assert_eq!(q.stock_code, "HSHCI");
        assert_eq!(q.name, "恒生医疗保健指数");
        assert!((q.prev_close - 3712.17).abs() < 1e-6);
        assert!((q.price - 3738.79).abs() < 1e-6);
        // 涨跌 = 现价/昨收 - 1 ≈ +0.7171%
        assert!((q.price / q.prev_close - 1.0 - 0.007171).abs() < 1e-5);
        // 非 hk 符号 / 畸形行应返回 None
        assert!(parse_sina_index_quote("var hq_str_hkHSI=\"X\"", "hkHSHCI").is_none());
    }

    #[test]
    fn qdii_overseas_region_detects_us_and_hk() {
        // 港股区域
        assert_eq!(qdii_overseas_region("华夏恒生科技ETF联接(QDII)"), "hk");
        assert_eq!(qdii_overseas_region("易方达香港恒生H股"), "hk");
        // 美股区域
        assert_eq!(qdii_overseas_region("广发纳斯达克100指数(QDII)"), "us");
        assert_eq!(qdii_overseas_region("博时标普500ETF联接"), "us");
        assert_eq!(qdii_overseas_region("华夏全球科技先锋(QDII)"), "us");
        // 未标识 → other（按美股时区处理）
        assert_eq!(qdii_overseas_region("某QDII基金"), "other");
    }

    #[test]
    fn extract_curyear_reads_quarter_from_title() {
        // 中报（半年报）
        assert_eq!(extract_curyear("2026年2季度投资组合"), Some("2026Q2".to_string()));
        // 年报
        assert_eq!(extract_curyear("2025年4季度投资组合"), Some("2025Q4".to_string()));
        // 带「第」字
        assert_eq!(extract_curyear("2026年第2季度"), Some("2026Q2".to_string()));
        // 中文数字季度
        assert_eq!(extract_curyear("2026年第二季度投资组合"), Some("2026Q2".to_string()));
    }

    #[test]
    fn extract_curyear_avoids_date_collision() {
        // 正文日期「2026年08月」的月=0X 不是季度数字，不应误命中；
        // 真正的报告期（2026年2季度）在更靠后位置，应被正确提取。
        let body = "更新于2026年08月13日 09:30，基金2026年2季度投资组合如下";
        assert_eq!(extract_curyear(body), Some("2026Q2".to_string()));
        // 只有日期、无报告期标题时返回 None（而非错误的「2026Q0」）
        assert_eq!(extract_curyear("数据更新于2026年08月13日"), None);
    }

    #[test]
    fn extract_curyear_no_panic_on_multibyte_before_year() {
        // 回归：当「年」前 4 字节落在某个中文字符中间（如「某某年」）时，
        // 旧实现 body[abs-4..abs] 会触发 "start byte index is not a char boundary" panic
        // 并使整个 app 崩溃（abort）。新实现按字符索引，安全返回 None。
        assert_eq!(extract_curyear("某某年2季度投资组合"), None);
        // 正常报告期不受字符索引改动影响
        assert_eq!(
            extract_curyear("基金2026年2季度投资组合"),
            Some("2026Q2".to_string())
        );
    }

    #[test]
    fn candidate_periods_prefers_latest_ended_as_of_aug() {
        // 模拟「当前日期 = 2026-08-13」：最新已结束报告期 = 2026Q2（中报，6/30 结束）
        let now = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        let cands = candidate_periods(now);
        // 最新候选应为 2026Q2（full 全持仓）
        assert_eq!(cands.first(), Some(&(2026, 2, "full")));
        // 尚未结束的 2026Q3/Q4 不应出现
        assert!(!cands.contains(&(2026, 3, "top10")));
        assert!(!cands.contains(&(2026, 4, "full")));
        // 上一期 2026Q1 与 2025Q4 应作为回退候选存在
        assert!(cands.contains(&(2026, 1, "top10")));
        assert!(cands.contains(&(2025, 4, "full")));
        // 按从新到旧排序：2026Q2 应在 2026Q1 之前
        let pos_q2 = cands.iter().position(|c| *c == (2026, 2, "full")).unwrap();
        let pos_q1 = cands.iter().position(|c| *c == (2026, 1, "top10")).unwrap();
        assert!(pos_q2 < pos_q1);
    }

    #[test]
    fn parse_nav_history_reads_lsjz_list() {
        // 模拟东财 lsjz 真实返回：含一条 DWJZ 为空（分红除息日）应跳过
        let body = r#"{"Data":{"LSJZList":[{"FSRQ":"2026-08-11","DWJZ":"3.4520","LJJZ":"3.4520"},{"FSRQ":"2026-08-12","DWJZ":"3.4600","LJJZ":"3.4600"},{"FSRQ":"2026-08-13","DWJZ":"","LJJZ":""}],"FundType":"007"}}"#;
        let pts = parse_nav_history(body).unwrap();
        assert_eq!(pts.len(), 2); // 跳过空 DWJZ
        assert_eq!(pts[0].date, "2026-08-11");
        assert_eq!(pts[0].nav, 3.4520);
        assert_eq!(pts[1].date, "2026-08-12");
        assert!(pts[0].date < pts[1].date); // 升序
    }

    #[test]
    fn parse_nav_history_handles_null_and_numeric() {
        // LJJZ 为 null → 0；DWJZ 以数字而非字符串出现也应解析；DWJZ 为 null 的行应跳过
        let body = r#"{"Data":{"LSJZList":[{"FSRQ":"2026-08-10","DWJZ":"1.0000","LJJZ":null},{"FSRQ":"2026-08-09","DWJZ":2.5,"LJJZ":2.6},{"FSRQ":"2026-08-08","DWJZ":null,"LJJZ":"3.0"}]}}"#;
        let pts = parse_nav_history(body).unwrap();
        assert_eq!(pts.len(), 2); // 仅保留有 DWJZ 的两条
        assert_eq!(pts[0].date, "2026-08-09"); // 升序：08-09 在前
        assert_eq!(pts[0].nav, 2.5);
        assert_eq!(pts[0].acc_nav, 2.6);
        assert_eq!(pts[1].date, "2026-08-10");
        assert_eq!(pts[1].acc_nav, 0.0); // LJJZ null → 0
    }

    #[test]
    fn parse_nav_history_returns_none_on_garbage() {
        assert!(parse_nav_history("not json").is_none());
        assert!(parse_nav_history(r#"{"Data":{}}"#).is_none());
        assert!(parse_nav_history(r#"{"Data":{"LSJZList":[]}}"#).is_none());
    }

    #[test]
    fn candidate_periods_early_year_falls_back_to_prior_year_annual() {
        // 模拟「当前日期 = 2026-02-01」：2026Q1 尚未结束（3/31），最新应为 2025Q4（年报）
        let now = chrono::NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
        let cands = candidate_periods(now);
        assert_eq!(cands.first(), Some(&(2025, 4, "full")));
        assert!(!cands.contains(&(2026, 1, "top10"))); // 2026Q1 未结束，不应出现
    }

    #[test]
    fn us_daylight_time_boundaries() {
        // 2026 年：3 月第 2 个周日 = 3/8，11 月第 1 个周日 = 11/1
        let dt = |m, d| chrono::Local
            .from_local_datetime(
                &chrono::NaiveDate::from_ymd_opt(2026, m, d)
                    .unwrap()
                    .and_hms_opt(12, 0, 0)
                    .unwrap(),
            )
            .unwrap();
        assert!(is_us_daylight(&dt(3, 8))); // 夏令时开始当天
        assert!(!is_us_daylight(&dt(3, 7))); // 开始前一天
        assert!(is_us_daylight(&dt(7, 15))); // 夏季中间
        assert!(is_us_daylight(&dt(10, 31))); // 结束前一天
        assert!(!is_us_daylight(&dt(11, 1))); // 结束当天
    }

    #[test]
    fn parse_tencent_fund_nav_hk_mrf_real_payload() {
        // 实测 968072（摩根亚洲增长PRC人民币对冲累计，香港互认基金，东财 F10 不收录）
        let body = "v_jj968072=\"968072~摩根亚洲增长PRC人民币对冲累计~0.0000~0.0000~~17.7900~17.7900~-0.7808~2026-08-18~\";\n";
        let (nav, date, pct) = parse_tencent_fund_nav(body).expect("应解析成功");
        assert!((nav - 17.7900).abs() < 1e-9);
        assert_eq!(date, "2026-08-18");
        assert!((pct - (-0.7808)).abs() < 1e-9);
        // 昨收反推：nav / (1 + pct/100)
        let prev = nav / (1.0 + pct / 100.0);
        assert!((prev - 17.9300).abs() < 0.001, "反推昨收应约 17.9300，实际 {prev}");
    }

    #[test]
    fn parse_tencent_fund_nav_mainland_fund_matches_eastmoney() {
        // 实测 004139：腾讯 [5]=1.9496 [7]=-0.2252% [8]=2026-08-20
        // 东财 lsjz 官方：08-20=1.9496，08-19=1.9540 → 反推昨收应等于 1.9540
        let body = "v_jj004139=\"004139~某基金~0.0000~0.0000~~1.9496~1.9496~-0.2252~2026-08-20~\";\n";
        let (nav, date, pct) = parse_tencent_fund_nav(body).expect("应解析成功");
        assert!((nav - 1.9496).abs() < 1e-9);
        assert_eq!(date, "2026-08-20");
        let prev = nav / (1.0 + pct / 100.0);
        assert!((prev - 1.9540).abs() < 1e-6, "反推昨收应等于东财 08-19 官方值 1.9540，实际 {prev}");
    }

    #[test]
    fn parse_tencent_fund_nav_garbage_returns_none() {
        assert!(parse_tencent_fund_nav("").is_none());
        assert!(parse_tencent_fund_nav("v_jj968072=\"garbage\"").is_none());
        // 净值为 0（腾讯对无数据基金返回 0.0000）应判为失败
        let zero = "v_jj968072=\"968072~某基金~0.0000~0.0000~~0.0000~0.0000~0.0000~2026-08-18~\";\n";
        assert!(parse_tencent_fund_nav(zero).is_none());
    }

    /// 实时验证：东财 F10 不收录的香港互认基金（968072）通过腾讯兜底能取到真实净值。
    /// 网络依赖，默认忽略；手动运行：cargo test -- --ignored tencent_live_968072
    #[test]
    #[ignore]
    fn tencent_live_968072_fallback_gets_nav() {
        let nav = fetch_official_nav("968072");
        let nav = nav.expect("腾讯兜底应取到 968072 净值");
        assert!(nav.nav > 0.0, "净值应 > 0");
        assert!(!nav.nav_date.is_empty(), "应有净值日期");
        eprintln!("968072 fallback: nav={} date={}", nav.nav, nav.nav_date);

        // 昨收反推：with_prev 应给出 prev_nav。当日下跌时昨收应高于最新净值。
        let (latest, prev) = fetch_official_nav_with_prev("968072").expect("with_prev 应成功");
        assert!((latest.nav - nav.nav).abs() < 1e-9);
        let prev = prev.expect("应有反推昨收");
        assert!(prev.nav > 0.0 && prev.nav != latest.nav, "昨收应可反推且不等于最新净值");
        eprintln!("968072 prev_nav(反推)={}", prev.nav);
    }
}
