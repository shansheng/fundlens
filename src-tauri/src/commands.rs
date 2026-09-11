// Tauri 命令层 — 实现 SPEC.md 第 5 节约定的 11 条命令。
// 命令签名与前端 src/api.ts 的 invoke 调用保持一致。
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Timelike;

use tauri::Manager;

use crate::db;
use crate::data;
use crate::valuation::{self, PositionForSummary};

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FundMetaOut {
    code: String,
    name: String,
    platform: String,
    platform_name: String,
    shares: f64,
    cost_amount: f64,
    avg_cost: f64,
    official_nav: f64,
    /// 披露期：取自最新一条披露持仓记录（disclosures 表），无披露则为 None
    report_period: Option<String>,
    /// 披露口径：top10=前十大 / full=完整持仓，无披露则为空串
    disclosure_type: String,
    fund_type: String,
    fund_type_label: String,
    valuation_applicable: bool,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PositionRowOut {
    fund: FundMetaOut,
    est_nav: f64,
    est_change_pct: f64,
    market_value: f64,
    day_pnl: f64,
    day_pnl_pct: f64,
    day_pnl_est: f64,
    day_pnl_pct_est: f64,
    /// 当日实际收益（金额）：份额 ×(官方净值 − 昨收基准)；交易中/休市/QDII 延迟未确认为 0
    day_pnl_act: f64,
    /// 当日实际收益率（比率口径）
    day_pnl_pct_act: f64,
    /// 是否有「上一次净值」实际收益可用（非盘中、官方净值与昨收基准均有效）：
    /// true → 当日列用实际口径(day_pnl_act)，false → 用当日估算口径。不再要求 nav_date==今日，
    /// 故开盘前/周末/休盘（展示最近交易日确认净值）也为 true。
    has_day_actual: bool,
    /// 当日官方净值是否真的取到（发布日期==今日）：区分「当日实际」(true，盘后且当日净值已确认)
    /// 与「上一次净值」(false，开盘前/周末/休盘展示最近交易日确认净值)。仅用于当日列标签文案。
    day_is_today: bool,
    /// 官方净值日期（YYYY-MM-DD；空串=未取到），供前端「上次」标签显示具体净值日（透明化）
    nav_date: String,
    total_pnl: f64,
    total_pnl_pct: f64,
    estimated: bool,
    disclosure_type: String,
    disclosed_weight_sum: f64,
    /// 估值来源：realtime=盘中实时估值(平台) / local=本地自算 / none=无
    valuation_source: String,
    /// 交叉验证置信度：high/medium/low/none（穿透估值 vs 平台估值）
    confidence: String,
    /// 估值口径：index=指数实时估值优先（指数型基金）/ penetration=本地持仓穿透自算 / none=无
    valuation_method: Option<String>,
    /// QDII 延迟结算提示：如 "T+1·海外交易中" / "T+1·海外净值"；非 QDII 为 None
    delay_note: Option<String>,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OverviewOut {
    summary: valuation::PortfolioSummary,
    positions: Vec<PositionRowOut>,
    trading: bool,
    /// 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / prev_day=休市(上一交易日实际)
    market_session: String,
    as_of: String,
}

// ===================== v2.6.0 P-A：总览/明细短 TTL 快照缓存 =====================
// 进程内内存快照：同窗口（交易时段 10s / 盘前盘后休市 60s）重复 get_overview / get_fund_detail
// 不再全量重算（disclosures 扫描 + ~839 符号行情批抓取 + ~200 持仓逐只穿透重算），仅 clone 返回；
// single-flight（COMPUTE 锁）保证并发首 miss 只算一次，避免 ReportsPage 打开时 5~6 份行情风暴。
// 缓存不改变任何估值口径；记录快照(record_daily_snapshot)按日幂等副作用仍在 compute_* 内执行。
// 任何写 positions/transactions/funds/nav_history/snapshots/disclosures 的命令成功后调 invalidate_caches()。

/// 可测 seam：短 TTL + single-flight 快照缓存。
/// cache: 当前快照 (写入时刻, key, 值)；compute_lock: 计算互斥锁（防并发重复计算）。
/// key 相同且未过期 → clone 返回；未命中持锁后二次检查再算，算完写回。
fn cached_with<F, T>(
    cache: &Mutex<Option<(Instant, String, T)>>,
    compute_lock: &Mutex<()>,
    key: &str,
    ttl: Duration,
    compute: F,
) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
    T: Clone,
{
    // 1) 先查（不持计算锁）
    {
        // 中毒后取内部值继续，避免缓存失效导致全链路 panic
        let g = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((ts, k, v)) = g.as_ref() {
            if k == key && ts.elapsed() < ttl {
                return Ok(v.clone());
            }
        }
    }
    // 2) 未命中：持计算锁（single-flight，并发只放一个线程进去算）
    let _guard = compute_lock.lock().unwrap_or_else(|e| e.into_inner());
    // 3) 持锁二次检查：别的线程可能已在等待期间算完
    {
        let g = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((ts, k, v)) = g.as_ref() {
            if k == key && ts.elapsed() < ttl {
                return Ok(v.clone());
            }
        }
    }
    // 4) 真正计算
    let v = compute()?;
    // 5) 写回缓存
    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), key.to_string(), v.clone()));
    Ok(v)
}

static OVERVIEW_CACHE: Mutex<Option<(Instant, String, OverviewOut)>> = Mutex::new(None);
static OVERVIEW_COMPUTE: Mutex<()> = Mutex::new(());
static DETAIL_CACHE: Mutex<Option<(Instant, String, FundDetailOut)>> = Mutex::new(None);
static DETAIL_COMPUTE: Mutex<()> = Mutex::new(());

/// TTL：交易时段 10s，盘前/盘后/休市 60s（用 data::market_phase 区分）。
/// 午休 11:30–13:00 无行情跳动，10s 刷新属微效浪费 → 放宽到 60s。
fn snapshot_ttl() -> Duration {
    if data::market_phase() == "intraday" {
        // 午休窗口（本地时间 11:30–13:00）仍属盘中（market_phase 返回 intraday），
        // 但无行情跳动，放宽 TTL 至 60s 减少无效重算。
        let now_secs = chrono::Local::now().num_seconds_from_midnight();
        let lunch_start = 11 * 3600 + 30 * 60;
        let lunch_end = 13 * 3600;
        if now_secs >= lunch_start && now_secs < lunch_end {
            return Duration::from_secs(60);
        }
        Duration::from_secs(10)
    } else {
        Duration::from_secs(60)
    }
}

/// 总览缓存入口：key=None→"all"，否则平台 String。命中 clone 返回；未命中走 compute_overview。
fn cached_overview(platform: Option<String>) -> Result<OverviewOut, String> {
    let key = platform.clone().unwrap_or_else(|| "all".to_string());
    cached_with(
        &OVERVIEW_CACHE,
        &OVERVIEW_COMPUTE,
        &key,
        snapshot_ttl(),
        || compute_overview(platform),
    )
}

/// 明细缓存入口：key=基金代码。命中 clone 返回；未命中走 compute_fund_detail。
fn cached_fund_detail(code: String) -> Result<FundDetailOut, String> {
    let key = code.clone();
    cached_with(
        &DETAIL_CACHE,
        &DETAIL_COMPUTE,
        &key,
        snapshot_ttl(),
        || compute_fund_detail(code),
    )
}

/// 失效：写命令成功后调用，清空总览 + 明细快照（二者都依赖持仓/净值/披露等用户数据）。
/// 写即失效保证「写后读」立即看到新数据，避免缓存窗口内的陈旧快照。
/// 拆成两块失效函数并复用，使总览级函数成为真实调用点（移除原 dead_code 标注）。
fn invalidate_caches() {
    invalidate_overview_cache();
    invalidate_fund_detail_cache();
}

/// 仅失效总览快照（被 invalidate_caches 复用，亦供单测注入 fake compute 验证 invalidation 后重算）。
fn invalidate_overview_cache() {
    // 中毒后取内部值继续，避免缓存失效导致全链路 panic
    *OVERVIEW_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 仅失效明细快照（被 invalidate_caches 复用）。
fn invalidate_fund_detail_cache() {
    // 中毒后取内部值继续，避免缓存失效导致全链路 panic
    *DETAIL_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// 真实总览计算（重算路径）：disclosures 扫描 + 行情批抓取 + 逐持仓穿透估值 + 当日首查快照落盘。
/// 经 cached_overview 走短 TTL 快照缓存，避免高频刷新重复付网络 RTT；估值口径与 record_daily_snapshot
/// 副作用语义不变（按日幂等）。
fn compute_overview(platform: Option<String>) -> Result<OverviewOut, String> {
    // 加载全部持仓（单机单账户 = 本人），随后按平台过滤
    let mut holdings = db::list_holdings(None).map_err(|e| e.to_string())?;
    if let Some(p) = &platform {
        holdings.retain(|h| &h.platform == p);
    }
    let phase = data::market_phase();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    // 基准指数行情：按各基金「类型+名称」识别标的指数后，一次性批量拉取组合内所需的不同基准，
    // 供穿透估值近似未披露部分（指数/ETF联接用其真实跟踪指数，债券用国债指数，其余用沪深300）。
    let mut bench_syms: Vec<String> = Vec::new();
    for h in &holdings {
        let (sym, _, _) = data::pick_benchmark(&h.fund_type, &h.name);
        if !bench_syms.contains(&sym) {
            bench_syms.push(sym);
        }
    }
    // 仅本地自算估值：交易时段用本地持仓穿透估值（含基准近似）随行情跳动；
    // 不再调用平台实时估值接口（fundgz.tenorfun.com）——该接口在本机环境无法取到数据。

    let mut positions = Vec::new();
    let mut summary_input = Vec::new();

    // 性能优化：循环外一次性批量拉取所有基金的披露持仓与个股行情，
    // 避免对每只基金分别发起 DB 查询与网络请求（N 次网络往返是持仓总览卡顿主因）。
    let mut disclosures_by_fund: HashMap<String, Vec<valuation::DisclosedHolding>> = HashMap::new();
    for (fc, dh) in db::list_disclosures_batch().unwrap_or_default() {
        disclosures_by_fund.entry(fc).or_default().push(dh);
    }
    let mut all_stock_syms: Vec<String> = Vec::new();
    for hs in disclosures_by_fund.values() {
        for d in hs {
            let sym = to_quote_symbol(&d.stock_code);
            if !all_stock_syms.contains(&sym) {
                all_stock_syms.push(sym);
            }
        }
    }
    // 指数基准与个股行情分离抓取（修复 001551 bug 根因）：指数按完整符号（sh000933 等）建索引，
    // 个股按 digit 键（sz000933→000933）。若不分离，sh000933(中证医药) 与 sz000933(神火股份)
    // 共享末6位 000933，在按 digit 键的同一批 all_quotes 中后者覆盖前者，指数基金头条会用个股
    // 行情错误代理。个股行情批只含重仓股，供穿透估值使用。
    let all_quotes = data::fetch_quotes(&all_stock_syms).unwrap_or_default();
    // 指数行情单独成批：本批只含基准/跟踪指数符号（无个股），digit 键唯一，不会与同 digit 个股冲突。
    let index_all_quotes = data::fetch_quotes(&bench_syms).unwrap_or_default();

    let now_iso = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let now_ts = chrono::Local::now().timestamp();
    let mut est_items: Vec<(String, data::FundEstimate, i64)> = Vec::new();

    // 跟踪指数行情：A 股指数复用 index_all_quotes（按数字代码建索引；本批仅含指数符号，
    // digit 唯一不会与个股冲突），港股行业指数 gtimg 不覆盖，统一走新浪兜底
    // （fetch_hk_index_quotes 按完整符号建索引）。供指数基金头条「指数代理」使用。
    // 若某指数基金（如 014424 博时恒生医疗保健ETF联接）无任何披露持仓，它的跟踪指数行情
    // 只能从这里获得——否则指数代理无行情可代理，估算值消失。
    let mut index_quotes: HashMap<String, valuation::StockQuote> = HashMap::new();
    let mut hk_index_syms: Vec<String> = Vec::new();
    for sym in &bench_syms {
        if sym.starts_with("hk") {
            hk_index_syms.push(sym.clone());
        } else {
            let digit: String = sym.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Some(q) = index_all_quotes.get(&digit) {
                index_quotes.insert(sym.clone(), q.clone());
            }
        }
    }
    if !hk_index_syms.is_empty() {
        for (k, v) in data::fetch_hk_index_quotes(&hk_index_syms) {
            index_quotes.insert(k, v);
        }
    }

    for h in &holdings {
        // 由持仓视图构造与旧 FundRow/PositionRow 兼容的结构，复用既有估值逻辑
        let f = db::FundRow {
            code: h.code.clone(),
            name: h.name.clone(),
            platform: h.platform.clone(),
            official_nav: h.official_nav,
            report_period: h.report_period.clone(),
            disclosure_type: h.disclosure_type.clone(),
            fund_type: h.fund_type.clone(),
            track_index: String::new(),
            valuation_applicable: h.valuation_applicable,
        };
        let pos = db::PositionRow {
            fund_code: h.code.clone(),
            shares: h.shares,
            cost_amount: h.cost_amount,
            holding_amount: h.holding_amount,
            holding_profit: h.holding_profit,
            yesterday_profit: h.yesterday_profit,
            profit_rate: h.profit_rate,
        };
        let disclosures = disclosures_by_fund.get(&f.code).cloned().unwrap_or_default();
        let quotes = fetch_quotes_for_batch(&disclosures, &all_quotes);
        let is_index = data::is_index_fund(&f.fund_type, &f.name);
        let pure_index = data::is_pure_index_fund(&f.fund_type, &f.name);
        let v_local = if f.valuation_applicable {
            // bench_sym 既用于穿透基准，也作为指数代理符号；港股（hk 前缀）在 index_quotes 中按完整符号建索引。
            let (bench_sym, _, _) = data::pick_benchmark(&f.fund_type, &f.name);
            let bench = index_quotes.get(&bench_sym).cloned();
            // 指数基金头条采用真实跟踪指数：优先库里 track_index，否则同 pick_benchmark（hk/沪深300 等符号）。
            let tracked_index = if is_index {
                let (tsym, _, _) = data::resolve_tracked_index(&f.fund_type, &f.name, &f.track_index)
                    .unwrap_or_else(|| (bench_sym.clone(), String::new(), String::new()));
                index_quotes.get(&tsym).cloned()
            } else {
                None
            };
            valuation::value_fund(valuation::ValuationInput {
                fund_code: f.code.clone(),
                official_nav: f.official_nav,
                holdings: disclosures.clone(),
                quotes,
                benchmark: bench.clone(),
                // 仅被动指数型基金才传入跟踪指数行情（主动基金 tracked_index 置空，避免误当作指数基金）
                tracked_index,
                pure_index,
            })
        } else {
            valuation::FundValuationResult {
                fund_code: f.code.clone(),
                official_nav: f.official_nav,
                est_nav: f.official_nav,
                est_change_pct: 0.0,
                disclosure_type: "none".into(),
                disclosed_weight_sum: 0.0,
                holdings: vec![],
                estimated: false,
                reason: Some(format!(
                    "模型不适用（{}）：货币型/理财型（002/005）净值恒定≈1，不纳入本地自算估值",
                    data::fund_type_label(&f.fund_type)
                )),
                benchmark_code: None,
                benchmark_name: None,
                benchmark_return: 0.0,
                benchmark_weight: 0.0,
                platform_est_change_pct: None,
                confidence: "none".into(),
                divergence: 0.0,
                penetration_est_change_pct: None,
                consensus_est_change_pct: None,
                valuation_method: None,
            }
        };
        // 统一走本地持仓穿透自算估值（平台实时估值接口已停用）：
        // est_nav = 官方净值 ×(1 + Σ披露占比ᵢ×个股涨跌ᵢ + 未披露占比×跟踪指数涨跌)，随行情跳动。
        let is_qdii = data::is_qdii_fund(&f.fund_type);
        let qdii_suppress = is_qdii && data::qdii_overseas_open(&f.name);
        let mut delay_note: Option<String> = None;
        if is_qdii {
            delay_note = Some(if qdii_suppress {
                "T+1·海外交易中".to_string()
            } else {
                "T+1·海外净值".to_string()
            });
        }
        let valuation_source = if v_local.estimated {
            "local".to_string()
        } else {
            "none".to_string()
        };
        // 克隆而非移动：v_local 已在上方构造，本地自算直接复用；无平台实时收益分支。
        let mut v = v_local.clone();
        // 本地自算单一来源：不再与平台实时估值做交叉验证（该接口已停用）。
        let platform_pct: Option<f64> = None;
        let (conf, div, consensus) = compute_confidence(v_local.est_change_pct, platform_pct);
        v.platform_est_change_pct = platform_pct;
        // 穿透源涨跌幅由估值引擎统一产出（指数基金=追踪指数参考、主动基金=头条），
        // 不再在此覆盖，避免指数基金头条被错误地改写为穿透口径。
        v.confidence = conf;
        v.divergence = div;
        v.consensus_est_change_pct = consensus;
        // 即便头条采用平台实时估值，也把穿透估值的基准信息带出，供前端展示口径
        if v.benchmark_code.is_none() {
            v.benchmark_code = v_local.benchmark_code.clone();
            v.benchmark_name = v_local.benchmark_name.clone();
            v.benchmark_return = v_local.benchmark_return;
            v.benchmark_weight = v_local.benchmark_weight;
        }
        // 份额自算：支付宝风格截图导入（shares=0、holding_amount>0）按「当前官方净值」把持仓金额
        // 折算成份额，使持仓进入实时自算路径（盘中估值与当日估算收益随行情跳动），累计盈亏也以
        // 「持仓金额−持有收益」(导入成本) 为基础变活。新导入已在 import_positions_batch 落库时折算，
        // 此处兜底历史/净值缺失数据；官方净值缺失时 eff_shares=0，退回 holding_amount 直接展示。
        let eff_shares = if pos.shares > 0.0 {
            pos.shares
        } else if pos.holding_amount > 0.0 && f.official_nav > 0.0 {
            pos.holding_amount / f.official_nav
        } else {
            0.0
        };
        let eff_cost = if pos.shares > 0.0 {
            pos.cost_amount
        } else if pos.holding_amount > 0.0 {
            (pos.holding_amount - pos.holding_profit).max(0.0)
        } else {
            0.0
        };
        // 仅当「有真实 6 位代码 且 折算后份额>0」才走本地自算估值；份额始终为 0 的兜底持仓用金额/收益直接展示。
        let has_real_code = f.code.len() == 6
            && f.code.chars().all(|c| c.is_ascii_digit())
            && eff_shares > 0.0;
        let avg_cost = if eff_shares > 0.0 { eff_cost / eff_shares } else { 0.0 };

        // 标准「业界口径」指标：用估值引擎统一计算市值 / 累计盈亏 / 当日估算 / 当日实际（含比率）。
        // 基准净值 = 上一交易日收盘净值；当日收益 = 份额 ×(参考净值 − 基准净值)。
        let is_money_or_wealth = matches!(f.fund_type.as_str(), "002" | "005");

        // 昨收基准优先级（P0 单源化）：永远优先 funds.prev_nav（官方接口显式昨收，权威）；
        // 缺失时回退 prev_nav_from_history（nav_history 前一交易日净值）；最后才为 0。
        // 不再回退 est_cache.prev_nav（已被删除，其 dwjz 来源会污染基准，见 006503 案例）。
        let baseline_prev = if !is_money_or_wealth {
            if h.prev_nav > 0.0 {
                h.prev_nav
            } else {
                db::prev_nav_from_history_code(&f.code, &h.nav_date)
                    .filter(|p| *p > 0.0)
                    .unwrap_or(0.0)
            }
        } else {
            0.0
        };

        // 货币/理财型：净值恒定≈1，不做日波动估算，仅展示累计持有收益。
        // 口径统一（P0）：与明细页一致，采用可审计的「市值 = 份额 × 官方净值，累计盈亏 = 市值 − 成本基数」。
        // 官方净值缺失或份额为 0 时退回导入的持仓金额兜底（避免显示 0 市值）。
        // 兼容性：净值不变时 `mv − eff_cost` 恒等于导入的 holding_profit
        // （因 eff_cost = holding_amount − holding_profit），故本口径是原写法的严格推广，历史数值不变；
        // 净值变动或份额型货基时新口径能正确反映盈亏，原写法会冻结为导入时的静态值。
        let m = if is_money_or_wealth {
            let mv = if eff_shares > 0.0 && f.official_nav > 0.0 {
                eff_shares * f.official_nav
            } else {
                pos.holding_amount
            };
            let tpnl = if mv > 0.0 { mv - eff_cost } else { 0.0 };
            valuation::PositionMetrics {
                market_value: mv,
                baseline_nav: f.official_nav,
                prev_close_market_value: mv,
                total_pnl: tpnl,
                total_pnl_pct: if eff_cost > 1e-9 { tpnl / eff_cost } else { 0.0 },
                day_pnl_est: 0.0,
                day_pnl_act: 0.0,
                day_pnl_pct_est: 0.0,
                day_pnl_pct_act: 0.0,
                anchored_est_nav: f.official_nav,
            }
        } else if has_real_code {
            let mut m = valuation::compute_position_metrics(&valuation::PositionMetricsInput {
                shares: eff_shares,
                cost_amount: eff_cost,
                est_nav: v.est_nav,
                official_nav: f.official_nav,
                prev_nav: baseline_prev,
                // 传 DB 真实官方净值日期 h.nav_date（而非 today）：compute_position_metrics 内
                // 当日实际收益 day_pnl_act 始终为真实官方口径=份额×(official_nav−prev_nav)，不做 stale 回填；
                // 市值口径 = 官方净值确为今日真值（nav_date==today）时份额×official_nav，否则重锚定估算。
                // 「实际/上次」标签由 commands.rs 的 day_is_today=h.nav_date==today 控制。
                nav_date: &h.nav_date,
                phase,
                today: &today,
            });
            // QDII 海外交易中：平台 gsz 非终值，当日估算不展示（前端显示「—」）。
            // 同时把估值列回退为官方净值、涨跌归 0——境外交易时段不应展示盘中估算（P2-12）。
            if qdii_suppress {
                m.day_pnl_est = 0.0;
                m.day_pnl_pct_est = 0.0;
                v.est_nav = f.official_nav;
                v.est_change_pct = 0.0;
            }
            // 维护 est_cache：仅刷新盘中估算字段（est_nav/est_change_pct/gztime）。落库 est_nav 必须是
            // 「按昨收基准重锚定后的 anchored_est_nav」而非原始 est_nav（消除 official_nav 漂移）。
            // P0：prev_nav 不再落库（基准唯一来源为 funds.prev_nav / nav_history 派生）。
            est_items.push((
                f.code.clone(),
                data::FundEstimate {
                    est_nav: m.anchored_est_nav,
                    est_change_pct: v.est_change_pct,
                    prev_nav: 0.0,
                    gztime: now_iso.clone(),
                },
                now_ts,
            ));
            m
        } else {
            // 无份额/无代码的兜底持仓：用金额/收益直接展示，无当日波动
            let mv = pos.holding_amount;
            let tpnl = pos.holding_profit;
            valuation::PositionMetrics {
                market_value: mv,
                baseline_nav: f.official_nav,
                prev_close_market_value: mv,
                total_pnl: tpnl,
                // 收益率分母统一为「成本基数」（= 持仓金额 − 持有收益），与货基/完整指标分支及明细页一致。
                // 原用市值作分母（收益/市值 并非收益率），与同层级其它分支口径不一致，此处修正。
                total_pnl_pct: if eff_cost > 1e-9 { tpnl / eff_cost } else { 0.0 },
                day_pnl_est: 0.0,
                day_pnl_act: 0.0,
                day_pnl_pct_est: 0.0,
                day_pnl_pct_act: 0.0,
                anchored_est_nav: f.official_nav,
            }
        };

        // 是否有「上一次净值」真实官方口径可用：官方净值有效、且真实昨收基准有效 →
        // 当日列用真实官方口径（今日实际 / 上日实际，由 nav_date==today 区分标签）。
        // 不再要求 nav_date==今日（否则标「上次」），也不再要求非盘中——盘中无今日实际时同样展示
        // 「上日实际」（真实官方），与「当日估算收益」列（盘中实时浮动估算）严格区分、互不替代。
        //
        // 判定条件必须与 day_pnl_act 的非零条件「完全同源」——即 compute_position_metrics 内
        // `official_nav > 0 && prev_nav > 0`，其中 prev_nav 传入的就是 baseline_prev。
        // 此前此处只看 funds.prev_nav（h.prev_nav），而基准允许回退 nav_history 派生：
        // 当 funds.prev_nav 缺失、靠历史兜底成功时，day_pnl_act 已算出真实值，却因本判定为 false
        // 被前端标「估算」并丢弃实际值（标签与数值判定不同源）。改用 baseline_prev 后二者严格一致。
        let has_day_actual = !is_money_or_wealth
            && has_real_code
            && f.official_nav > 0.0
            && baseline_prev > 0.0;
        // 当日官方净值是否真的取到（nav_date==今日）：区分「当日实际」与「上一次净值」标签。
        let day_is_today = h.nav_date == today;

        summary_input.push(PositionForSummary {
            fund_code: f.code.clone(),
            market_value: m.market_value,
            prev_close_market_value: m.prev_close_market_value,
            cost_amount: eff_cost,
            total_pnl: m.total_pnl,
            total_pnl_pct: m.total_pnl_pct,
            day_pnl_est: m.day_pnl_est,
            day_pnl_act: m.day_pnl_act,
            day_pnl_pct_est: m.day_pnl_pct_est,
            day_pnl_pct_act: m.day_pnl_pct_act,
            estimated: v.estimated && !is_money_or_wealth,
        });

        positions.push(PositionRowOut {
            fund: FundMetaOut {
                code: f.code.clone(),
                name: f.name.clone(),
                platform: f.platform.clone(),
                platform_name: platform_name(&f.platform),
                shares: pos.shares,
                cost_amount: pos.cost_amount,
                avg_cost,
                official_nav: f.official_nav,
                report_period: f.report_period.clone(),
                disclosure_type: f.disclosure_type.clone().unwrap_or_default(),
                fund_type: f.fund_type.clone(),
                fund_type_label: data::fund_type_label(&f.fund_type).to_string(),
                valuation_applicable: f.valuation_applicable,
            },
            est_nav: v.est_nav,
            est_change_pct: v.est_change_pct,
            market_value: m.market_value,
            // 「当日收益」始终为真实官方口径（今日实际 / 上日实际），绝不回填估算；
            // 盘中实时浮动估算只在独立的「当日估算收益」字段(day_pnl_est)展示。
            day_pnl: m.day_pnl_act,
            day_pnl_pct: m.day_pnl_pct_act,
            day_pnl_est: m.day_pnl_est,
            day_pnl_pct_est: m.day_pnl_pct_est,
            day_pnl_act: m.day_pnl_act,
            day_pnl_pct_act: m.day_pnl_pct_act,
            has_day_actual,
            day_is_today,
            nav_date: h.nav_date.clone(),
            total_pnl: m.total_pnl,
            total_pnl_pct: m.total_pnl_pct,
            estimated: v.estimated && !is_money_or_wealth,
            disclosure_type: v.disclosure_type.clone(),
            disclosed_weight_sum: v.disclosed_weight_sum,
            valuation_source,
            confidence: v.confidence.clone(),
            valuation_method: v.valuation_method.clone(),
            delay_note: delay_note.clone(),
        });
    }

    // 批量写回估值缓存（事务），供下一自然日作为「昨收」基准。
    let _ = db::save_est_cache(&est_items);

    let mut summary = valuation::summarize_portfolio(&summary_input, phase);
    positions.sort_by(|a, b| b.market_value.partial_cmp(&a.market_value).unwrap_or(std::cmp::Ordering::Equal));
    // 市场时段三态：intraday=交易中(当日预估) / post_close=盘后(当日实际) / closed=休市(上一交易日实际)
    let market_session = phase.to_string();
    // 进阶风险指标：基于各基金 nav_history 按「当前份额恒定」近似聚合组合净值序列。
    // 历史净值来自本地缓存（db::get_nav_history），无网络；数据不足时 summary.risk 保持 None。
    // 使用累计净值 acc_nav 计算，避免分红除息日单位净值除息造成虚假回撤/波动。
    let mut nav_series: Vec<valuation::FundNavSeries> = Vec::new();
    for h in &holdings {
        if let Ok(navs) = db::get_nav_history(&h.code) {
            if navs.len() >= 2 {
                let shares = if h.shares > 0.0 {
                    h.shares
                } else if h.holding_amount > 0.0 && h.official_nav > 0.0 {
                    h.holding_amount / h.official_nav
                } else {
                    0.0
                };
                if shares > 0.0 {
                    nav_series.push(valuation::FundNavSeries {
                        shares,
                        navs: navs
                            .into_iter()
                            .map(|n| {
                                // 优先累计净值（复权），缺失时退化为单位净值
                                let v = if n.acc_nav > 0.0 { n.acc_nav } else { n.nav };
                                (n.date, v)
                            })
                            .collect(),
                    });
                }
            }
        }
    }
    if let Some(risk) = valuation::compute_portfolio_risk(&nav_series) {
        summary.risk = Some(risk);
    }
    // 自动落库：当日首查即记录组合日快照（幂等 upsert），日报/周报/月报/年报/盈亏日历的历史从启用起累积。
    // 单机单账户，快照始终以 scope=0（全账户聚合）存储，历史不丢；平台筛选仅影响展示层。
    // 估算市值投影：盘中（实际当日收益未实现=0）市值已是估算口径，直接取 total_market_value；
    // 盘后/休市则用「实际市值 − 实际当日收益 + 估算当日收益」把当日收益替换为估算口径。
    let est_mv = if phase == "intraday" && summary.act_day_pnl.abs() < f64::EPSILON {
        summary.total_market_value
    } else {
        summary.total_market_value - summary.act_day_pnl + summary.est_day_pnl
    };
    record_daily_snapshot(
        0,
        summary.total_market_value,
        summary.total_cost,
        summary.total_pnl,
        summary.act_day_pnl,
        summary.est_day_pnl,
        est_mv,
    );

    Ok(OverviewOut {
        summary,
        positions,
        trading: phase == "intraday",
        market_session,
        as_of: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

#[tauri::command]
pub fn get_overview(platform: Option<String>) -> Result<OverviewOut, String> {
    // 走短 TTL 快照缓存：命中直接 clone 返回，未命中经 single-flight 重算一次后写回。
    cached_overview(platform)
}

/// 记录某账户（scope=0 表示全部账户聚合）当日的组合市值快照。
/// 当日盈亏 = 市值变动 − 当日净现金流（入金−出金），避免充值/取现被误算为收益。
/// 同时落库当日估算收益（day_pnl_est）与估算市值（est_mv），供各周期报告的估算统计使用。
fn record_daily_snapshot(
    scope: i64,
    total_mv: f64,
    total_cost: f64,
    total_pnl: f64,
    act_day_pnl: f64,
    day_pnl_est: f64,
    est_mv: f64,
) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let prev = db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT snapshot_date, total_market_value FROM snapshots
             WHERE account_id = ?1 AND snapshot_date < ?2
             ORDER BY snapshot_date DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![scope, today.clone()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        })?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    })
    .ok()
    .flatten();
    // 定向 SQL 聚合当日净现金流，避免全表扫描所有交易记录
    let net_cash = db::sum_cash_flow_on(&today).unwrap_or(0.0);
    // P1-5：仅当上一快照是「上一个 A 股交易日」时，市值链差才是真正的单日盈亏；
    // 若中间隔了多日（用户多日未打开 App），链差是「多日累计变动」，记成当日会制造巨额单日假象。
    // 断档时改用「当日真实官方实际收益」（act_day_pnl = Σ 份额×(official_nav−prev_nav)，由
    // 总览统一估值口径实时计算）作为当日盈亏；首次快照无前驱同样走该口径，不再误报累计/负现金流。
    let contiguous = match &prev {
        Some((d, _)) => valuation::trading_days_between(d, &today) == 1,
        None => false,
    };
    let day_pnl = if contiguous {
        total_mv - prev.as_ref().map(|(_, mv)| mv).copied().unwrap_or(total_mv) - net_cash
    } else {
        act_day_pnl
    };
    let _ = db::record_snapshot(
        scope,
        &today,
        "",
        total_mv,
        total_cost,
        total_pnl,
        day_pnl,
        0.0,
        0.0,
        day_pnl_est,
        est_mv,
    );
}

/// 单只基金「我的持仓」业界标准指标（与总览页 PositionRowOut 同口径，由 valuation::compute_position_metrics 计算）。
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FundPositionOut {
    /// 当前份额
    shares: f64,
    /// 单位成本（avg_cost）
    avg_cost: f64,
    /// 持仓成本（累计投入成本基数）
    cost_amount: f64,
    /// 市值（intraday=估算口径市值 / 其余=官方净值口径市值）
    market_value: f64,
    /// 累计盈亏 = 市值 − 持仓成本
    total_pnl: f64,
    /// 累计收益率 = 累计盈亏 / 持仓成本
    total_pnl_pct: f64,
    /// 当日收益（头条口径：交易中=估算，否则=实际）
    day_pnl: f64,
    /// 当日收益率（头条口径）
    day_pnl_pct: f64,
    /// 当日估算收益（盘中浮动估算，随行情跳动）
    day_pnl_est: f64,
    /// 当日估算收益率
    day_pnl_pct_est: f64,
    /// 当日官方净值是否真的取到（nav_date==今日）：true→「当日收益」标「实际」，false→标「上日实际」
    day_is_today: bool,
    /// 是否纳入浮动净值估算（货基/理财=false，仅展示累计持有收益）
    estimated: bool,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FundDetailOut {
    fund: FundMetaOut,
    valuation: valuation::FundValuationResult,
    quotes: Vec<QuoteView>,
    /// 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / closed=休市(上一交易日实际)
    market_session: String,
    /// 估值来源：local=本地穿透自算 / none=无估值（平台实时估值接口已停用）
    valuation_source: String,
    /// QDII 延迟结算提示：如 "T+1·海外交易中" / "T+1·海外净值"；非 QDII 为 None
    delay_note: Option<String>,
    /// 该基金的交易流水（买卖/分红/手动），供基金明细页「交易记录」区块展示
    transactions: Vec<TransactionOut>,
    /// 该基金「我的持仓」业界标准指标（市值/成本/累计盈亏/当日收益等）
    position: FundPositionOut,
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuoteView {
    stock_code: String,
    stock_name: String,
    price: f64,
    prev_close: f64,
    price_return: f64,
}

/// 真实明细计算（重算路径），经 cached_fund_detail 走短 TTL 缓存；口径与总览同窗口一致。
fn compute_fund_detail(code: String) -> Result<FundDetailOut, String> {
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let f = funds.into_iter().find(|x| x.code == code).ok_or("基金不存在")?;
    let disclosures = db::list_disclosures(&code).unwrap_or_default();
    let quotes = fetch_quotes_for(&disclosures);
    let (bench_sym, bench_code, _) = data::pick_benchmark(&f.fund_type, &f.name);
    // 港股行业指数（hk 前缀）腾讯 gtimg 不覆盖，走新浪兜底；A 股指数仍走 gtimg。
    let benchmark_quote = if bench_sym.starts_with("hk") {
        data::fetch_hk_index_quotes(&[bench_sym.clone()])
            .remove(&bench_sym)
            .or_else(|| data::fetch_quotes(&[bench_sym.clone()]).and_then(|mut m| m.remove(&bench_code)))
    } else {
        data::fetch_quotes(&[bench_sym.clone()]).and_then(|mut m| m.remove(&bench_code))
    };
    // P2-5：与总览页一致，指数基金优先使用 funds.track_index 作为真实跟踪指数行情，
    // 而不是把通用基准 benchmark_quote 误当成跟踪指数（会导致无披露持仓的港股指数基金估值为 0）。
    let is_index = data::is_index_fund(&f.fund_type, &f.name);
    let pure_index = data::is_pure_index_fund(&f.fund_type, &f.name);
    let tracked_index_quote = if is_index {
        let (tsym, _, _) = data::resolve_tracked_index(&f.fund_type, &f.name, &f.track_index)
            .unwrap_or_else(|| (bench_sym.clone(), String::new(), String::new()));
        if tsym.starts_with("hk") {
            data::fetch_hk_index_quotes(&[tsym.clone()]).remove(&tsym)
        } else {
            let digit: String = tsym.chars().filter(|c| c.is_ascii_digit()).collect();
            data::fetch_quotes(&[tsym.clone()]).and_then(|mut m| m.remove(&digit))
        }
    } else {
        None
    };
    // 时间戳用于 est_cache 写入，与总览页统一
    let now_iso = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    let now_ts = chrono::Local::now().timestamp();
    let mut v = if f.valuation_applicable {
        valuation::value_fund(valuation::ValuationInput {
            fund_code: code.clone(),
            official_nav: f.official_nav,
            holdings: disclosures.clone(),
            quotes: quotes.clone(),
            benchmark: benchmark_quote.clone(),
            // 仅被动指数型基金传入真实跟踪指数行情（主动基金置空，避免误当作指数基金）
            tracked_index: tracked_index_quote.clone(),
            pure_index,
        })
    } else {
        valuation::FundValuationResult {
            fund_code: code.clone(),
            official_nav: f.official_nav,
            est_nav: f.official_nav,
            est_change_pct: 0.0,
            disclosure_type: "none".into(),
            disclosed_weight_sum: 0.0,
            holdings: vec![],
            estimated: false,
            reason: Some(format!(
                "模型不适用（{}）：债基/货基/QDII 不纳入本地自算估值",
                data::fund_type_label(&f.fund_type)
            )),
            benchmark_code: None,
            benchmark_name: None,
            benchmark_return: 0.0,
            benchmark_weight: 0.0,
            platform_est_change_pct: None,
            confidence: "none".into(),
            divergence: 0.0,
            penetration_est_change_pct: None,
            consensus_est_change_pct: None,
            valuation_method: None,
        }
    };
    let quote_views = disclosures
        .iter()
        .filter_map(|h| {
            let q = quotes.get(&h.stock_code)?;
            let price_return = if q.prev_close > 0.0 { q.price / q.prev_close - 1.0 } else { 0.0 };
            Some(QuoteView {
                stock_code: q.stock_code.clone(),
                stock_name: h.stock_name.clone(),
                price: q.price,
                prev_close: q.prev_close,
                price_return,
            })
        })
        .collect();

    // 取该基金的持仓快照（单机单账户，直接按 code 在全部持仓中查找）
    let pos_holding = db::list_holdings(None)
        .ok()
        .and_then(|hs| hs.into_iter().find(|h| h.code == code));
    // 提前取出上一交易日净值 prev_nav / 官方净值日期 nav_date（match 会消耗 pos_holding，须先借出）。
    let (ph_prev_nav, ph_nav_date) = pos_holding
        .as_ref()
        .map(|h| (h.prev_nav, h.nav_date.clone()))
        .unwrap_or((0.0, String::new()));
    let pos = match pos_holding {
        Some(h) => db::PositionRow {
            fund_code: h.code.clone(),
            shares: h.shares,
            cost_amount: h.cost_amount,
            holding_amount: h.holding_amount,
            holding_profit: h.holding_profit,
            yesterday_profit: h.yesterday_profit,
            profit_rate: h.profit_rate,
        },
        None => db::PositionRow {
            fund_code: code.clone(),
            shares: 0.0,
            cost_amount: 0.0,
            holding_amount: 0.0,
            holding_profit: 0.0,
            yesterday_profit: 0.0,
            profit_rate: 0.0,
        },
    };
    // 份额自算：与 get_overview 一致，对支付宝风格(snapshots shares=0)按当前官方净值折算份额，
    // 使「我的持仓」展示真实份额与成本（新导入已在落库时折算，此处兜底历史数据）。
    let eff_shares = if pos.shares > 0.0 {
        pos.shares
    } else if pos.holding_amount > 0.0 && f.official_nav > 0.0 {
        pos.holding_amount / f.official_nav
    } else {
        0.0
    };
    let eff_cost = if pos.shares > 0.0 {
        pos.cost_amount
    } else if pos.holding_amount > 0.0 {
        (pos.holding_amount - pos.holding_profit).max(0.0)
    } else {
        0.0
    };
    let avg_cost = if eff_shares > 0.0 { eff_cost / eff_shares } else { 0.0 };
    // 与 get_overview 完全同口径的两个门禁（P0 统一）：
    // - is_money_or_wealth：货基/理财走「无日波动」独立分支；
    // - has_real_code：必须「6 位真实数字代码 且 折算后份额>0」才走完整浮动净值指标；
    //   否则（占位/无真实代码/份额为 0 的兜底持仓）退回「金额 + 导入收益」直接展示，
    //   避免在无净值可依时算出 0 值（此前明细页无此门禁，与总览页分叉）。
    let is_money_or_wealth = matches!(f.fund_type.as_str(), "002" | "005");
    let has_real_code =
        code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) && eff_shares > 0.0;
    // 业界标准持仓指标：与总览页同一套 compute_position_metrics 中央函数，保证明细页与总览页数字一致。
    let phase = data::market_phase();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    // 昨收基准优先级（P0 单源化）：优先 funds.prev_nav（ph_prev_nav），缺失时回退 nav_history 派生；
    // 不再回退 est_cache.prev_nav（已删除）。
    let prev_nav = if ph_prev_nav > 0.0 {
        ph_prev_nav
    } else {
        db::prev_nav_from_history_code(&code, &ph_nav_date)
            .filter(|p| *p > 0.0)
            .unwrap_or(0.0)
    };
    // 传 DB 真实官方净值日期，供判定 official_nav 是否为今日真值：nav_date==today → 今日实际，否则上日实际。
    let nav_date = ph_nav_date.clone();
    let detail_day_is_today = ph_nav_date == today;
    // 计算持仓指标，同时带出 anchored_est_nav 用于 est_cache 回写，保持详情页与总览页基准一致。
    // 三分支与 get_overview 完全对齐（P0 统一口径）：
    //   ① 货基/理财 → 无日波动分支；② 有真实 6 位代码且份额>0 → 完整浮动净值指标；③ 其余 → 金额兜底。
    let (position, anchored_est_nav) = if is_money_or_wealth {
        // 货基/理财：净值恒定≈1，不做日波动估算，仅展示累计持有收益。
        // 口径与总览页一致：市值 = 份额 × 官方净值（可审计、随净值变动），累计盈亏 = 市值 − 成本基数；
        // 净值缺失或份额为 0 时退回导入的持仓金额兜底，避免显示 0 市值。
        let mv = if eff_shares > 0.0 && f.official_nav > 0.0 {
            eff_shares * f.official_nav
        } else {
            pos.holding_amount
        };
        let tpnl = if mv > 0.0 { mv - eff_cost } else { 0.0 };
        (
            FundPositionOut {
                shares: eff_shares,
                avg_cost,
                cost_amount: eff_cost,
                market_value: mv,
                total_pnl: tpnl,
                total_pnl_pct: if eff_cost > 1e-9 { tpnl / eff_cost } else { 0.0 },
                day_pnl: 0.0,
                day_pnl_pct: 0.0,
                day_pnl_est: 0.0,
                day_pnl_pct_est: 0.0,
                day_is_today: detail_day_is_today,
                estimated: false,
            },
            f.official_nav,
        )
    } else if has_real_code {
        let m = valuation::compute_position_metrics(&valuation::PositionMetricsInput {
            shares: eff_shares,
            cost_amount: eff_cost,
            est_nav: v.est_nav,
            official_nav: f.official_nav,
            prev_nav,
            nav_date: &nav_date,
            phase,
            today: &today,
        });
        // QDII 海外交易中：抑制当日估算展示（与总览页一致），下方估算为上一海外交易日变动。
        let qdii_suppress = v.estimated
            && data::is_qdii_fund(&f.fund_type)
            && data::qdii_overseas_open(&f.name);
        let (day_pnl_est, day_pnl_pct_est) = if qdii_suppress {
            (0.0, 0.0)
        } else {
            (m.day_pnl_est, m.day_pnl_pct_est)
        };
        (
            FundPositionOut {
                shares: eff_shares,
                avg_cost,
                cost_amount: eff_cost,
                market_value: m.market_value,
                total_pnl: m.total_pnl,
                total_pnl_pct: m.total_pnl_pct,
                // 「当日收益」始终为真实官方口径（今日实际 / 上日实际），绝不回填估算；
                // 盘中实时浮动估算只在独立的「当日估算收益」字段(day_pnl_est)展示。
                day_pnl: m.day_pnl_act,
                day_pnl_pct: m.day_pnl_pct_act,
                day_pnl_est,
                day_pnl_pct_est,
                day_is_today: detail_day_is_today,
                estimated: v.estimated,
            },
            m.anchored_est_nav,
        )
    } else {
        // 兜底持仓（无真实 6 位代码 / 折算后份额为 0）：无净值可依，直接用导入的
        // 持仓金额与持有收益展示，不参与浮动净值估算（与总览页兜底分支完全一致）。
        let mv = pos.holding_amount;
        let tpnl = pos.holding_profit;
        (
            FundPositionOut {
                shares: eff_shares,
                avg_cost,
                cost_amount: eff_cost,
                market_value: mv,
                total_pnl: tpnl,
                // 收益率分母统一为「成本基数」（= 持仓金额 − 持有收益），与货基/完整指标分支一致。
                // 总览页兜底分支原用市值作分母（收益/市值 ≠ 收益率），已在总览页同步修正。
                total_pnl_pct: if eff_cost > 1e-9 { tpnl / eff_cost } else { 0.0 },
                day_pnl: 0.0,
                day_pnl_pct: 0.0,
                day_pnl_est: 0.0,
                day_pnl_pct_est: 0.0,
                day_is_today: detail_day_is_today,
                estimated: false,
            },
            f.official_nav,
        )
    };

    // P0：回写 est_cache 仅刷新盘中估算字段（est_nav/est_change_pct/gztime）；prev_nav 不再落库
    // （基准唯一来源为 funds.prev_nav / nav_history 派生）。保持明细页与总览页估算口径一致。
    let est_item = [(
        code.clone(),
        data::FundEstimate {
            est_nav: anchored_est_nav,
            est_change_pct: v.est_change_pct,
            prev_nav: 0.0,
            gztime: now_iso,
        },
        now_ts,
    )];
    let _ = db::save_est_cache(&est_item);
    // 本地自算单一来源：不再与平台实时估值做交叉验证（该接口已停用）。
    let platform_pct: Option<f64> = None;
    let (conf, div, consensus) = compute_confidence(v.est_change_pct, platform_pct);
    v.platform_est_change_pct = platform_pct;
    // 穿透源涨跌幅由估值引擎统一产出（指数基金=追踪指数参考、主动基金=头条），此处不再覆盖。
    v.confidence = conf;
    v.divergence = div;
    v.consensus_est_change_pct = consensus;
    // QDII：净值 T+1/T+2 确认，标注延迟结算提示（境外交易中 / 海外净值）
    let delay_note = if data::is_qdii_fund(&f.fund_type) {
        if data::qdii_overseas_open(&f.name) {
            Some("T+1·海外交易中".to_string())
        } else {
            Some("T+1·海外净值".to_string())
        }
    } else {
        None
    };
    // QDII 海外交易中：与总览页一致，估值列回退官方净值、涨跌归 0（P2-12），
    // 避免把境外未收盘的 T+1 盘中估算当作当日估值展示。
    if v.estimated && data::is_qdii_fund(&f.fund_type) && data::qdii_overseas_open(&f.name) {
        v.est_nav = f.official_nav;
        v.est_change_pct = 0.0;
    }
    // 该基金的交易流水（买卖/分红/手动），按交易日期倒序；附基金名称便于展示
    let txn_rows = db::list_transactions(Some(1), Some(code.clone())).map_err(|e| e.to_string())?;
    let all_funds = db::list_funds().map_err(|e| e.to_string())?;
    let name_map: HashMap<String, String> =
        all_funds.into_iter().map(|f| (f.code, f.name)).collect();
    let transactions: Vec<TransactionOut> = txn_rows
        .into_iter()
        .map(|t| TransactionOut {
            id: t.id,
            account_id: t.account_id,
            txn_type: t.txn_type,
            fund_code: t.fund_code.clone(),
            fund_name: t.fund_code.as_ref().and_then(|c| name_map.get(c).cloned()),
            shares: t.shares,
            amount: t.amount,
            price: t.price,
            txn_date: t.txn_date,
            txn_time: t.txn_time,
            note: t.note,
            source: t.source,
            source_ref: t.source_ref,
        })
        .collect();
    // 估值来源：本地穿透自算(local)；不适用则 none（仅货币型/理财型 002/005 估值模型不适用）。
    let valuation_source: String = if v.estimated { "local".into() } else { "none".into() };
    Ok(FundDetailOut {
        fund: FundMetaOut {
            code: f.code,
            name: f.name,
            platform: f.platform.clone(),
            platform_name: platform_name(&f.platform),
            shares: eff_shares,
            cost_amount: eff_cost,
            avg_cost,
            official_nav: f.official_nav,
            // 披露期/口径从已拉取的披露持仓派生（disclosures 表），而非 funds 表的空列
            report_period: disclosures.first().map(|d| d.report_period.clone()),
            disclosure_type: disclosures
                .first()
                .map(|d| d.disclosure_type.clone())
                .unwrap_or_default(),
            fund_type: f.fund_type.clone(),
            fund_type_label: data::fund_type_label(&f.fund_type).to_string(),
            valuation_applicable: f.valuation_applicable,
        },
        valuation: v,
        quotes: quote_views,
        market_session: phase.to_string(),
        valuation_source,
        delay_note,
        transactions,
        position,
    })
}

#[tauri::command]
pub fn get_fund_detail(code: String) -> Result<FundDetailOut, String> {
    // 走短 TTL 快照缓存：命中直接 clone 返回，未命中重算一次；口径与总览同窗口一致。
    cached_fund_detail(code)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsOut {
    summary: valuation::PortfolioSummary,
    best: Option<PositionRowOut>,
    worst: Option<PositionRowOut>,
    by_platform: Vec<PlatformAgg>,
    estimated_coverage: f64,
    asset_allocation: Vec<AssetSlice>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetSlice {
    category: String,
    label: String,
    market_value: f64,
    pct: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformAgg {
    platform: String,
    platform_name: String,
    market_value: f64,
    total_pnl: f64,
}

#[tauri::command]
pub fn get_stats(platform: Option<String>) -> Result<StatsOut, String> {
    let ov = get_overview(platform)?;
    let mut by_platform: HashMap<String, PlatformAgg> = HashMap::new();
    let mut asset_map: HashMap<String, f64> = HashMap::new();
    let mut total_mv = 0.0f64;
    let mut est_count = 0usize;
    for p in &ov.positions {
        if p.estimated {
            est_count += 1;
        }
        let agg = by_platform.entry(p.fund.platform.clone()).or_insert(PlatformAgg {
            platform: p.fund.platform.clone(),
            platform_name: p.fund.platform_name.clone(),
            market_value: 0.0,
            total_pnl: 0.0,
        });
        agg.market_value += p.market_value;
        agg.total_pnl += p.total_pnl;
        // 资产配置全景：按 fund_type 归并大类市值
        let cat = data::asset_category(&p.fund.fund_type).to_string();
        *asset_map.entry(cat).or_insert(0.0) += p.market_value;
        total_mv += p.market_value;
    }
    let mut asset_allocation: Vec<AssetSlice> = asset_map
        .into_iter()
        .map(|(category, mv)| AssetSlice {
            category: category.clone(),
            label: data::asset_category_label(&category).to_string(),
            market_value: mv,
            pct: if total_mv > 0.0 { mv / total_mv } else { 0.0 },
        })
        .collect();
    asset_allocation.sort_by(|a, b| b.market_value.partial_cmp(&a.market_value).unwrap_or(std::cmp::Ordering::Equal));
    let mut sorted: Vec<&PositionRowOut> = ov.positions.iter().collect();
    sorted.sort_by(|a, b| b.total_pnl_pct.partial_cmp(&a.total_pnl_pct).unwrap_or(std::cmp::Ordering::Equal));
    Ok(StatsOut {
        summary: ov.summary,
        best: sorted.first().cloned().cloned(),
        worst: sorted.last().cloned().cloned(),
        by_platform: by_platform.into_values().collect(),
        estimated_coverage: if ov.positions.is_empty() {
            0.0
        } else {
            est_count as f64 / ov.positions.len() as f64
        },
        asset_allocation,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreviewOut {
    platform: String,
    platform_name: String,
    detected_count: usize,
    funds: Vec<ImportFundOut>,
    ocr_ready: bool,
    note: String,
    raw_lines: Vec<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFundOut {
    code: String,
    name: String,
    shares: f64,
    nav: f64,
    /// 持仓金额（支付宝风格）
    holding_amount: f64,
    /// 持有收益（支付宝风格）
    holding_profit: f64,
    /// 昨日收益（支付宝风格）
    yesterday_profit: f64,
    /// 收益率（支付宝风格，百分数）
    profit_rate: f64,
}

/// 读取本地图片并以 base64 data URL 返回，供前端 <img> 直接预览。
/// 由后端（受信任）读取文件，规避 Tauri 2 asset 协议对作用域的限制，
/// 无需为图片预览额外申请 fs / asset 权限。
#[tauri::command]
pub fn read_image_data_url(path: String) -> Result<String, String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("读取图片失败: {e}"))?;
    let ext = path
        .rsplit('.')
        .next()
        .unwrap_or("png")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    };
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

#[tauri::command]
pub fn import_screenshots(
    app: tauri::AppHandle,
    platform: String,
    file_paths: Vec<String>,
) -> Result<ImportPreviewOut, String> {
    // v1.1：本地 PaddleOCR 识别每张截图 -> 文本行 -> 平台模板抽取基金持仓。
    // ⚠️ 多图必须「逐张图独立抽取」再合并：每张图的 OCR 行 y 坐标各自从 0 起，
    // 若把所有图的行拼成一条平铺列表再按 y 聚类，图2 的 y=200 行会和图1 的 y=200
    // 行聚成同一卡片、数字跨图错配，导致识别全乱。逐图抽取可保证坐标空间互不干扰。
    let mut per_image_funds: Vec<Vec<crate::ocr::OcrFund>> = Vec::new();
    let mut raw_text: Vec<String> = Vec::new();
    let mut ocr_ready = true;
    for path in &file_paths {
        match crate::ocr::recognize_image(path, Some(&app)) {
            Ok(lines) => {
                for l in &lines {
                    raw_text.push(l.text.clone());
                }
                // 每张图独立抽取（坐标空间隔离），避免跨图混行
                per_image_funds.push(crate::ocr::extract_fund_rows(&platform, &lines));
            }
            Err(e) => {
                ocr_ready = false;
                eprintln!("[FundLens] OCR 失败 {}: {}", path, e);
            }
        }
    }

    let mut funds: Vec<crate::ocr::OcrFund> = if ocr_ready {
        per_image_funds.into_iter().flatten().collect()
    } else {
        Vec::new()
    };

    // 补全真实基金代码：支付宝/京东等无代码平台，OCR 会把名称当代码入库。
    // 这里按名称解析真实 6 位代码（本地别名兜底 + 联网搜索），解析不到则保留原名。
    // 用缓存避免同一名称重复发起联网请求（每张截图仅每个唯一名称查一次）。
    let mut code_cache: HashMap<String, Option<String>> = HashMap::new();
    for f in funds.iter_mut() {
        let is_real = f.code.len() == 6 && f.code.chars().all(|c| c.is_ascii_digit());
        if !is_real {
            let name = f.name.clone();
            let resolved = code_cache
                .entry(name.clone())
                .or_insert_with(|| data::resolve_fund_code(&name));
            if let Some(real) = resolved.clone() {
                f.code = real;
            } else {
                // 解析不到真实代码，跳过净值/类型补全（保留原名作主键）
                continue;
            }
        }
        // 真实代码（含刚解析出的）：补官方净值 + 可靠基金类型，使类型标签可显示、估值口径正确。
        // 类型用 fundsuggest 的 FTYPE（描述准确），而非 lsjz 的 FUNDTYPE 数字码（多数误报为货币型）。
        if let Some(nav) = data::fetch_official_nav(&f.code) {
            let ftype = data::fetch_fund_type(&f.code).unwrap_or_default();
            let _ = db::update_fund_nav(
                &f.code,
                nav.nav,
                &ftype,
                data::is_estimable_fund(&ftype),
                &nav.nav_date,
                None,
            );
            // P1-2：截图导入份额反推必须使用截图上打印的净值（与持仓金额同口径），
            // 不能无条件覆盖为 fetch_official_nav 返回的（可能陈旧的）官方净值，否则份额会系统性偏差。
            // 仅在 OCR 未识别出净值（f.nav <= 0）时才用官方净值兜底。
            if f.nav <= 0.0 {
                f.nav = nav.nav;
            }
        }
    }

    // 识别到的基金按账户批量写入本地库（替换该账户 import 基线 + 统一重算持仓）
    let mut import_items: Vec<db::ImportHolding> = Vec::new();
    for f in &funds {
        import_items.push(db::ImportHolding {
            code: f.code.clone(),
            name: f.name.clone(),
            platform: platform.clone(),
            nav: f.nav,
            shares: f.shares,
            holding_amount: f.holding_amount,
            holding_profit: f.holding_profit,
            yesterday_profit: f.yesterday_profit,
            profit_rate: f.profit_rate,
        });
    }
    // 单机单账户固定 account_id = 1（与 seed_demo_data / 默认账户一致）
    let persisted = if db::import_positions_batch(1, &import_items).is_ok() {
        import_items.len()
    } else {
        0
    };

    let import_funds = funds
        .iter()
        .map(|f| ImportFundOut {
            code: f.code.clone(),
            name: f.name.clone(),
            shares: f.shares,
            nav: f.nav,
            holding_amount: f.holding_amount,
            holding_profit: f.holding_profit,
            yesterday_profit: f.yesterday_profit,
            profit_rate: f.profit_rate,
        })
        .collect::<Vec<_>>();

    let note = if ocr_ready {
        if funds.is_empty() && !raw_text.is_empty() {
            format!(
                "OCR 已识别 {} 行文本，但未匹配到基金持仓。可在下方「OCR 原始文本」查看，并把内容发我以微调列定位。",
                raw_text.len()
            )
        } else {
            format!(
                "本地 PaddleOCR 识别完成：识别 {} 条持仓，已写入 {} 只基金（可在总览页查看，随后刷新行情/披露即可估值）。",
                funds.len(),
                persisted
            )
        }
    } else {
        "OCR 引擎未就绪：请先运行 src-tauri/download_ocr_models.sh 下载 PP-OCRv4 模型，并以 --features ocr 构建（npm run tauri build --features ocr）。".into()
    };

    invalidate_caches();
    Ok(ImportPreviewOut {
        platform: platform.clone(),
        platform_name: platform_name(&platform),
        detected_count: funds.len(),
        funds: import_funds,
        ocr_ready,
        note,
        raw_lines: raw_text,
    })
}

/// 应用缓存目录（Android 上 std::env::temp_dir() 无 TMPDIR 时回落 /tmp 不可写；统一用
/// app.path().app_cache_dir()——Android=内部 cacheDir、桌面=平台缓存，均恒可写）。调用方负责用完删除。
fn app_cache_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let p = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("无法定位应用缓存目录: {e}"))?;
    let _ = std::fs::create_dir_all(&p);
    Ok(p)
}

/// base64 图片列表 → 应用缓存目录临时文件（供「内容传参」命令落地后走既有 path 管线，用完即删）。
fn write_temp_images(
    app: &tauri::AppHandle,
    images_b64: &[String],
    prefix: &str,
) -> Result<Vec<std::path::PathBuf>, String> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let base = app_cache_dir(app)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut paths = Vec::with_capacity(images_b64.len());
    for (i, b) in images_b64.iter().enumerate() {
        let bytes = engine
            .decode(b)
            .map_err(|e| format!("图片数据解码失败: {e}"))?;
        let p = base.join(format!("{prefix}_{nanos}_{i}.img"));
        std::fs::write(&p, &bytes).map_err(|e| format!("写入临时图片失败: {e}"))?;
        paths.push(p);
    }
    Ok(paths)
}

/// 持仓截图 OCR 导入——**内存字节版**（M2-P0「内容传参」）。Android/移动端 tauri-plugin-dialog
/// 返回 content:// URI，std::fs 无法直读；前端改用 <input type=file> 读字节→base64 传参，
/// 后端 decode 落应用 cache 临时文件后与桌面路径版走完全相同的识别管线（零逻辑分叉）。
#[tauri::command]
pub fn import_screenshots_b64(
    app: tauri::AppHandle,
    platform: String,
    images_b64: Vec<String>,
) -> Result<ImportPreviewOut, String> {
    let paths = write_temp_images(&app, &images_b64, "fl_ocr_fund")?;
    let r = import_screenshots(
        app,
        platform,
        paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    );
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    r
}

/// 单条交易记录 OCR 预览项（可编辑后落地为真实流水）
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTxnOut {
    pub txn_type: String,
    pub txn_type_raw: String,
    pub date: String,
    pub has_year: bool,
    pub time: String,
    pub code: String,
    pub name: String,
    pub shares: f64,
    pub amount: f64,
    pub price: f64,
    pub confidence: f64,
}

/// 交易记录截图 OCR 预览（可编辑后由前端调用 import_transactions 落地）
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTxnPreviewOut {
    pub platform: String,
    pub platform_name: String,
    pub detected_count: usize,
    pub txns: Vec<ImportTxnOut>,
    pub ocr_ready: bool,
    pub note: String,
    pub raw_lines: Vec<String>,
}

/// 识别交易记录截图（买/卖/分红）：OCR -> 几何抽取交易流水 -> 按名称补真实基金代码
/// -> 返回**可编辑**的预览。注意：本命令**不落库**，由前端核对后调用 import_transactions 写入真实流水。
#[tauri::command]
pub fn import_txn_screenshots(
    app: tauri::AppHandle,
    platform: String,
    file_paths: Vec<String>,
) -> Result<ImportTxnPreviewOut, String> {
    // 与 import_screenshots 同理：多图必须逐张独立抽取再合并，避免各图 y 坐标
    // 拼接后跨图混行（交易块切分/纵向合并同样依赖 y 坐标）。
    let mut per_image_txns: Vec<Vec<crate::ocr::OcrTxn>> = Vec::new();
    let mut raw_text: Vec<String> = Vec::new();
    let mut ocr_ready = true;
    for path in &file_paths {
        match crate::ocr::recognize_image(path, Some(&app)) {
            Ok(lines) => {
                for l in &lines {
                    raw_text.push(l.text.clone());
                }
                per_image_txns.push(crate::ocr::extract_txn_rows(&platform, &lines));
            }
            Err(e) => {
                ocr_ready = false;
                eprintln!("[FundLens] 交易截图 OCR 失败 {}: {}", path, e);
            }
        }
    }

    let mut txns: Vec<crate::ocr::OcrTxn> = if ocr_ready {
        per_image_txns.into_iter().flatten().collect()
    } else {
        Vec::new()
    };

    // 补全真实基金代码：无代码平台（支付宝等）按名称解析 6 位代码
    let mut code_cache: HashMap<String, Option<String>> = HashMap::new();
    for t in txns.iter_mut() {
        let is_real = t.code.len() == 6 && t.code.chars().all(|c| c.is_ascii_digit());
        if !is_real && !t.name.is_empty() {
            let name = t.name.clone();
            let resolved = code_cache
                .entry(name.clone())
                .or_insert_with(|| data::resolve_fund_code(&name));
            if let Some(real) = resolved.clone() {
                t.code = real;
            }
        }
    }

    let import_txns = txns
        .iter()
        .map(|t| ImportTxnOut {
            txn_type: t.txn_type.clone(),
            txn_type_raw: t.txn_type_raw.clone(),
            date: t.date.clone(),
            has_year: t.has_year,
            time: t.time.clone(),
            code: t.code.clone(),
            name: t.name.clone(),
            shares: t.shares,
            amount: t.amount,
            price: t.price,
            confidence: t.confidence,
        })
        .collect::<Vec<_>>();

    let note = if ocr_ready {
        if txns.is_empty() && !raw_text.is_empty() {
            format!(
                "OCR 已识别 {} 行文本，但未匹配到交易记录。可在下方「OCR 原始文本」核对，并把截图发我以微调列定位。",
                raw_text.len()
            )
        } else {
            format!(
                "本地 PaddleOCR 识别完成：识别 {} 条交易记录（买/卖/分红）。请核对下方预览（类型/日期/份额/金额/价格均可手改），确认后点「导入交易记录」写入真实流水。",
                txns.len()
            )
        }
    } else {
        "OCR 引擎未就绪：请先运行 src-tauri/download_ocr_models.sh 下载 PP-OCRv4 模型，并以 --features ocr 构建（npm run tauri build --features ocr）。".into()
    };

    Ok(ImportTxnPreviewOut {
        platform: platform.clone(),
        platform_name: platform_name(&platform),
        detected_count: txns.len(),
        txns: import_txns,
        ocr_ready,
        note,
        raw_lines: raw_text,
    })
}

/// 交易记录截图 OCR 预览——**内存字节版**（M2-P0「内容传参」，见 import_screenshots_b64 说明）。
#[tauri::command]
pub fn import_txn_screenshots_b64(
    app: tauri::AppHandle,
    platform: String,
    images_b64: Vec<String>,
) -> Result<ImportTxnPreviewOut, String> {
    let paths = write_temp_images(&app, &images_b64, "fl_ocr_txn")?;
    let r = import_txn_screenshots(
        app,
        platform,
        paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    );
    for p in &paths {
        let _ = std::fs::remove_file(p);
    }
    r
}

#[tauri::command]
pub fn refresh_quotes() -> Result<RefreshOut, String> {
    // 拉取所有已披露持仓的实时行情并写入缓存
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let mut all_codes: Vec<String> = Vec::new();
    for f in &funds {
        for d in db::list_disclosures(&f.code).unwrap_or_default() {
            all_codes.push(to_quote_symbol(&d.stock_code));
        }
    }
    let quotes = data::fetch_quotes(&all_codes).unwrap_or_default();
    for q in quotes.values() {
        let _ = db::upsert_quote(&q.stock_code, q.price, q.prev_close);
    }
    Ok(RefreshOut {
        ok: true,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        count: quotes.len(),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshOut {
    pub ok: bool,
    pub at: String,
    pub count: usize,
}

// ===================== 批量刷新今日官方净值 =====================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshNavOut {
    /// 全部持仓基金数
    pub total: usize,
    /// 已持有今日/最新官方净值、无需刷新的只数
    pub skipped: usize,
    /// 本次实际发起抓取并成功写入的只数
    pub fetched: usize,
    /// 其中成功取到「今日」官方净值的只数（盘面将切换为「实际」）
    pub got_today: usize,
    /// 抓取失败只数（网络/接口异常）
    pub failed: usize,
    /// 抓取失败的基金代码
    pub failed_codes: Vec<String>,
    /// 操作完成时间
    pub at: String,
}

/// 批量刷新「官方净值尚未更新到最新交易日」的基金官方净值。
///
/// 判定「需要刷新」：官方净值日期(nav_date)为空，或早于今日。收盘后基金公司披露的最新净值，
/// 其 nav_date 等于上一交易日（ yesterday ），必须允许刷新；此前用 `parsed < yesterday` 会错误地跳过
/// 这些已披露但未入库的净值，导致「刷新今日净值」按钮无法拿到最新数据。
/// - 盘中点击不会拿到「今日」净值（今日净值收盘后才发布），仅会尝试刷新确实陈旧的基金；
/// - QDII T+1 海外净值滞后，刷新后 nav_date 仍为海外交易日，不会计入 got_today，盘面继续显示「估算」；
/// - 仅对确实需要刷新的基金发起网络请求，并对每只做礼貌间隔以降低被限流概率。
#[tauri::command]
pub fn refresh_official_nav() -> Result<RefreshNavOut, String> {
    let today = chrono::Local::now().date_naive();
    let today_s = today.format("%Y-%m-%d").to_string();
    let funds = db::list_funds_with_nav_date().map_err(|e| e.to_string())?;
    let total = funds.len();
    let mut skipped = 0usize;
    let mut fetched = 0usize;
    let mut got_today = 0usize;
    let mut failed_codes: Vec<String> = Vec::new();
    // 频率控制（2026-08-25）：连续失败退避 + 暂停，降低被东财接口限流/拒绝的概率
    let mut consecutive_fail = 0usize;

    for f in &funds {
        // 判定是否需要刷新：无 nav_date，或 nav_date 早于今日。收盘后披露的最新净值 nav_date 通常为上一交易日，
        // 必须允许刷新；盘中点击则不会拿到「今日」净值（尚未发布），只会重试陈旧数据。
        let need = match &f.nav_date {
            Some(d) if !d.is_empty() => match chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                Ok(parsed) => parsed < today,
                Err(_) => true,
            },
            _ => true,
        };
        if !need {
            skipped += 1;
            continue;
        }
        let code = f.code.clone();
        match data::fetch_official_nav_with_prev(&code) {
            Some((nav, prev)) => {
                if nav.nav_date == today_s {
                    got_today += 1;
                }
                // 类型码已有时复用，避免每只多一次网络请求；缺失才补拉
                let ftype = if f.fund_type.is_empty() {
                    data::fetch_fund_type(&code).unwrap_or_default()
                } else {
                    f.fund_type.clone()
                };
                let prev_nav = prev.as_ref().map(|p| p.nav);
                let _ = db::update_fund_nav(
                    &code,
                    nav.nav,
                    &ftype,
                    data::is_estimable_fund(&ftype),
                    &nav.nav_date,
                    prev_nav,
                );
                // 同时把最新两条净值写入 nav_history，供风险指标与后续校验使用。
                // 腾讯兜底来源的 prev 可能缺净值日期（由日涨跌幅反推），跳过避免污染历史。
                let mut pts: Vec<crate::data::NavPoint> = Vec::with_capacity(2);
                if let Some(p) = prev.as_ref() {
                    if !p.nav_date.is_empty() {
                        pts.push(crate::data::NavPoint {
                            date: p.nav_date.clone(),
                            nav: p.nav,
                            acc_nav: 0.0,
                        });
                    }
                }
                pts.push(crate::data::NavPoint {
                    date: nav.nav_date.clone(),
                    nav: nav.nav,
                    acc_nav: 0.0,
                });
                let _ = db::upsert_nav_history(&code, &pts);
                fetched += 1;
                consecutive_fail = 0;
            }
            None => {
                failed_codes.push(code);
                consecutive_fail += 1;
                // 失败退避：单只失败后歇 800ms；连续失败 5 只后暂停 3s（防接口拒绝）
                if consecutive_fail >= 5 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    consecutive_fail = 0;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
            }
        }
        // 礼貌间隔，降低被东财接口限流的概率（100ms → 200ms，配合自动补齐更稳）
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    // 净值刷新完成后，回填「待净值」交易流水（OCR 金额导入、此前本地无确认日净值）；
    // 【v9】回填内部已把新增份额的增量应用到持仓，不再需要全量重放。
    let _ = db::backfill_pending_txn_shares(1);

    invalidate_caches();
    Ok(RefreshNavOut {
        total,
        skipped,
        fetched,
        got_today,
        failed: failed_codes.len(),
        failed_codes,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

// ===================== 数据库备份 / 恢复（SPEC §F5：SQLite 可导出备份） =====================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    /// 备份文件路径
    pub path: String,
    /// 文件大小（字节）
    pub size: i64,
    /// 操作完成时间
    pub at: String,
}

/// 导出当前数据库为独立备份文件（在线一致快照，活动库不受影响）。
/// `target_path` 由前端通过系统「保存」对话框选定（含 .db 扩展名）。
#[tauri::command]
pub fn export_db(target_path: String) -> Result<BackupInfo, String> {
    let dest = std::path::Path::new(&target_path);
    db::export_db_backup(dest).map_err(|e| format!("导出备份失败: {e}"))?;
    let size = std::fs::metadata(dest).map(|m| m.len() as i64).unwrap_or(0);
    Ok(BackupInfo {
        path: target_path,
        size,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

/// 从备份文件恢复数据库（整个覆盖当前数据，活动连接保持有效）。
/// 调用方（前端）必须先经用户二次确认，因为此操作不可逆地替换全部本地数据。
#[tauri::command]
pub fn import_db(source_path: String) -> Result<BackupInfo, String> {
    let src = std::path::Path::new(&source_path);
    if !src.is_file() {
        return Err(format!("备份文件不存在: {source_path}"));
    }
    db::import_db_backup(src).map_err(|e| format!("导入恢复失败: {e}"))?;
    let size = std::fs::metadata(src).map(|m| m.len() as i64).unwrap_or(0);
    invalidate_caches();
    Ok(BackupInfo {
        path: source_path,
        size,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

/// 备份文件的内存字节载体（M2-P0「内容传参」）：移动端 SAF/content:// URI 无法被 std::fs 写入，
/// 前端拿到 base64 后走系统分享/下载落地；桌面端仍走 export_db(target_path) 路径版。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupB64Out {
    /// 备份文件原始字节的 base64（无 data: 前缀）
    pub data: String,
    pub size: i64,
    pub at: String,
    pub file_name: String,
}

/// 导出数据库备份——**内存字节版**：后端在应用缓存目录生成在线一致快照后读回字节，
/// 以 base64 返回（快照文件随即删除）。逻辑同 export_db，只是目的地从用户选定路径改为内存。
#[tauri::command]
pub fn export_db_b64(app: tauri::AppHandle) -> Result<BackupB64Out, String> {
    use base64::Engine;
    let base = app_cache_dir(&app)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = base.join(format!("fl_backup_{nanos}.db"));
    db::export_db_backup(&tmp).map_err(|e| format!("导出备份失败: {e}"))?;
    let bytes = std::fs::read(&tmp).map_err(|e| format!("读取备份失败: {e}"))?;
    let _ = std::fs::remove_file(&tmp);
    let size = bytes.len() as i64;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    Ok(BackupB64Out {
        data: b64,
        size,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        file_name: format!("fundlens-backup-{stamp}.db"),
    })
}

/// 从内存字节恢复数据库——**内容传参版**（逻辑同 import_db）。前端经 <input type=file>
/// 读取 .db 字节 → base64 传入，后端落缓存临时文件后走既有 db::import_db_backup 整库覆盖。
#[tauri::command]
pub fn import_db_b64(app: tauri::AppHandle, data: String) -> Result<BackupInfo, String> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine
        .decode(&data)
        .map_err(|e| format!("备份数据解码失败: {e}"))?;
    let base = app_cache_dir(&app)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = base.join(format!("fl_restore_{nanos}.db"));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时备份失败: {e}"))?;
    let r = db::import_db_backup(&tmp).map_err(|e| format!("导入恢复失败: {e}"));
    let _ = std::fs::remove_file(&tmp);
    let size = bytes.len() as i64;
    r.map(|_| {
        invalidate_caches();
        BackupInfo {
            path: "(base64 内存导入)".to_string(),
            size,
            at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    })
}

// ============================================================
// 多设备同步 M3：设备快照导出/导入 + 冲突/状态查询（传输无关）
//
// 命令层只认「设备快照」（crate::sync::full_device_snapshot）：
//   全部存活行 upsert（ts = 该行 updated_at）+ 删除墓碑 → JSONL。
// 文件是本阶段的 interim transport；M2 的 CloudBase 通道复用同一载荷与同一批命令语义，
// 因此 UI 与命令面在接入云通道时无需返工。导入回放幂等（LWW + upsert/delete），可反复执行。
// ============================================================

/// 快照导出结果（桌面：写入用户选定路径）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshotOut {
    /// 落盘路径
    pub path: String,
    /// 变更条数（头行除外）
    pub count: usize,
    /// 文件字节数
    pub size: i64,
    /// 操作完成时间
    pub at: String,
}

/// 快照导出结果（移动端：内存字节 + base64，由前端走系统分享落地）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshotB64Out {
    pub file_name: String,
    pub data: String,
    pub count: usize,
    pub size: i64,
    pub at: String,
}

/// 快照导入结果。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshotImportOut {
    /// 成功应用条数
    pub applied: usize,
    /// 因对端更新更晚而落 sync_conflicts 的条数
    pub conflicts: usize,
    /// 快照内变更总条数
    pub total: usize,
    /// 源设备标识（文件无头时为 unknown）
    pub device: String,
    /// 操作完成时间
    pub at: String,
    /// 本次导入前的自动备份文件名（M4；备份失败时为 null，不阻断导入）
    pub backup_file: Option<String>,
}

/// 一条未解冲突。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConflictRow {
    pub id: i64,
    pub tbl: String,
    /// 表的中文标签（后端统一映射，供列表直接展示）
    pub table_label: String,
    pub row_key: String,
    pub device: String,
    pub resolved: i64,
    pub created_at: String,
}

/// 同步状态（供 UI 状态卡）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// 本设备标识
    pub device_id: String,
    /// 参与同步的表数量
    pub tables_synced: usize,
    /// 自上次导出以来本机新增的变更条数
    pub pending_changes: i64,
    /// sync_log 总条数
    pub total_changes: i64,
    /// 最近导出/导入时间（未做过为 null）
    pub last_export_at: Option<String>,
    pub last_import_at: Option<String>,
    /// 未解冲突数
    pub conflict_count: i64,
    /// M4 自动备份：保留份数 / 现有份数 / 最近一份时间 / 备份目录
    pub backup_keep: i64,
    pub backup_count: usize,
    pub last_backup_at: Option<String>,
    pub backup_dir: String,
    /// M2 云通道：模式 / 是否配置完整 / 地址（展示用，不含令牌）/ 最近推送、拉取时间 / 已认识的远端设备数
    pub cloud_mode: String,
    pub cloud_ready: bool,
    pub cloud_endpoint: String,
    pub cloud_dir: String,
    pub cloud_token_set: bool,
    pub cloud_last_push: Option<String>,
    pub cloud_last_pull: Option<String>,
    pub cloud_peers: usize,
}

/// sync_meta 键：上次导出时的日志游标（用于「待同步」计数）。
const META_EXPORT_CURSOR: &str = "file_export_cursor";

/// 生成设备快照文本（并顺带记录导出时间/游标到 sync_meta）。
fn build_snapshot_text() -> Result<(String, usize), String> {
    let (changes, device, at) = db::with_conn(|conn| {
        let changes = crate::sync::full_device_snapshot(conn)?;
        let device = crate::sync::device_id(conn)?;
        let (ts, id) = crate::sync::latest_log_cursor(conn)?;
        let cursor = serde_json::json!({ "ts": ts, "id": id }).to_string();
        crate::sync::write_meta(conn, META_EXPORT_CURSOR, &cursor)?;
        Ok((changes, device, at_now()))
    })
    .map_err(|e| format!("生成快照失败: {e}"))?;

    let count = changes.len();
    let text = crate::sync::snapshot_to_jsonl(&changes, &device, &at);
    db::with_conn(|conn| crate::sync::write_meta(conn, crate::sync::META_LAST_EXPORT, &at))
        .map_err(|e| format!("记录导出时间失败: {e}"))?;
    Ok((text, count))
}

/// 当前本地时间（秒级，统一格式）。
fn at_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 从快照文本导入（解析 → LWW 回放 → 记时间与失效缓存）。
fn import_snapshot_text(text: &str) -> Result<SyncSnapshotImportOut, String> {
    let (header, changes) =
        crate::sync::parse_snapshot(text).map_err(|e| format!("快照解析失败: {e}"))?;
    let device = header
        .map(|h| h.device)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let total = changes.len();
    // M4 写库前自动备份：导入是写操作，先留一份可回退的整库快照（best-effort，失败不阻断导入）。
    let backup_file = crate::backup::auto_backup_before_write("pre-import").map(|b| b.file);
    // 回放包在一个事务里，并开启 defer_foreign_keys：
    // - 原子性：任一条抛错即整体回滚，绝不留下「导了一半」的库；
    // - 外键顺序无关：快照已按父表优先导出（sync::SNAPSHOT_TABLE_ORDER），但被 LWW 跳过的父行、
    //   或外部工具改序后的文件仍可能让子表先落库 → 延迟到 COMMIT 时统一校验，避免误报约束失败。
    let (applied, conflicts) = db::with_conn(|conn| {
        conn.execute_batch("BEGIN; PRAGMA defer_foreign_keys = ON;")?;
        match crate::sync::apply_changeset_lww(conn, &changes, &device) {
            Ok(r) => {
                conn.execute_batch("COMMIT;")?;
                Ok(r)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                Err(e)
            }
        }
    })
    .map_err(|e| format!("快照回放失败: {e}"))?;
    let at = at_now();
    db::with_conn(|conn| crate::sync::write_meta(conn, crate::sync::META_LAST_IMPORT, &at))
        .map_err(|e| format!("记录导入时间失败: {e}"))?;
    // 回放直写 positions/funds 等表 → 总览/明细缓存必须失效（复用既有失效入口）。
    invalidate_caches();
    Ok(SyncSnapshotImportOut {
        applied,
        conflicts,
        total,
        device,
        at,
        backup_file,
    })
}

/// 导出设备快照到用户选定路径（桌面端；`.jsonl`）。
#[tauri::command]
pub fn sync_export_snapshot(target_path: String) -> Result<SyncSnapshotOut, String> {
    let (text, count) = build_snapshot_text()?;
    let path = std::path::Path::new(&target_path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    std::fs::write(path, text.as_bytes()).map_err(|e| format!("写入快照失败: {e}"))?;
    let size = std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0);
    Ok(SyncSnapshotOut {
        path: target_path,
        count,
        size,
        at: at_now(),
    })
}

/// 导出设备快照为内存字节（移动端：无系统保存对话框，前端走系统分享/下载落地）。
#[tauri::command]
pub fn sync_export_snapshot_b64() -> Result<SyncSnapshotB64Out, String> {
    use base64::Engine;
    let (text, count) = build_snapshot_text()?;
    let bytes = text.into_bytes();
    let size = bytes.len() as i64;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    Ok(SyncSnapshotB64Out {
        file_name: format!("fundlens-sync-{stamp}.jsonl"),
        data: b64,
        count,
        size,
        at: at_now(),
    })
}

/// 从快照文件导入（桌面端路径版）。合并语义：按行 LWW，不整库覆盖。
#[tauri::command]
pub fn sync_import_snapshot(source_path: String) -> Result<SyncSnapshotImportOut, String> {
    if !std::path::Path::new(&source_path).is_file() {
        return Err(format!("快照文件不存在: {source_path}"));
    }
    let text = std::fs::read_to_string(&source_path).map_err(|e| format!("读取快照失败: {e}"))?;
    import_snapshot_text(&text)
}

/// 从内存字节导入快照（移动端：<input type=file> / SAF 读取后 base64 传入）。
#[tauri::command]
pub fn sync_import_snapshot_b64(data: String) -> Result<SyncSnapshotImportOut, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data)
        .map_err(|e| format!("快照数据解码失败: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|e| format!("快照内容不是合法 UTF-8: {e}"))?;
    import_snapshot_text(&text)
}

/// 列出 LWW 冲突（未解优先，最新在前）。
#[tauri::command]
pub fn sync_list_conflicts() -> Result<Vec<SyncConflictRow>, String> {
    db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, tbl, row_key, device, resolved, created_at FROM sync_conflicts \
             ORDER BY resolved ASC, id DESC LIMIT 200",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let tbl: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
            out.push(SyncConflictRow {
                id: r.get(0)?,
                table_label: crate::sync::table_label(&tbl).to_string(),
                tbl,
                row_key: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                device: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                resolved: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                created_at: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            });
        }
        Ok(out)
    })
    .map_err(|e| format!("读取冲突失败: {e}"))
}

/// 冲突裁决结果。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConflictResolveOut {
    /// 实际改为已解的条数
    pub resolved: usize,
    /// 写回本地的行数（「保留本地」恒为 0）
    pub applied: usize,
    /// 未能处理（载荷损坏等）的条数，这些条目保持未解
    pub failed: usize,
}

/// 解析裁决口径（大小写不敏感）。未知值一律报错——不静默取默认值，避免误改用户数据。
fn parse_conflict_choice(choice: &str) -> Result<bool, String> {
    match choice.trim().to_ascii_lowercase().as_str() {
        "remote" => Ok(true),
        "local" => Ok(false),
        other => Err(format!("未知的裁决口径：{other}（应为 local 或 remote）")),
    }
}

/// 读取一条冲突的字段级详情（本地当前行 vs 远端被拒变更）。
#[tauri::command]
pub fn sync_conflict_detail(id: i64) -> Result<crate::sync::ConflictDetail, String> {
    let found = db::with_conn(|conn| crate::sync::conflict_detail(conn, id))
        .map_err(|e| format!("读取冲突详情失败: {e}"))?;
    found.ok_or_else(|| format!("冲突记录不存在：{id}"))
}

/// 解算一条冲突：`choice = "local"`（保留本地，丢弃远端）| `"remote"`（采用远端，覆盖本地）。
#[tauri::command]
pub fn sync_conflict_resolve(id: i64, choice: String) -> Result<SyncConflictResolveOut, String> {
    let adopt_remote = parse_conflict_choice(&choice)?;
    let (found, applied) =
        db::with_conn(|conn| crate::sync::resolve_conflict(conn, id, adopt_remote))
            .map_err(|e| format!("解算冲突失败: {e}"))?;
    if !found {
        return Err(format!("冲突记录不存在：{id}"));
    }
    Ok(SyncConflictResolveOut {
        resolved: 1,
        applied,
        failed: 0,
    })
}

/// 批量解算全部未解冲突（同 `sync_conflict_resolve` 的口径）。
#[tauri::command]
pub fn sync_conflicts_resolve_all(choice: String) -> Result<SyncConflictResolveOut, String> {
    let adopt_remote = parse_conflict_choice(&choice)?;
    let (resolved, applied, failed) =
        db::with_conn(|conn| crate::sync::resolve_all_conflicts(conn, adopt_remote))
            .map_err(|e| format!("批量解算冲突失败: {e}"))?;
    Ok(SyncConflictResolveOut {
        resolved,
        applied,
        failed,
    })
}

/// 同步状态：设备标识、参与表数、自上次导出以来的变更数、最近导出/导入时间、未解冲突数。
#[tauri::command]
pub fn sync_status() -> Result<SyncStatus, String> {
    db::with_conn(|conn| {
        let device_id = crate::sync::device_id(conn)?;
        let (cursor_ts, cursor_id): (String, i64) =
            crate::sync::read_meta(conn, META_EXPORT_CURSOR)?
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    (
                        v.get("ts").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        v.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
                    )
                })
                .unwrap_or_default();
        let pending = crate::sync::count_changes_after(conn, &cursor_ts, cursor_id)?;
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM sync_log", [], |r| r.get(0))?;
        let conflicts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sync_conflicts WHERE resolved = 0",
            [],
            |r| r.get(0),
        )?;
        // 备份目录列表只做文件 IO（不再取全局连接），不会与 with_conn 形成嵌套锁。
        let backups = crate::backup::list_backups()?;
        // M2 云通道：配置 + 水位（均存 sync_meta，令牌只回「是否已设置」，绝不回传明文）
        let cloud = crate::cloud::load_config(conn);
        let peers = crate::cloud::seen_keys(conn)?.len();
        Ok(SyncStatus {
            device_id,
            tables_synced: crate::sync::SYNCED_TABLES.len(),
            pending_changes: pending,
            total_changes: total,
            last_export_at: crate::sync::read_meta(conn, crate::sync::META_LAST_EXPORT)?,
            last_import_at: crate::sync::read_meta(conn, crate::sync::META_LAST_IMPORT)?,
            conflict_count: conflicts,
            backup_keep: crate::backup::keep_count_from(conn),
            backup_count: backups.len(),
            last_backup_at: backups.first().map(|b| b.at.clone()),
            backup_dir: crate::backup::backup_dir().to_string_lossy().to_string(),
            cloud_ready: cloud.is_ready(),
            cloud_mode: cloud.mode,
            cloud_endpoint: cloud.endpoint,
            cloud_dir: cloud.dir,
            cloud_token_set: !cloud.token.is_empty(),
            cloud_last_push: crate::sync::read_meta(conn, crate::cloud::META_LAST_PUSH)?,
            cloud_last_pull: crate::sync::read_meta(conn, crate::cloud::META_LAST_PULL)?,
            cloud_peers: peers,
        })
    })
    .map_err(|e| format!("读取同步状态失败: {e}"))
}

/// 立即生成一份整库备份（M4）。
#[tauri::command]
pub fn sync_create_backup() -> Result<crate::backup::BackupEntry, String> {
    crate::backup::create_backup("manual").map_err(|e| format!("备份失败: {e}"))
}

/// 列出全部整库备份（最新在前）。
#[tauri::command]
pub fn sync_list_backups() -> Result<Vec<crate::backup::BackupEntry>, String> {
    crate::backup::list_backups().map_err(|e| format!("读取备份列表失败: {e}"))
}

/// 设置自动备份保留份数（夹在 1..=60），并立即剪枝；返回生效值。
#[tauri::command]
pub fn sync_set_backup_keep(keep: i64) -> Result<i64, String> {
    crate::backup::set_keep_count(keep).map_err(|e| format!("设置保留份数失败: {e}"))
}

/// 恢复单条备份的返回体（camelCase，前端契约冻结）。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreBackupOut {
    /// 被恢复的备份文件名
    pub file: String,
    /// 恢复前自动生成的安全网备份文件名（安全网失败时置 None，供前端警告）
    pub safety_backup: Option<String>,
}

/// 校验 `file` 只能是一个位于 `backup_dir()` 内的纯文件名，防路径穿越。
///
/// 拒绝：空串 / 含 `/`、`\`、`..` / 非 `.db` 扩展名 / 拼出的路径不在备份目录直接子级 /
/// 目标文件不存在。返回拼好的绝对路径或面向用户的中文错误。
fn resolve_backup_file(file: &str) -> Result<std::path::PathBuf, String> {
    if file.is_empty() {
        return Err("格式不合法的备份文件名".into());
    }
    if file.contains('/') || file.contains('\\') || file.contains("..") {
        return Err("格式不合法的备份文件名".into());
    }
    let dir = crate::backup::backup_dir();
    let p = dir.join(file);
    // 必须落在备份目录的直接子级（join 不会归一化 `..`，这里再用 parent 双保险）。
    match p.parent() {
        Some(parent) if parent == dir.as_path() => {}
        _ => return Err("格式不合法的备份文件名".into()),
    }
    if !file.ends_with(".db") {
        return Err("格式不合法的备份文件名".into());
    }
    if !p.is_file() {
        return Err("备份文件不存在".into());
    }
    Ok(p)
}

/// 从一份整库备份在线恢复（M4 闭环：安全网 + 恢复 + 失效缓存）。
///
/// 顺序：①先 `create_backup("before-restore")` 做安全网（捕获恢复前状态），失败**不阻断**恢复；
/// ②再 `db::import_db_backup` 在线整库覆盖（活动连接保持有效）；
/// ③恢复后 `invalidate_caches()`，否则 UI 读到旧快照。
/// 注意：create_backup / import_db_backup 各自取全局锁、顺序执行，绝不在持锁闭包内调用。
#[tauri::command]
pub fn sync_restore_backup(file: String) -> Result<RestoreBackupOut, String> {
    let path = resolve_backup_file(&file)?;
    // ① 显式校验源文件是有效的 SQLite 库。
    //    实测（对抗性探针）：即使不做这步，用垃圾文件恢复也不会破坏活动库 —— SQLite 的
    //    sqlite3_backup 把写入放在事务内、失败即回滚，且源以只读打开会以 NOTADB 失败。
    //    但那个错误对用户不可读，且「安全」是隐式的；这里把它变成显式、可读、可测的守卫。
    {
        let probe = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| "所选文件不是有效的数据库备份".to_string())?;
        probe
            .query_row("PRAGMA schema_version", [], |r| r.get::<_, i64>(0))
            .map_err(|_| "所选文件不是有效的数据库备份（已损坏或非 SQLite 文件）".to_string())?;
    }
    // ② 安全网：恢复前先备份当前库，失败不阻断（用户就是来救数据的）。
    let safety = crate::backup::create_backup("before-restore")
        .ok()
        .map(|b| b.file);
    // ③ 在线整库覆盖恢复。
    crate::db::import_db_backup(&path).map_err(|e| format!("恢复备份失败: {e}"))?;
    // ④ 失效总览 + 明细快照缓存。
    invalidate_caches();
    Ok(RestoreBackupOut {
        file,
        safety_backup: safety,
    })
}

/// 删除一条备份文件（M4 闭环）。目标不存在即报错，绝不静默成功。
#[tauri::command]
pub fn sync_delete_backup(file: String) -> Result<(), String> {
    let path = resolve_backup_file(&file)?;
    std::fs::remove_file(&path).map_err(|_| "备份文件不存在".to_string())?;
    Ok(())
}

// ============================================================
// 多设备同步 M2：云通道（CloudBase 云函数 / 自建 relay / 本地目录）
//
// 传输实现在 crate::cloud（SyncTransport），本层只做「读配置 → 构造传输 → 编排 → 失效缓存」。
// 通道配置存 sync_meta（**不是** settings）→ 同步令牌不会随设备快照上传到远端。
// 换后端不改命令面与 UI：只要服务端实现同一份 relay 协议即可。
// ============================================================

/// 云通道配置的对外视图：**绝不含令牌明文**，只告知是否已配置。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfigOut {
    /// off / dir / cloud
    pub mode: String,
    pub endpoint: String,
    pub dir: String,
    pub token_set: bool,
    /// 配置是否完整、可发起同步
    pub ready: bool,
}

impl From<crate::cloud::CloudConfig> for CloudConfigOut {
    fn from(c: crate::cloud::CloudConfig) -> Self {
        let ready = c.is_ready();
        Self {
            mode: c.mode,
            endpoint: c.endpoint,
            dir: c.dir,
            token_set: !c.token.is_empty(),
            ready,
        }
    }
}

/// 连通性检查结果。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCheckOut {
    /// 通道模式
    pub mode: String,
    /// 远端条目总数
    pub items: usize,
    /// 其中属于**其它设备**的设备快照数（即潜在可拉取量）
    pub others: usize,
    /// 本设备标识
    pub device_id: String,
}

/// 在 `db::with_conn` 闭包内调用「返回 Result<_, String>」的云编排函数。
///
/// rusqlite 的错误类型装不下任意字符串，故用哨兵错误把字符串带出闭包，并在外层立即还原为原始文案
/// （哨兵本身绝不会暴露给用户）。这样云内核可以保持 `Result<_, String>` 的纯粹签名，
/// 既能在无全局连接的场景（纯函数单测）使用，也能走全局连接的命令层。
fn cloud_in_conn<T>(
    label: &str,
    f: impl FnOnce(&rusqlite::Connection) -> Result<T, String>,
) -> Result<T, String> {
    let mut failure: Option<String> = None;
    let r = db::with_conn(|conn| match f(conn) {
        Ok(v) => Ok(v),
        Err(e) => {
            failure = Some(e);
            Err(rusqlite::Error::InvalidParameterName(String::new()))
        }
    });
    match (r, failure) {
        (Ok(v), _) => Ok(v),
        (Err(_), Some(e)) => Err(format!("{label}: {e}")),
        (Err(e), None) => Err(format!("{label}: {e}")),
    }
}

/// 读取云通道配置（不含令牌明文）。
#[tauri::command]
pub fn sync_cloud_config_get() -> Result<CloudConfigOut, String> {
    let cfg = db::with_conn(|conn| Ok(crate::cloud::load_config(conn)))
        .map_err(|e| format!("读取云通道配置失败: {e}"))?;
    Ok(CloudConfigOut::from(cfg))
}

/// 保存云通道配置。
///
/// `token` 为 `None` 表示「保持不变」（UI 上不回显令牌，留空即不修改）；
/// 传 `Some("")` 表示清空令牌。
#[tauri::command]
pub fn sync_cloud_config_set(
    mode: String,
    endpoint: String,
    dir: String,
    token: Option<String>,
) -> Result<CloudConfigOut, String> {
    let saved = db::with_conn(|conn| {
        let mut cfg = crate::cloud::load_config(conn);
        cfg.mode = mode.clone();
        cfg.endpoint = endpoint.clone();
        cfg.dir = dir.clone();
        if let Some(t) = &token {
            cfg.token = t.clone();
        }
        crate::cloud::save_config(conn, &cfg)
    })
    .map_err(|e| format!("保存云通道配置失败: {e}"))?;
    Ok(CloudConfigOut::from(saved))
}

/// 用当前配置构造传输实现；返回（模式, 传输）。
///
/// 抽成一处的原因：三个云命令都要「读配置 → 构造传输」，且都必须发生在取全局连接**之外**
/// （构造 http 客户端耗时，且后续编排要自己开闭包取连接）。
fn build_cloud_transport() -> Result<(String, Box<dyn crate::cloud::SyncTransport>), String> {
    let cfg = db::with_conn(|conn| Ok(crate::cloud::load_config(conn)))
        .map_err(|e| format!("读取云通道配置失败: {e}"))?;
    let mode = cfg.mode.clone();
    let transport = crate::cloud::transport_from_config(&cfg).map_err(|e| format!("云通道不可用: {e}"))?;
    Ok((mode, transport))
}

/// 读取本设备标识。
fn local_device_id() -> Result<String, String> {
    db::with_conn(|conn| crate::sync::device_id(conn))
        .map_err(|e| format!("读取本机设备标识失败: {e}"))
}

/// 连通性检查：列出远端条目（只读：不上传、不改库、不推进水位）。
#[tauri::command]
pub fn sync_cloud_check() -> Result<CloudCheckOut, String> {
    let (mode, transport) = build_cloud_transport()?;
    let device_id = local_device_id()?;
    let items = transport.list()?;
    let others = items
        .iter()
        .filter(|i| i.kind == crate::cloud::KIND_SNAPSHOT && i.device != device_id)
        .count();
    Ok(CloudCheckOut {
        mode,
        items: items.len(),
        others,
        device_id,
    })
}

/// 立即把本设备快照推送到云通道。
#[tauri::command]
pub fn sync_cloud_push() -> Result<crate::cloud::PushOutcome, String> {
    let (_mode, transport) = build_cloud_transport()?;
    cloud_in_conn("云端推送失败", |conn| {
        crate::cloud::push(conn, transport.as_ref())
    })
}

/// 立即从云通道拉取他设备快照并按行 LWW 合并。
///
/// 顺序很关键：①先算**只读**的拉取计划 → ②计划为空就直接返回（不备份、不写库，避免反复点「拉取」刷出一堆空备份）
/// → ③有内容可拉时先做写库前自动备份（与文件导入同一口径，tag 复用 `pre-import`）→ ④再回放。
/// 备份必须发生在取连接之外：它自己会取全局连接，而在 `with_conn` 闭包里嵌套加锁会死锁。
#[tauri::command]
pub fn sync_cloud_pull() -> Result<crate::cloud::PullOutcome, String> {
    let (_mode, transport) = build_cloud_transport()?;
    let plan = cloud_in_conn("云端拉取失败", |conn| {
        crate::cloud::pull_plan(conn, transport.as_ref())
    })?;
    if plan.is_empty() {
        return Ok(crate::cloud::PullOutcome::default());
    }
    let _ = crate::backup::auto_backup_before_write("pre-import");
    let out = cloud_in_conn("云端拉取失败", |conn| {
        crate::cloud::pull_apply(conn, transport.as_ref(), &plan)
    })?;
    if out.applied > 0 {
        invalidate_caches();
    }
    Ok(out)
}


/// 将任意文本内容写入用户选定的路径（创建父目录），用于周报/月报「保存为 .md」等导出场景。
/// 路径由前端对话框取得，仅写用户明确选定的文件；不限制扩展名（调用方决定内容语义）。
#[tauri::command]
pub fn write_text_file(target_path: String, content: String) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(&target_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target_path, content).map_err(|e| format!("写入文件失败: {e}"))
}

#[tauri::command]
pub fn add_fund(
    code: String,
    name: String,
    platform: String,
    official_nav: f64,
    shares: f64,
    cost_amount: f64,
) -> Result<(), String> {
    let f = db::FundRow {
        code: code.clone(),
        name,
        platform: platform.clone(),
        official_nav,
        report_period: None,
        disclosure_type: None,
        fund_type: String::new(),
        track_index: String::new(),
        valuation_applicable: true,
    };
    db::insert_fund(&f).map_err(|e| e.to_string())?;
    // 以「手动基线」写入持仓（替换既有 import/manual_set 基线，避免重复计数），随后重算。
    // 单机单账户固定 account_id = 1。
    db::set_baseline(1, &code, shares, cost_amount, 0.0, 0.0, 0.0, 0.0, &platform, "manual_set")
        .map_err(|e| e.to_string())?;
    // 尝试用官方接口补全净值与基金类型（lsjz 提供净值；类型用 fundsuggest 的 FTYPE，更可靠）
    if let Some(nav) = data::fetch_official_nav(&code) {
        let ftype = data::fetch_fund_type(&code).unwrap_or_default();
        let _ = db::update_fund_nav(
            &code,
            nav.nav,
            &ftype,
            data::is_estimable_fund(&ftype),
            &nav.nav_date,
            None,
        );
    }
    invalidate_caches();
    Ok(())
}

#[tauri::command]
pub fn update_position(code: String, shares: f64, cost_amount: f64, platform: Option<String>) -> Result<(), String> {
    // 手动改仓：平台优先透传调用方指定值；未指定（None 或空串）时回退到该基金既有持仓平台，
    // 避免落到空 '' 平台行而与真实持仓错层（多平台分别持有场景下尤其关键）。
    let platform = platform
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| db::resolve_position_platform(1, &code).unwrap_or_default());
    // 【v9】改持仓直接覆盖权威 positions，不产生任何流水（用户定稿：改持仓不产生流水）。
    db::update_position_inplace(1, &code, shares, cost_amount, &platform)
        .map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(())
}

#[tauri::command]
pub fn update_position_cost(code: String, cost_price: f64, platform: Option<String>) -> Result<(), String> {
    // 仅修改持仓成本价（单位成本）：目标持仓成本 = 当前份额 × 成本价，**不产生任何流水/账本改动**
    // （【v9】直接更新 positions 权威成本字段）。平台解析与 update_position 一致。
    let platform = platform
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| db::resolve_position_platform(1, &code).unwrap_or_default());
    if cost_price < 0.0 {
        return Err("成本价不能为负".to_string());
    }
    db::update_position_cost(1, &code, cost_price, &platform)
        .map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(())
}

#[tauri::command]
pub fn delete_fund(code: String) -> Result<(), String> {
    db::delete_fund(&code).map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(())
}

#[tauri::command]
pub fn list_disclosures(code: String) -> Result<Vec<valuation::DisclosedHolding>, String> {
    db::list_disclosures(&code).map_err(|e| e.to_string())
}

#[tauri::command]
/// 拉取某基金最新披露持仓并写入本地库。
/// 只替换「本次抓到的那一期」，其余历史期次完整保留——多期共存，供「较上期」对比展示。
/// 供单只 `fetch_disclosure` 与批量 `fetch_all_disclosures` 复用。
fn store_disclosure(code: &str) -> Result<usize, String> {
    let (period, _dtype, holdings) = data::fetch_disclosure(code).ok_or("拉取披露持仓失败")?;
    let n = db::replace_disclosure_period(code, &period, &holdings).map_err(|e| e.to_string())?;
    // 披露持仓写后失效总览/明细快照（refresh_official_nav / fetch_disclosure / fetch_all_disclosures 均经此写入）
    invalidate_caches();
    Ok(n)
}

#[tauri::command]
pub fn fetch_disclosure(code: String) -> Result<usize, String> {
    store_disclosure(&code)
}

// ─────────────────────────────────────────────────────────────
// 批量后台任务通用状态机：披露抓取 / 今日净值刷新 共用（2026-09-11 卡死修复）
// 根因：两个批量命令原为同步命令跑在主线程，逐只「网络请求(超时上限8s) + 500ms 节流
// + 失败退避」全量 2~5 分钟占死主线程 → 整个 UI 卡死。统一改为 start 立即返回 +
// 独立线程执行 + 前端轮询进度（按钮实时显示 n/total），并支持协作式取消。
// ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct FetchTaskState {
    running: bool,
    total: usize,
    done: usize,
    ok: usize,
    failed: usize,
    /// 无需处理而跳过的只数（净值刷新：净值已最新；披露任务恒 0）
    skipped: usize,
    /// 其中取到「今日」净值的只数（仅净值刷新使用）
    got_today: usize,
    current: Option<String>,
    failed_codes: Vec<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    cancel: bool,
    /// 本次任务是否被用户取消过（仅结束后为 true，供快照展示）
    was_cancelled: bool,
}

fn now_ts() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchTaskProgress {
    pub running: bool,
    pub total: usize,
    pub done: usize,
    pub ok: usize,
    pub failed: usize,
    /// 跳过只数（披露任务恒 0）
    pub skipped: usize,
    /// 取到「今日」净值的只数（仅净值刷新非 0）
    pub got_today: usize,
    /// 当前正在处理的基金代码（None=空闲或已结束）
    pub current: Option<String>,
    pub failed_codes: Vec<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// 本次任务是否被用户取消（仅结束后为 true）
    pub cancelled: bool,
}

impl FetchTaskState {
    const fn new() -> Self {
        Self {
            running: false,
            total: 0,
            done: 0,
            ok: 0,
            failed: 0,
            skipped: 0,
            got_today: 0,
            current: None,
            failed_codes: Vec::new(),
            started_at: None,
            finished_at: None,
            cancel: false,
            was_cancelled: false,
        }
    }

    fn snapshot(&self) -> FetchTaskProgress {
        FetchTaskProgress {
            running: self.running,
            total: self.total,
            done: self.done,
            ok: self.ok,
            failed: self.failed,
            skipped: self.skipped,
            got_today: self.got_today,
            current: self.current.clone(),
            failed_codes: self.failed_codes.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
            cancelled: self.was_cancelled,
        }
    }

    /// 任务收尾：统一记账并清出运行态（cancelled 标记本次是否被用户取消）。
    fn finish(&mut self, cancelled: bool) {
        self.failed = self.failed_codes.len();
        self.running = false;
        self.current = None;
        self.finished_at = Some(now_ts());
        self.was_cancelled = cancelled;
        self.cancel = false;
    }
}

static DISCLOSURE_FETCH: Mutex<FetchTaskState> = Mutex::new(FetchTaskState::new());
static NAV_REFRESH: Mutex<FetchTaskState> = Mutex::new(FetchTaskState::new());

/// 通用进度查询（参数区分任务通道）。
fn task_progress(st: &Mutex<FetchTaskState>) -> Result<FetchTaskProgress, String> {
    let s = st.lock().unwrap_or_else(|e| e.into_inner());
    Ok(s.snapshot())
}

/// 通用协作式取消请求（当前这只跑完即停）。返回是否确有任务在跑。
fn task_cancel(st: &Mutex<FetchTaskState>) -> Result<bool, String> {
    let mut s = st.lock().unwrap_or_else(|e| e.into_inner());
    if !s.running {
        return Ok(false);
    }
    s.cancel = true;
    Ok(true)
}

/// 启动批量披露抓取后台任务。幂等：已在跑则直接返回当前进度，不重复启动。
#[tauri::command]
pub fn disclosure_fetch_start() -> Result<FetchTaskProgress, String> {
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let mut st = DISCLOSURE_FETCH.lock().unwrap_or_else(|e| e.into_inner());
    if st.running {
        return Ok(st.snapshot()); // 已有任务在跑：幂等返回进度
    }
    *st = FetchTaskState {
        running: true,
        total: funds.len(),
        started_at: Some(now_ts()),
        ..FetchTaskState::new()
    };
    let snapshot = st.snapshot();
    drop(st);

    // 后台线程逐只抓取：DB 锁只在每只写库瞬间短暂持有，网络等待期间不占任何锁，
    // 总览/行情等其他命令完全不受影响（与旧同步版占死主线程的本质区别）。
    // 失败安全与旧同步版一致：单只失败仅记入 failed_codes，不中断整体。
    std::thread::spawn(move || {
        let mut cancelled = false;
        for f in &funds {
            {
                let mut st = DISCLOSURE_FETCH.lock().unwrap_or_else(|e| e.into_inner());
                if st.cancel {
                    cancelled = true;
                    break;
                }
                st.current = Some(f.code.clone());
            }
            let res = store_disclosure(&f.code);
            let mut st = DISCLOSURE_FETCH.lock().unwrap_or_else(|e| e.into_inner());
            st.done += 1;
            match res {
                Ok(_) => st.ok += 1,
                Err(_) => st.failed_codes.push(f.code.clone()),
            }
            if st.cancel {
                cancelled = true;
                drop(st);
                break;
            }
            // 礼貌间隔，降低被东财接口限流的概率
            drop(st);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let mut st = DISCLOSURE_FETCH.lock().unwrap_or_else(|e| e.into_inner());
        st.finish(cancelled);
    });
    Ok(snapshot)
}

#[tauri::command]
pub fn disclosure_fetch_progress() -> Result<FetchTaskProgress, String> {
    task_progress(&DISCLOSURE_FETCH)
}

/// 请求取消进行中的批量抓取（协作式：当前这只跑完即停）。返回是否确有任务在跑。
#[tauri::command]
pub fn disclosure_fetch_cancel() -> Result<bool, String> {
    task_cancel(&DISCLOSURE_FETCH)
}

/// 启动今日净值刷新后台任务。幂等：已在跑则直接返回当前进度，不重复启动。
/// 节流/退避/写库口径与旧同步版 refresh_official_nav 完全一致，仅移入后台线程。
#[tauri::command]
pub fn nav_refresh_start() -> Result<FetchTaskProgress, String> {
    let funds = db::list_funds_with_nav_date().map_err(|e| e.to_string())?;
    let mut st = NAV_REFRESH.lock().unwrap_or_else(|e| e.into_inner());
    if st.running {
        return Ok(st.snapshot()); // 已有任务在跑：幂等返回进度
    }
    *st = FetchTaskState {
        running: true,
        total: funds.len(),
        started_at: Some(now_ts()),
        ..FetchTaskState::new()
    };
    let snapshot = st.snapshot();
    drop(st);

    std::thread::spawn(move || {
        let today = chrono::Local::now().date_naive();
        let today_s = today.format("%Y-%m-%d").to_string();
        let mut consecutive_fail = 0usize;
        let mut cancelled = false;
        for f in &funds {
            // 判定是否需要刷新：无 nav_date，或 nav_date 早于今日。收盘后披露的最新净值
            // nav_date 通常为上一交易日，必须允许刷新；盘中点击拿不到「今日」净值（未发布）。
            let need = match &f.nav_date {
                Some(d) if !d.is_empty() => match chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                    Ok(parsed) => parsed < today,
                    Err(_) => true,
                },
                _ => true,
            };
            if !need {
                let mut st = NAV_REFRESH.lock().unwrap_or_else(|e| e.into_inner());
                st.skipped += 1;
                st.done += 1;
                continue;
            }
            {
                let mut st = NAV_REFRESH.lock().unwrap_or_else(|e| e.into_inner());
                if st.cancel {
                    cancelled = true;
                    break;
                }
                st.current = Some(f.code.clone());
            }
            let code = f.code.clone();
            let fetched = data::fetch_official_nav_with_prev(&code);
            {
                let mut st = NAV_REFRESH.lock().unwrap_or_else(|e| e.into_inner());
                st.done += 1;
                match &fetched {
                    Some((nav, _)) => {
                        st.ok += 1;
                        if nav.nav_date == today_s {
                            st.got_today += 1;
                        }
                    }
                    None => {
                        st.failed_codes.push(code.clone());
                        consecutive_fail += 1;
                    }
                }
                if st.cancel {
                    cancelled = true;
                    drop(st);
                    break;
                }
            }
            if let Some((nav, prev)) = fetched {
                // 类型码已有时复用，避免每只多一次网络请求；缺失才补拉
                let ftype = if f.fund_type.is_empty() {
                    data::fetch_fund_type(&code).unwrap_or_default()
                } else {
                    f.fund_type.clone()
                };
                let prev_nav = prev.as_ref().map(|p| p.nav);
                let _ = db::update_fund_nav(
                    &code,
                    nav.nav,
                    &ftype,
                    data::is_estimable_fund(&ftype),
                    &nav.nav_date,
                    prev_nav,
                );
                // 同时把最新两条净值写入 nav_history，供风险指标与后续校验使用。
                // 腾讯兜底来源的 prev 可能缺净值日期（由日涨跌幅反推），跳过避免污染历史。
                let mut pts: Vec<crate::data::NavPoint> = Vec::with_capacity(2);
                if let Some(p) = prev.as_ref() {
                    if !p.nav_date.is_empty() {
                        pts.push(crate::data::NavPoint {
                            date: p.nav_date.clone(),
                            nav: p.nav,
                            acc_nav: 0.0,
                        });
                    }
                }
                pts.push(crate::data::NavPoint {
                    date: nav.nav_date.clone(),
                    nav: nav.nav,
                    acc_nav: 0.0,
                });
                let _ = db::upsert_nav_history(&code, &pts);
                consecutive_fail = 0;
            } else if consecutive_fail >= 5 {
                // 失败退避：单只失败后歇 800ms；连续失败 5 只后暂停 3s（防接口拒绝）
                std::thread::sleep(std::time::Duration::from_secs(3));
                consecutive_fail = 0;
            } else {
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
            // 礼貌间隔，降低被东财接口限流的概率（100ms → 200ms，配合自动补齐更稳）
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // 净值刷新完成后，回填「待净值」交易流水（OCR 金额导入、此前本地无确认日净值）；
        // 【v9】回填内部已把新增份额的增量应用到持仓，不再需要全量重放。
        let _ = db::backfill_pending_txn_shares(1);
        invalidate_caches();
        let mut st = NAV_REFRESH.lock().unwrap_or_else(|e| e.into_inner());
        st.finish(cancelled);
    });
    Ok(snapshot)
}

#[tauri::command]
pub fn nav_refresh_progress() -> Result<FetchTaskProgress, String> {
    task_progress(&NAV_REFRESH)
}

/// 请求取消进行中的净值刷新（协作式：当前这只跑完即停）。返回是否确有任务在跑。
#[tauri::command]
pub fn nav_refresh_cancel() -> Result<bool, String> {
    task_cancel(&NAV_REFRESH)
}

// ===================== 基金穿透（Look-through）：只读分析层 =====================

use crate::lookthrough;

/// 组合穿透主查询：两级行业（9 大类 L1 + 东财细分 L2 直出）+ 个股虚拟重仓（含当日涨跌）
/// + 基金穿透明细（各自报告期/覆盖率）+ 未穿透桶 + 汇总口径。
/// 交易时段批量拉取全部披露股票行情（与总览估算链路同一 fetch_quotes 节流通道，不新增出站压力）
/// 并顺带回写 quotes_cache；非交易时段不带行情（前端隐藏当日列）。
#[tauri::command]
pub fn lookthrough_overview(platform: Option<String>) -> Result<lookthrough::LookthroughResult, String> {
    let mut holdings = db::list_holdings(None).map_err(|e| e.to_string())?;
    if let Some(p) = &platform {
        holdings.retain(|h| &h.platform == p);
    }
    // 批量取每只基金最新披露期持仓（list_disclosures_batch 已保证仅最新期，防跨期合并成「曾持有」）
    let mut disclosures_by_fund: HashMap<String, Vec<valuation::DisclosedHolding>> = HashMap::new();
    for (fc, dh) in db::list_disclosures_batch().unwrap_or_default() {
        disclosures_by_fund.entry(fc).or_default().push(dh);
    }
    // 股票行业画像（L1=映射大类 / L2=东财行业名直出）
    let profiles = db::list_stock_profiles().unwrap_or_default();
    // v2.5 指数成分穿透：装载 constituents map（index_code → 成分 rows），门禁命中则按成分权重展开
    let constituents = load_index_constituents_map();
    // v2.5 门禁需要 track_index：建 code→track_index 索引
    let track_by_code: HashMap<String, String> = db::list_funds()
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.code, f.track_index))
        .collect();

    // 行情：仅交易时段拉取（非交易时段不发起任何行情请求——麒麟刷新卡死防御同款口径）
    let phase = data::market_phase();
    let has_quotes = phase == "intraday";
    let mut quotes: HashMap<String, lookthrough::QuoteLite> = HashMap::new();
    if has_quotes {
        let mut all_syms: Vec<String> = Vec::new();
        for hs in disclosures_by_fund.values() {
            for d in hs {
                let sym = to_quote_symbol(&d.stock_code);
                if !sym.is_empty() && !all_syms.contains(&sym) {
                    all_syms.push(sym);
                }
            }
        }
        let fetched = data::fetch_quotes(&all_syms).unwrap_or_default();
        // fetch_quotes 返回 key=纯数字代码；顺带回写 quotes_cache 供其他链路复用
        for q in fetched.values() {
            let _ = db::upsert_quote(&q.stock_code, q.price, q.prev_close);
            quotes.insert(
                q.stock_code.clone(),
                lookthrough::QuoteLite {
                    change_pct: if q.prev_close > 0.0 { q.price / q.prev_close - 1.0 } else { 0.0 },
                },
            );
        }
    }

    // 基金输入：市值口径 = 份额×最新官方净值（可审计）；货基/兜底用持仓金额。
    // 份额折算与 get_overview 完全一致（shares>0 直用；支付宝风格 shares=0 按 holding_amount/官方净值折算）。
    let funds: Vec<lookthrough::LtFundInput> = holdings
        .iter()
        .map(|h| {
            let eff_shares = if h.shares > 0.0 {
                h.shares
            } else if h.holding_amount > 0.0 && h.official_nav > 0.0 {
                h.holding_amount / h.official_nav
            } else {
                0.0
            };
            let is_money_or_wealth = matches!(h.fund_type.as_str(), "002" | "005");
            let has_real_code = h.code.len() == 6 && h.code.chars().all(|c| c.is_ascii_digit()) && eff_shares > 0.0;
            let market_value = if eff_shares > 0.0 && h.official_nav > 0.0 {
                eff_shares * h.official_nav
            } else {
                h.holding_amount
            };
            let holdings_vec = disclosures_by_fund.get(&h.code).cloned().unwrap_or_default();
            lookthrough::LtFundInput {
                code: h.code.clone(),
                name: h.name.clone(),
                market_value,
                is_money_or_wealth,
                has_real_code,
                report_period: holdings_vec.first().map(|d| d.report_period.clone()),
                holdings: holdings_vec,
                fund_type: h.fund_type.clone(),
                track_index: track_by_code.get(&h.code).cloned().unwrap_or_default(),
            }
        })
        .collect();

    Ok(lookthrough::aggregate(
        &funds,
        &profiles,
        &quotes,
        has_quotes,
        &constituents,
        &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    ))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchStockProfilesOut {
    /// 需要画像的 A 股股票总数（全部披露股票去重后）
    pub total: usize,
    /// 本次缺失/过期需补拉数
    pub needed: usize,
    /// 成功抓取并入库数
    pub fetched: usize,
    pub failed: usize,
    pub failed_codes: Vec<String>,
    pub at: String,
}

/// 批量补股票行业画像：只拉缺失/超过 90 天的股票（P1 起含港股/美股，供境外细分），
/// 走既有全局出站节流（throttle_wait 500ms）；单只失败计数退避，绝不阻塞整体。
#[tauri::command]
pub fn fetch_stock_profiles() -> Result<FetchStockProfilesOut, String> {
    // 收集全部披露股票代码（去重）：A 股 6 位 / 港股 5 位 / 美股纯字母
    let mut codes: Vec<String> = Vec::new();
    for (_, hs) in db::list_disclosures_batch().unwrap_or_default() {
        let c = hs.stock_code.trim();
        let ok = (c.len() == 6 || c.len() == 5) && c.chars().all(|ch| ch.is_ascii_digit())
            || c.chars().all(|ch| ch.is_ascii_alphabetic()) && !c.is_empty();
        if ok && !codes.contains(&c.to_string()) {
            codes.push(c.to_string());
        }
    }
    let total = codes.len();
    let missing = db::missing_stock_profiles(&codes).map_err(|e| e.to_string())?;
    let needed = missing.len();
    let mut fetched = 0usize;
    let mut failed_codes: Vec<String> = Vec::new();
    let mut consecutive_fail = 0usize;
    for code in &missing {
        match data::fetch_stock_industry(code) {
            Some((name, industry_em, market)) => {
                let l1 = lookthrough::sector_l1_of(&industry_em).to_string();
                let _ = db::upsert_stock_profile(code, &name, &industry_em, &l1, &market, "eastmoney_push2");
                fetched += 1;
                consecutive_fail = 0;
            }
            None => {
                failed_codes.push(code.clone());
                consecutive_fail += 1;
                // 失败退避：与 refresh_official_nav 同款（连续 5 只失败暂停 3s，防接口拒绝）
                if consecutive_fail >= 5 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    consecutive_fail = 0;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
            }
        }
    }
    Ok(FetchStockProfilesOut {
        total,
        needed,
        fetched,
        failed: failed_codes.len(),
        failed_codes,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

/// 构建基金穿透输入向量（消除 lookthrough_overlap / lookthrough_fund / lookthrough_overlap_detail
/// 的三重重复）。口径与 P0 红线一致：市值=份额×官方净值（兜底用持仓金额）；
/// 货基 002/005 与金额兜底整只未穿透，不产生个股行。
fn lookthrough_funds(platform: Option<&str>) -> Result<Vec<lookthrough::LtFundInput>, String> {
    let mut holdings = db::list_holdings(None).map_err(|e| e.to_string())?;
    if let Some(p) = platform {
        holdings.retain(|h| &h.platform == p);
    }
    let mut disclosures_by_fund: HashMap<String, Vec<valuation::DisclosedHolding>> = HashMap::new();
    for (fc, dh) in db::list_disclosures_batch().unwrap_or_default() {
        disclosures_by_fund.entry(fc).or_default().push(dh);
    }
    // v2.5 门禁需要 track_index（库里真实跟踪指数符号，可能为空）；从 funds 表建 code→track_index 索引。
    let track_by_code: HashMap<String, String> = db::list_funds()
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.code, f.track_index))
        .collect();
    let funds: Vec<lookthrough::LtFundInput> = holdings
        .iter()
        .map(|h| {
            let eff_shares = if h.shares > 0.0 {
                h.shares
            } else if h.holding_amount > 0.0 && h.official_nav > 0.0 {
                h.holding_amount / h.official_nav
            } else {
                0.0
            };
            let is_money_or_wealth = matches!(h.fund_type.as_str(), "002" | "005");
            let has_real_code = h.code.len() == 6 && h.code.chars().all(|c| c.is_ascii_digit()) && eff_shares > 0.0;
            let market_value = if eff_shares > 0.0 && h.official_nav > 0.0 {
                eff_shares * h.official_nav
            } else {
                h.holding_amount
            };
            let holdings_vec = disclosures_by_fund.get(&h.code).cloned().unwrap_or_default();
            lookthrough::LtFundInput {
                code: h.code.clone(),
                name: h.name.clone(),
                market_value,
                is_money_or_wealth,
                has_real_code,
                report_period: holdings_vec.first().map(|d| d.report_period.clone()),
                holdings: holdings_vec,
                fund_type: h.fund_type.clone(),
                track_index: track_by_code.get(&h.code).cloned().unwrap_or_default(),
            }
        })
        .collect();
    Ok(funds)
}

/// v2.5：从 db 装载全部指数成分表为 constituents map（index_code → rows），供聚合纯函数穿透使用。
fn load_index_constituents_map() -> HashMap<String, Vec<(String, String, f64)>> {
    let mut map: HashMap<String, Vec<(String, String, f64)>> = HashMap::new();
    if let Ok(codes) = db::list_index_codes() {
        for c in codes {
            if let Ok(rows) = db::list_index_constituents(&c) {
                if !rows.is_empty() {
                    map.insert(c, rows);
                }
            }
        }
    }
    map
}

/// P1：基金两两重合矩阵（识别「伪分散」）。双口径：权重重合 Σmin(wᵢ,wⱼ) + top10 Jaccard。
/// 只读分析，不发任何网络请求（纯 DB 聚合），毫秒级返回。
#[tauri::command]
pub fn lookthrough_overlap(platform: Option<String>) -> Result<lookthrough::OverlapResult, String> {
    let funds = lookthrough_funds(platform.as_deref())?;
    Ok(lookthrough::overlap_matrix(
        &funds,
        &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    ))
}

/// P1：单基金穿透（FundDetailPage 卡片）。分母 = 该基金市值，口径与组合穿透一致。
#[tauri::command]
pub fn lookthrough_fund(code: String) -> Result<lookthrough::FundLookthroughResult, String> {
    let funds = lookthrough_funds(None)?;
    let fund = funds
        .into_iter()
        .find(|f| f.code == code)
        .ok_or_else(|| format!("未找到基金 {code} 的持仓记录"))?;
    let profiles = db::list_stock_profiles().unwrap_or_default();
    let constituents = load_index_constituents_map();
    Ok(lookthrough::fund_lookthrough(
        &fund,
        &profiles,
        &constituents,
        &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    ))
}

/// P2：重合矩阵钻取（点击矩阵单元格下钻）。返回两基金共同持仓明细；
/// 任一基金不存在/货基/无披露，或两 code 相同 → 返回带基金名的错误字符串。
#[tauri::command]
pub fn lookthrough_overlap_detail(code_a: String, code_b: String) -> Result<lookthrough::OverlapDetail, String> {
    let funds = lookthrough_funds(None)?;
    let name_a = funds.iter().find(|f| f.code == code_a).map(|f| f.name.clone()).unwrap_or_default();
    let name_b = funds.iter().find(|f| f.code == code_b).map(|f| f.name.clone()).unwrap_or_default();
    match lookthrough::overlap_detail(&funds, &code_a, &code_b) {
        Some(d) => Ok(d),
        None => Err(format!(
            "无法钻取重合明细：基金 {} ({}) 或 {} ({}) 不存在、为货基/金额兜底/无披露，或两基金相同",
            code_a, name_a, code_b, name_b
        )),
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchStockStyleOut {
    /// 需要风格估值的 A 股股票总数（全部披露股票去重后，仅 6 位数字）
    pub total: usize,
    /// 本次缺失/过期需补拉数
    pub needed: usize,
    /// 成功抓取并入库数
    pub fetched: usize,
    pub failed: usize,
    pub failed_codes: Vec<String>,
    pub at: String,
}

/// 批量补股票风格估值（总市值/动态PE/市净率）。只拉 A 股 6 位数字代码（其余市场跳过），
/// 走既有全局出站节流（throttle_wait）；单只失败计数退避，绝不阻塞整体。
#[tauri::command]
pub fn refresh_stock_style() -> Result<FetchStockStyleOut, String> {
    // 收集全部披露股票中 A 股 6 位数字代码（去重）
    let mut codes: Vec<String> = Vec::new();
    for (_, hs) in db::list_disclosures_batch().unwrap_or_default() {
        let c = hs.stock_code.trim();
        if c.len() == 6 && c.chars().all(|ch| ch.is_ascii_digit()) && !codes.contains(&c.to_string()) {
            codes.push(c.to_string());
        }
    }
    let total = codes.len();
    let missing = db::missing_stock_style(&codes).map_err(|e| e.to_string())?;
    let needed = missing.len();
    let mut fetched = 0usize;
    let mut failed_codes: Vec<String> = Vec::new();
    let mut consecutive_fail = 0usize;
    for code in &missing {
        match data::fetch_stock_style(code) {
            Some((name, total_mv, pe_ttm, pb)) => {
                let _ = db::upsert_stock_style(code, &name, total_mv, pe_ttm, pb);
                fetched += 1;
                consecutive_fail = 0;
            }
            None => {
                failed_codes.push(code.clone());
                consecutive_fail += 1;
                // 失败退避：与 refresh_official_nav / fetch_stock_profiles 同款
                if consecutive_fail >= 5 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    consecutive_fail = 0;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
            }
        }
    }
    Ok(FetchStockStyleOut {
        total,
        needed,
        fetched,
        failed: failed_codes.len(),
        failed_codes,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

/// P2：风格箱九宫格（只读聚合，毫秒级，无网络）。先补拉 refresh_stock_style 才有风格快照。
#[tauri::command]
pub fn lookthrough_style(platform: Option<String>) -> Result<lookthrough::StyleBoxResult, String> {
    let funds = lookthrough_funds(platform.as_deref())?;
    let styles = db::list_stock_style().unwrap_or_default();
    let as_of = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    Ok(lookthrough::style_box(&funds, &styles, &as_of))
}

/// v2.5 指数成分穿透：刷新指数成分表（refresh_index_constituents）。
/// 收集持仓中的纯被动指数基金 → 解析基准指数码 → 过滤缺失者 → compose + replace。
/// 与 refresh_stock_style 同款节流与失败退避；网络失败安全，不阻塞整体。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshIndexConstituentsOut {
    /// 命中门禁的基准指数码总数（去重；含已存在者）
    pub total_target_codes: usize,
    /// 本次实际刷新入库的 (指数码, 成分数, 样本日)
    pub refreshed_codes: Vec<(String, usize, String)>,
    /// 失败（网络/解析）的指数码
    pub failed_codes: Vec<String>,
    pub at: String,
}

#[tauri::command]
pub fn refresh_index_constituents() -> Result<RefreshIndexConstituentsOut, String> {
    // 持仓基金码（去重）
    let holdings = db::list_holdings(None).map_err(|e| e.to_string())?;
    let mut fund_codes: Vec<String> = Vec::new();
    for h in &holdings {
        if !fund_codes.contains(&h.code) {
            fund_codes.push(h.code.clone());
        }
    }
    // code → (name, fund_type, track_index)
    let meta: HashMap<String, (String, String, String)> = db::list_funds()
        .unwrap_or_default()
        .into_iter()
        .map(|f| (f.code.clone(), (f.name, f.fund_type, f.track_index)))
        .collect();

    // 收集目标指数码：门禁（纯被动 + 非排除 token + 解析出非 hk/空 数字码），去重
    let mut target_codes: Vec<String> = Vec::new();
    for fc in &fund_codes {
        let (name, ftype, track) = match meta.get(fc) {
            Some(m) => m,
            None => continue,
        };
        if !data::is_pure_index_fund(ftype, name) {
            continue;
        }
        let low = name.to_lowercase();
        // 排除 token：商品/海外/港股/红利/低波（市值加权近似失真或非境内股票市值指数）
        const EXCLUDE: &[&str] = &[
            "黄金", "金etf", "原油", "期货", "豆粕", "白银", "商品", "reit", "纳斯达克", "纳指",
            "标普", "美国", "全球", "海外", "香港", "港股", "恒生", "沪港深", "红利", "低波",
        ];
        if EXCLUDE.iter().any(|t| low.contains(t)) {
            continue;
        }
        let resolved = match data::resolve_tracked_index(ftype, name, track) {
            Some(r) => r,
            None => continue,
        };
        let code = resolved.1; // 纯数字码
        if code.is_empty() || code.starts_with("hk") || code.starts_with("105") || code.starts_with("116") {
            continue;
        }
        if !target_codes.contains(&code) {
            target_codes.push(code);
        }
    }
    let total_target_codes = target_codes.len();

    // 仅刷新缺失成分表者
    let missing = db::missing_index_constituents(&target_codes).unwrap_or_else(|_| target_codes.clone());
    let mut refreshed_codes: Vec<(String, usize, String)> = Vec::new();
    let mut failed_codes: Vec<String> = Vec::new();
    let mut consecutive_fail = 0usize;
    for code in &missing {
        // compose（fetch 名单+流通市值）失败 → 退避；replace 入库失败同样退避
        let composed = match data::compose_index_constituents(code) {
            Some(c) => c,
            None => {
                failed_codes.push(code.clone());
                consecutive_fail += 1;
                if consecutive_fail >= 5 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    consecutive_fail = 0;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
                continue;
            }
        };
        match db::replace_index_constituents(code, &composed.1, &composed.0) {
            Ok(n) => {
                refreshed_codes.push((code.clone(), n, composed.0));
                consecutive_fail = 0;
            }
            Err(_) => {
                failed_codes.push(code.clone());
                consecutive_fail += 1;
                if consecutive_fail >= 5 {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    consecutive_fail = 0;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                }
            }
        }
    }
    Ok(RefreshIndexConstituentsOut {
        total_target_codes,
        refreshed_codes,
        failed_codes,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

// ===================== 披露持仓：历史期次 & 较上期变化 =====================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchHistoryOut {
    pub code: String,
    /// 实际尝试补录的期次数
    pub attempted: usize,
    /// 成功入库的期次（从新到旧；与 storedRows 一一对应）
    pub stored_periods: Vec<String>,
    pub stored_rows: Vec<usize>,
    /// 该基金当前已入库的全部期次（从旧到新）
    pub all_periods: Vec<String>,
    pub at: String,
}

/// 补录某基金的历史披露持仓：按候选期次从新到旧尝试 `quarters` 期，逐期入库。
///
/// 存在必要性：东财接口每次只返回「最新一期」，光放开存储历史也不会凭空出现——
/// 必须主动按 (年, 季) 逐期抓取，才能让「较上期」对比立刻有数据。
#[tauri::command]
pub fn fetch_disclosure_history(
    code: String,
    quarters: Option<usize>,
) -> Result<FetchHistoryOut, String> {
    let n = quarters.unwrap_or(8).clamp(1, 12);
    let now = chrono::Local::now().naive_local().date();
    let cands = data::candidate_periods(now);
    let cands: Vec<(i32, u32, &str)> = cands.into_iter().take(n).collect();
    let attempted = cands.len();
    let mut stored_periods: Vec<String> = Vec::new();
    let mut stored_rows: Vec<usize> = Vec::new();
    for (year, season, dtype) in cands {
        if let Some((period, _dt, holdings)) = data::fetch_disclosure_at(&code, year, season, dtype) {
            let cnt = holdings.len();
            db::replace_disclosure_period(&code, &period, &holdings).map_err(|e| e.to_string())?;
            // 东财偶发返回最新期表格，导致不同请求落到同一期次；去重后只记录一次
            if !stored_periods.contains(&period) {
                stored_periods.push(period);
                stored_rows.push(cnt);
            }
        }
        // 礼貌间隔，降低被东财接口限流的概率
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    let all_periods = db::list_disclosure_periods(&code).map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(FetchHistoryOut {
        code,
        attempted,
        stored_periods,
        stored_rows,
        all_periods,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingChangeOut {
    pub stock_code: String,
    pub stock_name: String,
    /// 本期占净值 0~1；本期已无此股为 null
    pub curr_weight: Option<f64>,
    /// 上期占净值 0~1；上期无此股为 null
    pub prev_weight: Option<f64>,
    /// 变化量 = 本期 − 上期
    pub delta: f64,
    /// new / exit / increase / decrease / flat
    pub change_type: String,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingChangesOut {
    pub code: String,
    /// 本期期次（最新）；无披露时为空串
    pub curr_period: String,
    /// 上期期次；无历史可比时为空串
    pub prev_period: String,
    /// 是否已存在上期（false 时界面应显示「暂无可对比的上期」）
    pub has_prev: bool,
    pub changes: Vec<HoldingChangeOut>,
}

/// 计算某基金「本期 vs 上期」的持仓变化：新增 / 退出 / 加仓 / 减仓 / 持平。
/// 仅做展示，不参与估值——估值始终只用最新一期（见 db::list_disclosures）。
#[tauri::command]
pub fn get_holding_changes(code: String) -> Result<HoldingChangesOut, String> {
    let periods = db::list_disclosure_periods(&code).map_err(|e| e.to_string())?;
    if periods.is_empty() {
        return Ok(HoldingChangesOut {
            code,
            curr_period: String::new(),
            prev_period: String::new(),
            has_prev: false,
            changes: Vec::new(),
        });
    }
    // list_disclosure_periods 已按从旧到新排序
    let curr_period = periods.last().cloned().unwrap_or_default();
    let prev_period = if periods.len() >= 2 {
        periods[periods.len() - 2].clone()
    } else {
        String::new()
    };
    let curr = db::list_disclosures_of_period(&code, &curr_period).map_err(|e| e.to_string())?;
    let prev: std::collections::HashMap<String, (String, f64)> = if prev_period.is_empty() {
        std::collections::HashMap::new()
    } else {
        db::list_disclosures_of_period(&code, &prev_period)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|h| (h.stock_code.clone(), (h.stock_name, h.weight)))
            .collect()
    };

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changes: Vec<HoldingChangeOut> = Vec::new();
    for h in &curr {
        seen.insert(h.stock_code.clone());
        let pw = prev.get(&h.stock_code).map(|(_, w)| *w);
        let delta = h.weight - pw.unwrap_or(0.0);
        let change_type = match pw {
            None => "new",
            Some(p) if (h.weight - p).abs() < 1e-9 => "flat",
            Some(p) if h.weight > p => "increase",
            _ => "decrease",
        };
        changes.push(HoldingChangeOut {
            stock_code: h.stock_code.clone(),
            stock_name: h.stock_name.clone(),
            curr_weight: Some(h.weight),
            prev_weight: pw,
            delta,
            change_type: change_type.to_string(),
        });
    }
    // 上期有、本期无 → 退出
    for (sc, (sn, w)) in prev {
        if seen.contains(&sc) {
            continue;
        }
        changes.push(HoldingChangeOut {
            stock_code: sc,
            stock_name: sn,
            curr_weight: None,
            prev_weight: Some(w),
            delta: -w,
            change_type: "exit".to_string(),
        });
    }
    // 排序：新增 → 加仓 → 减仓 → 退出 → 持平，同组内按变化绝对值降序
    changes.sort_by(|a, b| {
        let rank = |c: &HoldingChangeOut| match c.change_type.as_str() {
            "new" => 0,
            "increase" => 1,
            "decrease" => 2,
            "exit" => 3,
            _ => 4,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| {
                b.delta
                    .abs()
                    .partial_cmp(&a.delta.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    let has_prev = !prev_period.is_empty();
    Ok(HoldingChangesOut {
        code,
        curr_period,
        prev_period,
        has_prev,
        changes,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchAllDisclosuresOut {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
    pub failed_codes: Vec<String>,
    pub at: String,
}

#[tauri::command]
pub fn fetch_quotes(codes: Vec<String>) -> Result<usize, String> {
    let quotes = data::fetch_quotes(&codes).ok_or("拉取行情失败")?;
    for q in quotes.values() {
        let _ = db::upsert_quote(&q.stock_code, q.price, q.prev_close);
    }
    Ok(quotes.len())
}

// ===================== 净值走势 / 成本走势 =====================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavPointOut {
    pub date: String,
    pub nav: f64,
    pub acc_nav: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostPointOut {
    pub date: String,
    pub cumulative_cost: f64,
    pub unit_cost: f64,
    pub shares: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxnMarkerOut {
    pub date: String,
    pub txn_type: String, // buy / sell / dividend
    pub shares: f64,
    pub amount: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FundSeriesOut {
    pub nav_points: Vec<NavPointOut>,
    pub cost_points: Vec<CostPointOut>,
    pub txn_markers: Vec<TxnMarkerOut>,
    pub range: String,
}

/// 拉取并缓存某基金的历史净值（全量）。返回写入/更新的记录数。
/// 失败安全：网络异常/解析失败时，若本地已有历史净值则降级成功（走势仍可展示本地数据），
/// 仅本地完全无数据才返回 Err（调用方提示用户）。
#[tauri::command]
pub fn refresh_nav_history(code: String) -> Result<usize, String> {
    match data::fetch_nav_history(&code, 0) {
        Some(points) => {
            let n = db::upsert_nav_history(&code, &points).map_err(|e| e.to_string())?;
            // 历史净值到位后回填「待净值」交易流水；【v9】回填内部已把增量应用到持仓，不再全量重放。
            let _ = db::backfill_pending_txn_shares(1);
            invalidate_caches();
            Ok(n)
        }
        None => {
            // 网络失败降级：本地已有历史则视为成功（0 条新写入），不报错打扰
            let existing = db::get_nav_history(&code).map(|v| v.len()).unwrap_or(0);
            if existing > 0 {
                Ok(0)
            } else {
                Err("拉取历史净值失败（网络或接口异常）".to_string())
            }
        }
    }
}

/// 计算区间截止日（用于服务端按 range 过滤）。1m/3m/6m 返回 cutoff 日期；all 返回 None（不过滤）。
fn range_cutoff(range: &str) -> Option<String> {
    let months = match range {
        "1m" => 1,
        "3m" => 3,
        "6m" => 6,
        _ => return None,
    };
    let today = chrono::Local::now().naive_local().date();
    let cut = today - chrono::Duration::days((months as i64) * 30);
    Some(cut.format("%Y-%m-%d").to_string())
}

/// 聚合「净值走势 + 持仓成本走势 + 买卖/分红标记」，按 range 过滤后一次性返回前端。
/// range: "1m" / "3m" / "6m" / "all"。单机单账户固定 account_id = 1 取成本/标记。
#[tauri::command]
pub fn get_fund_series(code: String, range: String) -> Result<FundSeriesOut, String> {
    let acc = 1i64;
    let cutoff = range_cutoff(&range);
    let pass = |d: &str| cutoff.as_ref().map_or(true, |c| d >= c.as_str());

    let navs = db::get_nav_history(&code).map_err(|e| e.to_string())?;
    let mut nav_points: Vec<NavPointOut> = navs
        .into_iter()
        .filter(|n| pass(&n.date))
        .map(|n| NavPointOut { date: n.date, nav: n.nav, acc_nav: n.acc_nav })
        .collect();

    // 本地历史为空时，用 funds 表官方净值兜底：有昨收基准则补 2 点（昨收→今收，可画短线），
    // 否则补 1 点（前端配合 dot 显示单点 + 提示）。已有净值记录即可展示走势，不必联网拉取历史
    // （2026-08-25 用户反馈「已有净值记录却显示无数据」；单点画不出线 → 2026-08-26 补 2 点）。
    if nav_points.is_empty() {
        if let Ok(holdings) = db::list_holdings(None) {
            if let Some(h) = holdings.into_iter().find(|h| h.code == code) {
                if h.official_nav > 0.0 && !h.nav_date.is_empty() {
                    let d = h.nav_date.clone();
                    if pass(&d) {
                        if h.prev_nav > 0.0 {
                            if let Ok(pd) = chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d") {
                                let pd_s = (pd - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
                                if pass(&pd_s) {
                                    nav_points.push(NavPointOut { date: pd_s, nav: h.prev_nav, acc_nav: 0.0 });
                                }
                            }
                        }
                        nav_points.push(NavPointOut { date: d, nav: h.official_nav, acc_nav: 0.0 });
                    }
                }
            }
        }
    }

    // 【v9】成本线改读「权威持仓」画「当前持仓均价」水平参考线（横跨可见净值窗口的首末两端），
    // 不再由流水重放产出历史成本台阶——流水≠持仓镜像（如 FundVal 导入）时重放曲线会与
    // 实际持仓成本不一致而误导。无持仓或无净值点时不下发成本点。
    let mut cost_points: Vec<CostPointOut> = Vec::new();
    if let Some((shares, basis)) = db::get_position_basis(&code, acc).map_err(|e| e.to_string())? {
        if shares > 0.0 && nav_points.len() >= 1 {
            let unit = basis / shares;
            let mut min_d = nav_points[0].date.clone();
            let mut max_d = nav_points[0].date.clone();
            for n in &nav_points {
                if n.date < min_d {
                    min_d = n.date.clone();
                }
                if n.date > max_d {
                    max_d = n.date.clone();
                }
            }
            cost_points.push(CostPointOut { date: min_d.clone(), cumulative_cost: basis, unit_cost: unit, shares });
            if max_d != min_d {
                cost_points.push(CostPointOut { date: max_d, cumulative_cost: basis, unit_cost: unit, shares });
            }
        }
    }

    let markers = db::get_txn_markers(&code, acc).map_err(|e| e.to_string())?;
    let txn_markers: Vec<TxnMarkerOut> = markers
        .into_iter()
        .filter(|m| pass(&m.date))
        .map(|m| TxnMarkerOut {
            date: m.date,
            txn_type: m.txn_type,
            shares: m.shares,
            amount: m.amount,
        })
        .collect();

    Ok(FundSeriesOut { nav_points, cost_points, txn_markers, range })
}

// ===================== 交易流水 / 报表（单机单账户，账户维度不再暴露给前端） =====================

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TransactionOut {
    pub id: i64,
    pub account_id: i64,
    pub txn_type: String,
    pub fund_code: Option<String>,
    pub fund_name: Option<String>,
    pub shares: Option<f64>,
    pub amount: f64,
    pub price: Option<f64>,
    pub txn_date: String,
    pub txn_time: String,
    pub note: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
}

#[tauri::command]
pub fn list_transactions(
    fund_code: Option<String>,
) -> Result<Vec<TransactionOut>, String> {
    // 单机单账户固定 account_id = 1
    let txns = db::list_transactions(Some(1), fund_code).map_err(|e| e.to_string())?;
    // 附带基金名称（便于 UI 展示）
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let name_map: HashMap<String, String> =
        funds.into_iter().map(|f| (f.code, f.name)).collect();
    Ok(txns
        .into_iter()
        .map(|t| TransactionOut {
            id: t.id,
            account_id: t.account_id,
            txn_type: t.txn_type,
            fund_code: t.fund_code.clone(),
            fund_name: t.fund_code.as_ref().and_then(|c| name_map.get(c).cloned()),
            shares: t.shares,
            amount: t.amount,
            price: t.price,
            txn_date: t.txn_date,
            txn_time: t.txn_time,
            note: t.note,
            source: t.source,
            source_ref: t.source_ref,
        })
        .collect())
}

#[tauri::command]
pub fn add_transaction(
    txn_type: String,
    fund_code: Option<String>,
    shares: Option<f64>,
    amount: f64,
    price: Option<f64>,
    txn_date: String,
    txn_time: Option<String>,
    note: Option<String>,
    platform: Option<String>,
) -> Result<i64, String> {
    // 单机单账户固定 account_id = 1
    let id = db::add_transaction(
        1,
        &txn_type,
        fund_code,
        shares,
        amount,
        price,
        &txn_date,
        &txn_time.unwrap_or_default(),
        note,
        &platform.unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(id)
}

#[tauri::command]
pub fn delete_transaction(id: i64) -> Result<(), String> {
    db::delete_transaction(id).map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportTxnIn {
    pub fund_code: String,
    pub fund_name: Option<String>,
    pub txn_type: String,
    pub shares: Option<f64>,
    pub amount: f64,
    pub price: Option<f64>,
    pub txn_date: String,
    pub txn_time: Option<String>,
    pub note: Option<String>,
    /// 整批交易所属平台（交易截图导入时已知），用于补全 funds.platform
    pub platform: Option<String>,
}

/// 增量导入交易记录（买入/卖出/分红）。source_ref 标识导入批次：
/// 提供则与已有同批次(import_txn)幂等替换，避免叠加；不提供则纯追加。
/// 同一基金的截图/手动基线会被移除，真实流水成为持仓成本的唯一真相。
#[tauri::command]
pub fn import_transactions(
    items: Vec<ImportTxnIn>,
    source_ref: Option<String>,
    platform: Option<String>,
) -> Result<usize, String> {
    let platform_norm = platform.clone().unwrap_or_default();
    let mut norm: Vec<db::ImportTxn> = Vec::with_capacity(items.len());
    for it in &items {
        let t = match it.txn_type.trim().to_lowercase().as_str() {
            "buy" | "买入" | "申购" | "买进" => "buy",
            "sell" | "卖出" | "赎回" => "sell",
            "dividend" | "分红" | "现金分红" => "dividend",
            other => return Err(format!("不支持的交易类型：{}", other)),
        };
        norm.push(db::ImportTxn {
            fund_code: it.fund_code.trim().to_string(),
            fund_name: it.fund_name.as_ref().map(|s| s.trim().to_string()),
            txn_type: t.to_string(),
            shares: it.shares,
            amount: it.amount,
            price: it.price,
            txn_date: it.txn_date.trim().to_string(),
            txn_time: it.txn_time.as_ref().map(|s| s.trim().to_string()).unwrap_or_default(),
            note: it.note.as_ref().map(|s| s.trim().to_string()),
            platform: platform_norm.clone(),
        });
    }
    // 单机单账户固定 account_id = 1
    let n = db::import_transactions(1, &norm, source_ref).map_err(|e| e.to_string())?;
    invalidate_caches();
    Ok(n)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPoint {
    pub date: String,
    pub total_market_value: f64,
    pub total_cost: f64,
    pub total_pnl: f64,
    pub day_pnl: f64,
    /// 当日估算收益（快照日盘中估算投影；历史快照缺省为 0）
    pub day_pnl_est: f64,
    /// 当日估算市值（按估算净值口径；历史快照缺省为 0）
    pub est_market_value: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoverOut {
    pub code: String,
    pub name: String,
    pub total_pnl: f64,
    pub total_pnl_pct: f64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeriodReportOut {
    /// 报告周期：daily / weekly / monthly / yearly（四种报告共用同一结构，可直接对比）
    pub period: String,
    pub scope: String, // 账户名或「全部账户」
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub start_mv: f64,
    pub end_mv: f64,
    pub delta_mv: f64,
    pub delta_pnl: f64,
    pub pnl_rate: f64, // 区间收益率（相对期初成本）
    /// 区间估算收益累计（Σ 快照日当日估算收益；估算统计自启用起累积，旧数据为 0）
    pub est_delta_pnl: f64,
    /// 估算 − 实际偏差（est_delta_pnl − delta_pnl；>0 表示估算整体高估）
    pub est_act_diff: f64,
    /// 区间估算收益率（est_delta_pnl / 期初成本）
    pub est_pnl_rate: f64,
    /// 偏差率（est_act_diff / 期初成本）
    pub diff_rate: f64,
    pub positive_days: usize,
    pub negative_days: usize,
    /// 估算口径盈利天数（series 中 day_pnl_est > 0 的天数）
    pub est_positive_days: usize,
    /// 估算口径亏损天数（series 中 day_pnl_est < 0 的天数）
    pub est_negative_days: usize,
    pub series: Vec<SnapshotPoint>,
    pub best: Option<MoverOut>,
    pub worst: Option<MoverOut>,
    pub has_history: bool,
}

/// 取某账户（scope=0 全部）的快照序列，并定位「截至 end 之前、距 end 约 days 天」的期初快照。
fn load_report_snapshots(scope: i64) -> Vec<db::SnapshotRow> {
    db::list_snapshots(scope).unwrap_or_default()
}

fn build_period_report(scope: i64, scope_name: String, days: i64, period: &str) -> PeriodReportOut {
    let mut snaps = load_report_snapshots(scope);
    // P2-13：显式按日期升序排序——期初定位、日增量判定都依赖升序（相邻快照即相邻记录），
    // 不再隐式假设 list_snapshots 的返回顺序。
    snaps.sort_by(|a, b| a.snapshot_date.cmp(&b.snapshot_date));
    if snaps.len() < 2 {
        return PeriodReportOut {
            period: period.to_string(),
            scope: scope_name,
            start_date: snaps.last().map(|s| s.snapshot_date.clone()),
            end_date: snaps.last().map(|s| s.snapshot_date.clone()),
            start_mv: snaps.last().map(|s| s.total_market_value).unwrap_or(0.0),
            end_mv: snaps.last().map(|s| s.total_market_value).unwrap_or(0.0),
            delta_mv: 0.0,
            delta_pnl: 0.0,
            pnl_rate: 0.0,
            est_delta_pnl: 0.0,
            est_act_diff: 0.0,
            est_pnl_rate: 0.0,
            diff_rate: 0.0,
            positive_days: 0,
            negative_days: 0,
            est_positive_days: 0,
            est_negative_days: 0,
            series: snaps
                .iter()
                .map(|s| SnapshotPoint {
                    date: s.snapshot_date.clone(),
                    total_market_value: s.total_market_value,
                    total_cost: s.total_cost,
                    total_pnl: s.total_pnl,
                    day_pnl: s.day_pnl,
                    day_pnl_est: s.day_pnl_est,
                    est_market_value: s.est_market_value,
                })
                .collect(),
            best: None,
            worst: None,
            has_history: false,
        };
    }
    let end = snaps.last().unwrap();
    // 期初：日期 <= end_date - days 的最近一条快照（升序线性扫描，取最后一个满足条件的；无则取首条）。
    // P2-13：显式扫描替代旧 take_while——take_while 遇首个不满足即停、且依赖序列顺序，
    // 在稀疏/乱序快照下会漏选；此处与上方显式排序配合，定位稳定。
    let end_dt = chrono::NaiveDate::parse_from_str(&end.snapshot_date, "%Y-%m-%d").ok();
    let mut start_idx = 0usize;
    if let Some(ed) = end_dt {
        for (i, s) in snaps.iter().enumerate() {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&s.snapshot_date, "%Y-%m-%d") {
                if (ed - d).num_days() >= days {
                    start_idx = i;
                }
            }
        }
    }
    let start = &snaps[start_idx];
    let delta_mv = end.total_market_value - start.total_market_value;
    let delta_pnl = end.total_pnl - start.total_pnl;
    let pnl_rate = if start.total_cost > 1e-9 {
        delta_pnl / start.total_cost
    } else {
        0.0
    };
    // 区间序列（窗口 [start_idx..] 内全部快照点，与期初/期末定位一致）。
    let mut series: Vec<SnapshotPoint> = Vec::with_capacity(snaps.len() - start_idx);
    let mut positive_days = 0usize;
    let mut negative_days = 0usize;
    let mut est_positive_days = 0usize;
    let mut est_negative_days = 0usize;
    let mut est_delta_pnl: f64 = 0.0;
    // P1-6：实际侧统一用「窗口内快照 day_pnl 之和」（每条 day_pnl 落库时已按当日净现金流调整），
    // 与估算和同口径对比。旧实现用 delta_pnl（期末−期初累计盈亏）相减——出金/提现只降市值不降
    // 成本会让累计盈亏虚降，把现金流错当「估算误差」；改用单日实际和才是真正的估算 vs 实际差。
    // 断档日（多日未开 App 后的补快照）的 day_pnl 已在 record_daily_snapshot 改为「当日真实实际」
    // （P1-5），故此处直接求和不会把多日累计当成单日。
    let mut act_delta_pnl: f64 = 0.0;
    for s in snaps.iter().skip(start_idx) {
        series.push(SnapshotPoint {
            date: s.snapshot_date.clone(),
            total_market_value: s.total_market_value,
            total_cost: s.total_cost,
            total_pnl: s.total_pnl,
            day_pnl: s.day_pnl,
            day_pnl_est: s.day_pnl_est,
            est_market_value: s.est_market_value,
        });
        if s.day_pnl > 0.0 {
            positive_days += 1;
        } else if s.day_pnl < 0.0 {
            negative_days += 1;
        }
        if s.day_pnl_est > 0.0 {
            est_positive_days += 1;
        } else if s.day_pnl_est < 0.0 {
            est_negative_days += 1;
        }
        est_delta_pnl += s.day_pnl_est;
        act_delta_pnl += s.day_pnl;
    }
    let est_act_diff = est_delta_pnl - act_delta_pnl;
    let est_pnl_rate = if start.total_cost > 1e-9 {
        est_delta_pnl / start.total_cost
    } else {
        0.0
    };
    let diff_rate = if start.total_cost > 1e-9 {
        est_act_diff / start.total_cost
    } else {
        0.0
    };
    // 个基最佳/最差（按当前累计收益率），复用实时总览的持仓口径（报表固定全账户，传 None）。
    // v2.6.0 P-B：改走 cached_overview（与 get_overview 命令同一缓存入口）——报表与总览页数值恒一致，
    // 且打开报表页时 5~6 个并发周期不再各自触发行情网络风暴（single-flight 共享一次重算）。
    let ov = cached_overview(None).unwrap_or_else(|_| OverviewOut {
        summary: valuation::PortfolioSummary::default(),
        positions: vec![],
        trading: false,
        market_session: String::new(),
        as_of: String::new(),
    });
    let mut sorted: Vec<&PositionRowOut> = ov.positions.iter().collect();
    sorted.sort_by(|a, b| b.total_pnl_pct.partial_cmp(&a.total_pnl_pct).unwrap_or(std::cmp::Ordering::Equal));
    let best = sorted.first().map(|p| MoverOut {
        code: p.fund.code.clone(),
        name: p.fund.name.clone(),
        total_pnl: p.total_pnl,
        total_pnl_pct: p.total_pnl_pct,
    });
    let worst = sorted.last().map(|p| MoverOut {
        code: p.fund.code.clone(),
        name: p.fund.name.clone(),
        total_pnl: p.total_pnl,
        total_pnl_pct: p.total_pnl_pct,
    });
    PeriodReportOut {
        period: period.to_string(),
        scope: scope_name,
        start_date: Some(start.snapshot_date.clone()),
        end_date: Some(end.snapshot_date.clone()),
        start_mv: start.total_market_value,
        end_mv: end.total_market_value,
        delta_mv,
        delta_pnl,
        pnl_rate,
        est_delta_pnl,
        est_act_diff,
        est_pnl_rate,
        diff_rate,
        positive_days,
        negative_days,
        est_positive_days,
        est_negative_days,
        series,
        best,
        worst,
        has_history: true,
    }
}

#[tauri::command]
pub fn get_daily_report() -> Result<PeriodReportOut, String> {
    // 单机单账户，报表始终以 scope=0（全部账户聚合）生成；平台拆分属于后续增强。
    Ok(build_period_report(0, "全部账户".to_string(), 1, "daily"))
}

#[tauri::command]
pub fn get_weekly_report() -> Result<PeriodReportOut, String> {
    Ok(build_period_report(0, "全部账户".to_string(), 7, "weekly"))
}

#[tauri::command]
pub fn get_monthly_report() -> Result<PeriodReportOut, String> {
    Ok(build_period_report(0, "全部账户".to_string(), 30, "monthly"))
}

#[tauri::command]
pub fn get_yearly_report() -> Result<PeriodReportOut, String> {
    Ok(build_period_report(0, "全部账户".to_string(), 365, "yearly"))
}

#[tauri::command]
pub fn get_pnl_calendar(months: i64) -> Result<Vec<SnapshotPoint>, String> {
    let scope = 0i64;
    let snaps = load_report_snapshots(scope);
    let end_dt = chrono::Local::now().naive_local().date();
    let start_dt = end_dt - chrono::Duration::days((months * 30).max(1));
    Ok(snaps
        .into_iter()
        .filter_map(|s| {
            let d = chrono::NaiveDate::parse_from_str(&s.snapshot_date, "%Y-%m-%d").ok()?;
            if d >= start_dt && d <= end_dt {
                Some(SnapshotPoint {
                    date: s.snapshot_date,
                    total_market_value: s.total_market_value,
                    total_cost: s.total_cost,
                    total_pnl: s.total_pnl,
                    day_pnl: s.day_pnl,
                    day_pnl_est: s.day_pnl_est,
                    est_market_value: s.est_market_value,
                })
            } else {
                None
            }
        })
        .collect())
}

/// 区间操作收益：指定 [start, end]（YYYY-MM-DD），计算区间内买入/卖出操作的涨跌收益。
/// 口径见 crate::operation_pnl 模块头注释（交易口径、排除货基 002/005、末净值≤end 回退等）。
#[tauri::command]
pub fn get_operation_pnl(start_date: String, end_date: String) -> Result<crate::operation_pnl::OperationPnlOut, String> {
    crate::operation_pnl::build_operation_pnl(&start_date, &end_date)
}

// ---- 内部辅助 ----

/// 交叉验证置信度：比较穿透估值与平台实时估值的涨跌幅分歧。
/// 阈值（百分点绝对差）：<=0.3% 高 / <=0.8% 中 / 否则低；无平台估值则无法校验(none)。
/// 多源交叉验证：比较「平台实时估值」与「本地持仓穿透自算」两个独立源。
/// 返回 (置信度, 分歧百分点, 共识估值涨跌幅)。
/// 共识仅在两源均在且分歧不大时给出（平台权重略高，因其覆盖已披露+未披露全口径）；
/// 分歧过大（>0.8%）视为不可共识，返回 None，前端仍并列展示两源并标注低置信。
fn compute_confidence(penetration_pct: f64, platform_pct: Option<f64>) -> (String, f64, Option<f64>) {
    match platform_pct {
        None => ("none".to_string(), 0.0, None),
        Some(p) => {
            let d = (p - penetration_pct).abs();
            let c = if d <= 0.003 {
                "high"
            } else if d <= 0.008 {
                "medium"
            } else {
                "low"
            };
            let consensus = if d <= 0.008 {
                // 两源均在且分歧 ≤0.8%：平台 0.6 + 穿透 0.4 加权共识
                Some(p * 0.6 + penetration_pct * 0.4)
            } else {
                None
            };
            (c.to_string(), d, consensus)
        }
    }
}

/// 单只基金：拉取其披露持仓对应的个股行情（1 次网络请求）。
/// 供基金详情页 get_fund_detail 使用（单只，无需批量）。
fn fetch_quotes_for(
    disclosures: &[valuation::DisclosedHolding],
) -> HashMap<String, valuation::StockQuote> {
    if disclosures.is_empty() {
        return HashMap::new();
    }
    let codes: Vec<String> = disclosures
        .iter()
        .map(|d| to_quote_symbol(&d.stock_code))
        .collect();
    data::fetch_quotes(&codes).unwrap_or_default()
}

/// 从循环外已批量拉取的个股行情缓存中取数（O(1) 查找），避免循环内重复网络请求。
/// 返回的 map key 仍为纯数字代码，与 value_fund(valuation.rs:137) 的查询口径一致。
fn fetch_quotes_for_batch(
    disclosures: &[valuation::DisclosedHolding],
    cache: &HashMap<String, valuation::StockQuote>,
) -> HashMap<String, valuation::StockQuote> {
    let mut out = HashMap::new();
    for d in disclosures {
        // 与 data::fetch_quotes 内部一致：行情表 key 取纯数字代码（去前缀）
        let sym = to_quote_symbol(&d.stock_code);
        let digit: String = sym.chars().filter(|c| c.is_ascii_digit()).collect();
        if let Some(q) = cache.get(&digit) {
            out.insert(digit, q.clone());
        }
    }
    out
}

/// 将股票代码映射为腾讯 gtimg 行情请求符号，同时识别 A 股/港股/北交所/美股：
/// - 含字母（如 AAPL/TSLA）→ 美股 us 前缀（当前 fetch_quotes 主要覆盖 A/港股，美股会自然缺失行情，
///   权重归入基准近似，避免被错误地映射为 sz/sh）
/// - 6 位且以 4/8 开头（北交所/新三板）→ nq 前缀
/// - 5 位（如 00700）→ 港股 hk 前缀
/// - 6 位且以 6 开头（如 600519）→ 上交所 sh 前缀
/// - 6 位且以 0/3 开头（如 000568/300750）→ 深交所 sz 前缀
fn to_quote_symbol(stock_code: &str) -> String {
    let s = stock_code.trim();
    if s.is_empty() {
        return String::new();
    }
    if s.chars().any(|c| c.is_alphabetic()) {
        return format!("us{}", s.to_uppercase());
    }
    if s.len() == 6 && (s.starts_with('4') || s.starts_with('8')) {
        return format!("nq{}", s);
    }
    if s.len() == 5 {
        format!("hk{}", s)
    } else if s.starts_with('6') {
        format!("sh{}", s)
    } else {
        format!("sz{}", s)
    }
}

fn platform_name(code: &str) -> String {
    match code {
        "alipay" => "支付宝",
        "jd_finance" => "京东金融",
        "tencent_licai" => "腾讯理财通",
        _ => code,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 命令层单测复用 db 测试基础设施（唯一临时库 + 全局连接串行化），
    // 覆盖此前零覆盖的命令：write_text_file / export_db+import_db 往返 / update_position 平台解析。
    #[test]
    fn write_text_file_creates_parent_dirs_and_content() {
        let dir = std::env::temp_dir().join(format!("fundlens_wtf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("report.md");
        write_text_file(path.to_string_lossy().to_string(), "# hello\n".to_string()).unwrap();
        assert!(path.is_file(), "应自动创建父目录并写入文件");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "# hello\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_import_db_roundtrip_preserves_positions() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let acc = db::create_account("命令备份", "").unwrap();
        db::set_baseline(
            acc, "000777", 50.0, 500.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set",
        )
        .unwrap();

        let dest = std::env::temp_dir().join(format!("fundlens_cmd_backup_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        let info = export_db(dest.to_string_lossy().to_string()).unwrap();
        assert!(info.size > 0, "备份文件应非空");
        assert!(dest.is_file());

        // 破坏活动库数据后从备份恢复
        db::delete_fund("000777").unwrap();
        assert!(db::list_holdings(Some(acc)).unwrap().is_empty(), "删除后应为空");

        import_db(dest.to_string_lossy().to_string()).unwrap();
        let hs = db::list_holdings(Some(acc)).unwrap();
        assert_eq!(hs.len(), 1, "恢复后应重新出现 1 条持仓");
        assert!((hs[0].shares - 50.0).abs() < 1e-6);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn update_position_resolves_existing_platform_without_phantom() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        // 先在 alipay 平台建立基线
        db::set_baseline(
            1, "000888", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set",
        )
        .unwrap();
        // 省略平台改仓 → 应解析到既有 alipay 并更新，不产生 '' 幻影行
        update_position("000888".to_string(), 250.0, 2500.0, None).unwrap();
        let hs = db::list_holdings(None).unwrap();
        let alipay = hs
            .iter()
            .find(|h| h.platform == "alipay" && h.code == "000888")
            .expect("alipay 持仓缺失");
        assert!((alipay.shares - 250.0).abs() < 1e-6, "应在既有平台行上更新份额");
        let phantom = hs.iter().any(|h| h.platform == "" && h.code == "000888");
        assert!(!phantom, "不应产生空平台幻影行");
    }

    #[test]
    fn update_position_defaults_empty_platform_for_new_fund() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        // 全新基金且未指定平台 → 无既有持仓可解析，落到 '' 平台
        update_position("000999".to_string(), 10.0, 100.0, None).unwrap();
        let hs = db::list_holdings(None).unwrap();
        let h = hs.iter().find(|h| h.code == "000999").expect("应创建持仓");
        assert_eq!(h.platform, "", "全新基金默认落到空平台");
    }

    #[test]
    fn update_position_cost_changes_basis_without_new_record() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        // 【v9】基线 = 直写 positions（不产生流水）：100 份 × 成本价 10.0 → 持仓成本 1000
        db::set_baseline(1, "000777", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        assert!(db::list_transactions(None, None).unwrap().is_empty(), "基线不得产生流水");

        // 改成本价为 12.5 → 持仓成本 = 100 × 12.5 = 1250，份额不变，且不产生任何流水
        update_position_cost("000777".to_string(), 12.5, Some("alipay".to_string())).unwrap();

        let hs = db::list_holdings(None).unwrap();
        let h = hs
            .iter()
            .find(|h| h.code == "000777" && h.platform == "alipay")
            .expect("alipay 持仓缺失");
        assert!((h.shares - 100.0).abs() < 1e-6, "份额不应变化");
        assert!((h.cost_amount - 1250.0).abs() < 1e-6, "持仓成本 = 份额 × 新成本价，got {}", h.cost_amount);
        assert!(db::list_transactions(None, None).unwrap().is_empty(), "改成本不得产生任何流水/账本行");
    }

    #[test]
    fn update_position_cost_after_shares_edit_keeps_no_new_record() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        // 先改份额（update_position → 直写权威持仓，不产生流水），再改成本价
        db::set_baseline(1, "000888", 100.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        update_position("000888".to_string(), 200.0, 2200.0, Some("alipay".to_string())).unwrap();
        assert!(db::list_transactions(None, None).unwrap().is_empty(), "基线/改份额均不得产生流水");
        let cnt_before = 0usize;

        // 改成本价为 11.0 → 持仓成本 = 200 × 11.0 = 2200，不新增流水
        update_position_cost("000888".to_string(), 11.0, Some("alipay".to_string())).unwrap();

        let hs = db::list_holdings(None).unwrap();
        let h = hs
            .iter()
            .find(|h| h.code == "000888" && h.platform == "alipay")
            .expect("alipay 持仓缺失");
        assert!((h.shares - 200.0).abs() < 1e-6);
        assert!((h.cost_amount - 2200.0).abs() < 1e-6, "got {}", h.cost_amount);
        let after = db::list_transactions(None, None).unwrap();
        assert_eq!(after.len(), cnt_before, "不得新增交易/盘点记录");
    }

    /// 四种周期报告共用 build_period_report：估算收益/偏差必须与 delta_pnl 同窗口（start..end），
    /// 且 series 中早于期初的展示点不得计入估算累计。
    /// 注意：build_period_report 内部会经 get_overview 落「今日」快照，故每个周期断言独立重建临时库，
    /// 避免后续调用的 end 被今日快照污染。
    #[test]
    fn period_report_est_stats_match_window_daily() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let acc = 0i64;
        // 3 天快照（含估算列）：市值 1000→1100→1080，当日估算收益 60/50/−30
        db::record_snapshot(acc, "2026-08-18", "", 1000.0, 900.0, 100.0, 10.0, 0.0, 0.0, 60.0, 1060.0).unwrap();
        db::record_snapshot(acc, "2026-08-19", "", 1100.0, 900.0, 200.0, 90.0, 0.0, 0.0, 50.0, 1150.0).unwrap();
        db::record_snapshot(acc, "2026-08-20", "", 1080.0, 900.0, 180.0, -20.0, 0.0, 0.0, -30.0, 1050.0).unwrap();

        // 日报（days=1）：期初=08-19（最近 ≥1 天前），期末=08-20，窗口=08-19..08-20
        let daily = build_period_report(0, "全部账户".to_string(), 1, "daily");
        assert_eq!(daily.period, "daily");
        assert_eq!(daily.start_date.as_deref(), Some("2026-08-19"));
        assert_eq!(daily.end_date.as_deref(), Some("2026-08-20"));
        assert!((daily.delta_pnl - (-20.0)).abs() < 1e-9, "实际 = 期末180 − 期初200");
        // 估算累计 = 50 + (−30) = 20，必须排除 08-18 的 60（早于期初）
        assert!((daily.est_delta_pnl - 20.0).abs() < 1e-9, "估算累计应排除期初前点: {}", daily.est_delta_pnl);
        // 偏差（P1-6）＝ 估算和(20) − 单日实际和(90−20=70) = −50；
        // 不再用 delta_pnl(−20) 相减（累计口径含期初差异，会把期初点的 90 排除在外）。
        assert!((daily.est_act_diff - (-50.0)).abs() < 1e-9, "偏差 = 20 − 70，got {}", daily.est_act_diff);
        assert!((daily.est_pnl_rate - 20.0 / 900.0).abs() < 1e-9);
        assert!((daily.diff_rate - (-50.0) / 900.0).abs() < 1e-9);
        assert_eq!(daily.est_positive_days, 1, "窗口内估算收益为正的天数（50）");
        assert_eq!(daily.est_negative_days, 1, "窗口内估算收益为负的天数（−30）");
    }

    #[test]
    fn period_report_est_stats_match_window_weekly() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let acc = 0i64;
        db::record_snapshot(acc, "2026-08-18", "", 1000.0, 900.0, 100.0, 10.0, 0.0, 0.0, 60.0, 1060.0).unwrap();
        db::record_snapshot(acc, "2026-08-19", "", 1100.0, 900.0, 200.0, 90.0, 0.0, 0.0, 50.0, 1150.0).unwrap();
        db::record_snapshot(acc, "2026-08-20", "", 1080.0, 900.0, 180.0, -20.0, 0.0, 0.0, -30.0, 1050.0).unwrap();

        // 周报（days=7）：无 ≥7 天前快照 → 期初回退到最早快照 08-18，窗口=08-18..08-20
        let weekly = build_period_report(0, "全部账户".to_string(), 7, "weekly");
        assert_eq!(weekly.period, "weekly");
        assert_eq!(weekly.start_date.as_deref(), Some("2026-08-18"));
        assert_eq!(weekly.end_date.as_deref(), Some("2026-08-20"));
        assert!((weekly.delta_pnl - 80.0).abs() < 1e-9, "实际 = 180 − 100");
        assert!((weekly.est_delta_pnl - 80.0).abs() < 1e-9, "估算累计 = 60+50−30");
        assert!((weekly.est_act_diff - 0.0).abs() < 1e-9);
    }

    #[test]
    fn period_report_est_stats_match_window_yearly() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let acc = 0i64;
        db::record_snapshot(acc, "2026-08-18", "", 1000.0, 900.0, 100.0, 10.0, 0.0, 0.0, 60.0, 1060.0).unwrap();
        db::record_snapshot(acc, "2026-08-19", "", 1100.0, 900.0, 200.0, 90.0, 0.0, 0.0, 50.0, 1150.0).unwrap();
        db::record_snapshot(acc, "2026-08-20", "", 1080.0, 900.0, 180.0, -20.0, 0.0, 0.0, -30.0, 1050.0).unwrap();

        // 年报（days=365）：与周报同样回退到最早快照，period 标记为 yearly
        let yearly = build_period_report(0, "全部账户".to_string(), 365, "yearly");
        assert_eq!(yearly.period, "yearly");
        assert_eq!(yearly.start_date.as_deref(), Some("2026-08-18"));
        assert_eq!(yearly.end_date.as_deref(), Some("2026-08-20"));
        assert!((yearly.est_delta_pnl - 80.0).abs() < 1e-9);
    }

    #[test]
    fn to_quote_symbol_maps_exchanges_correctly() {
        assert_eq!(to_quote_symbol("00700"), "hk00700");
        assert_eq!(to_quote_symbol("600519"), "sh600519");
        assert_eq!(to_quote_symbol("000568"), "sz000568");
        assert_eq!(to_quote_symbol("300750"), "sz300750");
        // 北交所/新三板为 8 位代码
        assert_eq!(to_quote_symbol("835305"), "nq835305");
        assert_eq!(to_quote_symbol("430418"), "nq430418");
        assert_eq!(to_quote_symbol("AAPL"), "usAAPL");
        assert_eq!(to_quote_symbol("tsla"), "usTSLA");
        assert_eq!(to_quote_symbol(""), "");
    }

    /// 基金详情页应优先使用 funds.prev_nav 真实昨日净值，而不是被 est_cache 污染的错误基准。
    /// 修复前 est_cache.prev_nav=7.2429 会覆盖真实 prev_nav=7.5438，导致 006503 当日实际收益/涨跌幅错误。
    #[test]
    fn get_fund_detail_prefers_fund_prev_nav_over_polluted_est_cache() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let acc = db::create_account("详情页基准测试", "").unwrap();
        let code = "006503";
        let shares = 94.0730;
        let cost = 600.0;
        // 建立持仓（会同时创建 funds 占位记录）
        db::set_baseline(acc, code, shares, cost, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let official_nav = 6.8851;
        let real_prev_nav = 7.5438;
        let polluted_prev_nav = 7.2429;
        // 写入今日官方净值 + 真实昨日净值（模拟 refresh_official_nav 已回填）
        db::update_fund_nav(code, official_nav, "001", true, &today, Some(real_prev_nav)).unwrap();

        // 模拟被污染的旧 est_cache：prev_nav 是错误的 7.2429
        db::save_est_cache(&[(
            code.to_string(),
            crate::data::FundEstimate {
                est_nav: official_nav,
                est_change_pct: -0.0873,
                prev_nav: polluted_prev_nav,
                gztime: format!("{} 15:00", today),
            },
            chrono::Local::now().timestamp(),
        )])
        .unwrap();

        let detail = get_fund_detail(code.to_string()).unwrap();
        let p = detail.position;
        // 期望的当日实际收益 = shares * (official_nav - real_prev_nav)
        let expected_day_pnl = shares * (official_nav - real_prev_nav);
        // 期望的当日实际收益率 = (official_nav - real_prev_nav) / real_prev_nav ≈ -8.73%
        let expected_day_pnl_pct = (official_nav - real_prev_nav) / real_prev_nav;
        assert!(
            (p.day_pnl - expected_day_pnl).abs() < 1e-3,
            "当日收益应基于真实 prev_nav {} 计算，got {} (expected {})",
            real_prev_nav,
            p.day_pnl,
            expected_day_pnl
        );
        assert!(
            (p.day_pnl_pct - expected_day_pnl_pct).abs() < 1e-4,
            "当日收益率应基于真实 prev_nav {} 计算，got {} (expected {})",
            real_prev_nav,
            p.day_pnl_pct,
            expected_day_pnl_pct
        );
        // 确保没有回退到被污染的 est_cache 基准（ polluted 会算出 -4.94% 左右）
        let polluted_pct = (official_nav - polluted_prev_nav) / polluted_prev_nav;
        assert!(
            (p.day_pnl_pct - polluted_pct).abs() > 1e-4,
            "不应使用被污染的 est_cache prev_nav {} 计算", polluted_prev_nav
        );
    }

    /// 端到端复现（2026-09-04，P0-1/P0-2）：货币型基金总览页与明细页口径统一。
    /// 构造 type=002、official_nav=1.02、昨收 1.01 的份额型货基，并注入 holding_amount=1100
    /// （> 份额×净值 1020，用于区分修复前后：旧总览直接取 holding_amount=1100，新口径取份额×净值）。
    /// 期望（新统一口径，两页一致）：市值 = 1000×1.02 = 1020；盈亏 = 1020 − 1000 = 20；收益率 = 2%。
    #[test]
    fn e2e_money_fund_pages_unified() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        db::set_baseline(1, "000201", 1000.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set").unwrap();
        // 注入支付宝式「持仓金额」字段（> 份额×净值），若口径未统一，旧总览会取 1100 造成两页分裂。
        db::with_conn(|c| {
            c.execute(
                "UPDATE positions SET holding_amount=?1, holding_profit=?2 \
                 WHERE account_id=1 AND fund_code='000201' AND platform='alipay'",
                rusqlite::params![1100.0, 100.0],
            )?;
            Ok(())
        })
        .unwrap();
        // 类型=货币型 002，官方净值 1.02（昨收 1.01），估值不适用（净值恒定≈1）
        db::update_fund_nav("000201", 1.02, "002", false, "2026-09-03", Some(1.01)).unwrap();

        let ov = get_overview(None).unwrap();
        let pos = ov.positions.iter().find(|p| p.fund.code == "000201").expect("货基应在总览");
        assert!(
            (pos.market_value - 1020.0).abs() < 1e-6,
            "货基市值应=份额×净值 1020（无视 holding_amount=1100），got {}",
            pos.market_value
        );
        assert!((pos.total_pnl - 20.0).abs() < 1e-6, "got {}", pos.total_pnl);
        assert!((pos.total_pnl_pct - 20.0 / 1000.0).abs() < 1e-9, "got {}", pos.total_pnl_pct);
        assert!(!pos.estimated, "货基不参与浮动净值估算");

        let det = get_fund_detail("000201".to_string()).unwrap();
        assert!(
            (det.position.market_value - pos.market_value).abs() < 1e-6,
            "明细页市值必须与总览页一致：{} vs {}",
            det.position.market_value,
            pos.market_value
        );
        assert!((det.position.total_pnl - 20.0).abs() < 1e-6);
        assert!((det.position.total_pnl_pct - pos.total_pnl_pct).abs() < 1e-9);
        assert!((det.position.day_pnl_est).abs() < 1e-9, "货基当日估算恒 0");
    }

    /// 端到端复现（2026-09-04，P1-5）：快照断档 vs 连续日的 day_pnl 落库口径。
    /// - 断档（上一快照远早于上一交易日）：day_pnl 必须用「当日真实官方实际收益」act_day_pnl，
    ///   而不是「市值链差」（那是多日累计，会造成巨额单日假象）。
    /// - 连续（上一快照为上一交易日）：day_pnl = 市值 − 昨收快照市值 − 当日净现金流。
    /// 判定随运行日期自适应（用 trading_days_between 判断连续性），周末运行也不脆弱。
    #[test]
    fn e2e_snapshot_gap_day_pnl_uses_actual() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        // v2.6.0：清空总览/明细快照缓存，确保本测试基于刚重置的临时库重算（缓存为进程级全局，跨测试持久）。
        invalidate_caches();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let yesterday = (chrono::Local::now() - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();

        // ① 断档：上一快照停在 2026-08-28（远离今日，中间必隔多个交易日）→ 走 act_day_pnl
        db::record_snapshot(0, "2026-08-28", "", 5000.0, 4000.0, 1000.0, 0.0, 0.0, 0.0, 0.0, 5000.0).unwrap();
        record_daily_snapshot(0, 5500.0, 4100.0, 1400.0, 123.45, 88.0, 5588.0);
        let snaps = db::list_snapshots(0).unwrap();
        let gap = snaps.iter().find(|s| s.snapshot_date == today).expect("今日快照应存在");
        assert!(
            (gap.day_pnl - 123.45).abs() < 1e-9,
            "断档日 day_pnl 应=当日真实实际 123.45，而非多日链差 500，got {}",
            gap.day_pnl
        );
        assert!((gap.day_pnl_est - 88.0).abs() < 1e-9);

        // ② 连续：上一快照=昨日，today 覆盖同日期（record_snapshot UPSERT）
        let contiguous = valuation::trading_days_between(&yesterday, &today) == 1;
        let chain_expected = 5520.0 - 5400.0 - 0.0; // mv − 昨收快照 − 当日净现金流(无) = 120
        db::record_snapshot(0, &yesterday, "", 5400.0, 4000.0, 1400.0, 100.0, 0.0, 0.0, 50.0, 5450.0).unwrap();
        record_daily_snapshot(0, 5520.0, 4000.0, 1520.0, 999.0, 60.0, 5580.0);
        let snaps = db::list_snapshots(0).unwrap();
        let cont = snaps.iter().find(|s| s.snapshot_date == today).unwrap();
        let expect = if contiguous { chain_expected } else { 999.0 };
        assert!(
            (cont.day_pnl - expect).abs() < 1e-9,
            "连续性={} 时 day_pnl 应为 {}（连续走链差 120 / 否则走 act 999），got {}",
            contiguous,
            expect,
            cont.day_pnl
        );
        assert!((cont.day_pnl_est - 60.0).abs() < 1e-9);
    }

    // ===================== v2.6.0 P-A 缓存 seam 单测（纯逻辑，不触 DB/网络） =====================
    // 验证 cached_with 的可测 seam：同 key 窗口内 compute 仅执行一次；TTL 过期后重算；
    // invalidate 后重算；不同 key 不串扰。设计文档 §1.3 验收要求覆盖这四类断言。

    #[test]
    fn cached_with_computes_once_per_window() {
        let cache: std::sync::Mutex<Option<(std::time::Instant, String, i32)>> = std::sync::Mutex::new(None);
        let compute_lock: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let calls = std::cell::Cell::new(0);
        let ttl = std::time::Duration::from_secs(60);
        let a = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(42)
        })
        .unwrap();
        let b = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(42)
        })
        .unwrap();
        assert_eq!(a, 42);
        assert_eq!(b, 42);
        assert_eq!(calls.get(), 1, "窗口内同 key 只应计算一次");
    }

    #[test]
    fn cached_with_recomputes_after_ttl() {
        let cache: std::sync::Mutex<Option<(std::time::Instant, String, i32)>> = std::sync::Mutex::new(None);
        let compute_lock: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let calls = std::cell::Cell::new(0);
        let ttl = std::time::Duration::from_millis(1);
        let _ = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(1)
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        let _ = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(2)
        });
        assert_eq!(calls.get(), 2, "TTL 过期后应重算");
    }

    #[test]
    fn cached_with_recomputes_after_invalidate() {
        let cache: std::sync::Mutex<Option<(std::time::Instant, String, i32)>> = std::sync::Mutex::new(None);
        let compute_lock: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let calls = std::cell::Cell::new(0);
        let ttl = std::time::Duration::from_secs(60);
        let _ = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(1)
        });
        // 直接清空底层缓存（对应 invalidate_overview_cache / invalidate_caches 的语义）
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = cached_with(&cache, &compute_lock, "k", ttl, || {
            calls.set(calls.get() + 1);
            Ok(2)
        });
        assert_eq!(calls.get(), 2, "invalidate 后应重算");
    }

    /// 午休 TTL 断言（P3-2）：交易时段默认 10s，但本地时间落在 11:30–13:00 午休窗口时应放宽到 60s。
    /// snapshot_ttl 内部直接取 chrono::Local::now() 与 data::market_phase()，不可注入时刻，
    /// 故仅在「当前确为交易日午休」时才做强断言；其余时段跳过（注释说明，不为可测性大改结构）。
    #[test]
    fn snapshot_ttl_lunch_window_relaxes() {
        let now_secs = chrono::Local::now().num_seconds_from_midnight();
        let lunch_start = 11 * 3600 + 30 * 60;
        let lunch_end = 13 * 3600;
        let in_lunch = now_secs >= lunch_start && now_secs < lunch_end;
        if in_lunch && data::market_phase() == "intraday" {
            assert_eq!(
                snapshot_ttl(),
                Duration::from_secs(60),
                "午休窗口内 TTL 应放宽到 60s"
            );
        }
        // 非午休（或盘前/盘后/非交易日）时 TTL 由真实市场时段决定，跳过强断言。
    }

    #[test]
    fn cached_with_no_cross_key_interference() {
        // 单槽快照缓存：槽位仅保存「最近一个 key」的值，但必须按 key 区分——
        // 查 "b" 绝不能返回 "a" 的值（串扰），也绝不能因槽位被 "a" 占用而误命中。
        // 设计文档 §1.3「不同 key 不串扰」指的就是这个语义。
        let cache: std::sync::Mutex<Option<(std::time::Instant, String, i32)>> = std::sync::Mutex::new(None);
        let compute_lock: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let ttl = std::time::Duration::from_secs(60);
        let a = cached_with(&cache, &compute_lock, "a", ttl, || Ok(1)).unwrap();
        let b = cached_with(&cache, &compute_lock, "b", ttl, || Ok(2)).unwrap();
        assert_eq!(a, 1);
        // 关键断言：「写 b」之后，查 "b" 必须返回 b 自身的值 2，绝不能是 a 的 1（串扰）
        assert_eq!(b, 2, "不同 key 不应串扰：b 应返回自身值 2，而非 a 的 1");
        // 再次查 "b"（命中自身缓存槽）仍应是 2，而非被新 closure 的 888 覆盖或串到 a
        let b2 = cached_with(&cache, &compute_lock, "b", ttl, || Ok(888)).unwrap();
        assert_eq!(b2, 2, "b 命中自身缓存，应返回 2 而非重算的 888");
    }

    /// 真库副本验证（独立验证用，默认 ignore）：连续两次 get_overview，第二次应命中缓存、耗时 < 5ms
    /// 且数值与首查一致。设计文档 §1.3 验收要求。
    #[test]
    #[ignore]
    fn overview_second_call_hits_cache_and_fast() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        // 造一条最小持仓即可验证缓存命中路径（网络行情不可用时回退默认空，不影响命中判定）
        db::set_baseline(
            1, "000201", 1000.0, 1000.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set",
        )
        .unwrap();
        db::update_fund_nav("000201", 1.02, "002", false, "2026-09-03", Some(1.01)).unwrap();

        let t0 = std::time::Instant::now();
        let first = get_overview(None).unwrap();
        let first_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = std::time::Instant::now();
        let second = get_overview(None).unwrap();
        let second_ms = t1.elapsed().as_secs_f64() * 1000.0;

        assert_eq!(first.positions.len(), second.positions.len());
        assert!(
            second_ms < 5.0,
            "命中缓存的第二次 get_overview 应 < 5ms，got {:.3}ms（首次 {:.3}ms）",
            second_ms,
            first_ms
        );
    }

    // M3：命令层端到端——导出设备快照 → 破坏本地数据 → 导入快照合并恢复 → 重复导入幂等。
    #[test]
    fn sync_snapshot_roundtrip_via_commands() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();

        let acc = db::create_account("同步通道", "").unwrap();
        db::set_baseline(
            acc, "000666", 20.0, 200.0, 0.0, 0.0, 0.0, 0.0, "alipay", "manual_set",
        )
        .unwrap();

        let path = std::env::temp_dir().join(format!("fundlens_sync_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let out = sync_export_snapshot(path.to_string_lossy().to_string()).unwrap();
        assert!(out.count > 0, "快照应包含变更条目");
        assert!(path.is_file(), "快照文件应落盘");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.lines().next().unwrap_or_default().contains("fl_sync"),
            "首行应为快照头（fl_sync）"
        );

        let st = sync_status().unwrap();
        assert_eq!(st.pending_changes, 0, "导出后「自上次导出的变更」应为 0");
        assert_eq!(st.tables_synced, crate::sync::SYNCED_TABLES.len());
        assert!(st.last_export_at.is_some(), "应记录最近导出时间");

        // 破坏本地数据：删除该基金 → 导入快照应把它合并回来（而非整库覆盖）
        db::delete_fund("000666").unwrap();
        assert!(
            db::list_holdings(None).unwrap().iter().all(|h| h.code != "000666"),
            "删除后本地不应再有该持仓"
        );

        let imp = sync_import_snapshot(path.to_string_lossy().to_string()).unwrap();
        assert!(imp.total > 0, "快照应含变更条目");
        assert!(imp.applied > 0, "导入应实际应用变更");
        assert!(
            db::list_holdings(None).unwrap().iter().any(|h| h.code == "000666"),
            "导入后该持仓应恢复"
        );

        // 幂等：同一快照二次导入不产生冲突、不新增行
        let n_before = db::list_holdings(None).unwrap().len();
        let imp2 = sync_import_snapshot(path.to_string_lossy().to_string()).unwrap();
        assert_eq!(imp2.conflicts, 0, "同源快照二次导入不应记冲突");
        assert_eq!(
            db::list_holdings(None).unwrap().len(),
            n_before,
            "幂等：重复导入行数不变"
        );

        assert!(sync_status().unwrap().last_import_at.is_some(), "应记录最近导入时间");
        let _ = std::fs::remove_file(&path);
    }

    // M4：命令层备份——立即备份 / 列表 / 保留份数剪枝 / 导入前自动备份。
    #[test]
    fn sync_backup_commands_roundtrip() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        // 备份目录在进程内共享（DB_FILE 为 OnceLock）→ 先清空，保证份数断言确定。
        let _ = std::fs::remove_dir_all(crate::backup::backup_dir());

        assert_eq!(sync_set_backup_keep(2).unwrap(), 2, "保留份数落库并返回生效值");
        let b1 = sync_create_backup().unwrap();
        assert!(b1.size > 0, "备份文件应非空");
        assert_eq!(b1.tag, "manual");
        let listed = sync_list_backups().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].file, b1.file);

        // 导入前自动备份（M4 写库前触发）
        let snap = std::env::temp_dir().join(format!("fl_m4_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&snap);
        sync_export_snapshot(snap.to_string_lossy().to_string()).unwrap();
        let imp = sync_import_snapshot(snap.to_string_lossy().to_string()).unwrap();
        assert!(imp.backup_file.is_some(), "导入前应自动生成整库备份");
        let tags: Vec<String> = sync_list_backups()
            .unwrap()
            .into_iter()
            .map(|b| b.tag)
            .collect();
        assert!(tags.iter().any(|t| t == "pre-import"), "应存在 pre-import 备份");

        // 保留份数=2 → 剪枝后不超过 2 份
        let listed2 = sync_list_backups().unwrap();
        assert!(listed2.len() <= 2, "保留份数应生效，实际 {} 份", listed2.len());

        let st = sync_status().unwrap();
        assert_eq!(st.backup_keep, 2);
        assert_eq!(st.backup_count, listed2.len());
        assert!(st.last_backup_at.is_some(), "状态应带出最近备份时间");
        assert!(st.backup_dir.contains("backups"), "备份目录应在数据目录下的 backups/");
        let _ = std::fs::remove_file(&snap);
    }

    // M4：恢复闭环——建备份 → 改数据 → 恢复 → 数据回滚 + 安全网 + before-restore 记录。
    #[test]
    fn sync_restore_backup_roundtrip() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        // 备份目录在进程内共享（DB_FILE 为 OnceLock）→ 先清空，保证断言确定。
        let _ = std::fs::remove_dir_all(crate::backup::backup_dir());

        // 造一条数据并记下时点状态
        db::insert_fund(&db::FundRow {
            code: "110011".into(),
            name: "易方达中小盘混合".into(),
            platform: "alipay".into(),
            official_nav: 5.0,
            report_period: None,
            disclosure_type: None,
            fund_type: String::new(),
            track_index: String::new(),
            valuation_applicable: true,
        })
        .unwrap();

        // 备份（此时 nav = 5.0）
        let b = sync_create_backup().unwrap();
        assert_eq!(b.tag, "manual");

        // 改掉数据（nav = 9.0）
        db::update_fund_nav("110011", 9.0, "", false, "2026-01-01", None).unwrap();
        assert_eq!(
            db::list_funds().unwrap()[0].official_nav,
            9.0,
            "改数据后应读到新值"
        );

        // 恢复
        let out = sync_restore_backup(b.file.clone()).unwrap();
        assert!(out.safety_backup.is_some(), "恢复前应生成 before-restore 安全网");
        assert_eq!(out.file, b.file);

        // 数据应回滚到备份时点（nav = 5.0）
        let nav = db::list_funds()
            .unwrap()
            .iter()
            .find(|f| f.code == "110011")
            .unwrap()
            .official_nav;
        assert_eq!(nav, 5.0, "恢复后应回滚到备份时点的 nav");

        // 列表里应多一条 before-restore 记录
        let tags: Vec<String> = sync_list_backups().unwrap().into_iter().map(|x| x.tag).collect();
        assert!(
            tags.iter().any(|t| t == "before-restore"),
            "应存在 before-restore 安全网记录"
        );
    }

    // M4：删除闭环——建 → 删 → 文件与列表均消失；二次删除必须 Err。
    #[test]
    fn sync_delete_backup_removes_file() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        let _ = std::fs::remove_dir_all(crate::backup::backup_dir());

        let b = sync_create_backup().unwrap();
        let p = crate::backup::backup_dir().join(&b.file);
        assert!(p.is_file(), "备份文件应存在");

        sync_delete_backup(b.file.clone()).unwrap();
        assert!(!p.is_file(), "删除后文件应不存在");
        assert!(
            sync_list_backups().unwrap().iter().all(|x| x.file != b.file),
            "列表中应已移除该备份"
        );

        // 二次删除同一名字必须 Err
        assert!(
            sync_delete_backup(b.file.clone()).is_err(),
            "重复删除不存在的文件必须 Err"
        );
    }

    // M4：路径安全——拒绝穿越、非法扩展名、空串、不存在文件。
    #[test]
    fn backup_file_arg_rejects_traversal() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        let _ = std::fs::remove_dir_all(crate::backup::backup_dir());

        // 路径穿越（../ 与子目录与反斜杠）
        assert!(sync_restore_backup("../x.db".into()).is_err(), "拒绝 ../ 穿越");
        assert!(sync_restore_backup("a/b.db".into()).is_err(), "拒绝子目录穿越");
        assert!(sync_restore_backup("a\\b.db".into()).is_err(), "拒绝反斜杠穿越");
        assert!(sync_delete_backup("a/b.db".into()).is_err(), "删除也拒绝子目录穿越");
        // 空串
        assert!(sync_restore_backup("".into()).is_err(), "拒绝空串");
        // 非法扩展名
        assert!(sync_restore_backup("nope.txt".into()).is_err(), "拒绝非 .db 扩展名");
        // 不存在的合法命名文件
        assert!(
            sync_restore_backup("fundlens-20260101-000000-manual.db".into()).is_err(),
            "拒绝不存在的文件"
        );
        assert!(
            sync_delete_backup("fundlens-20260101-000000-manual.db".into()).is_err(),
            "删除不存在也要报错"
        );
    }

    // M2 云通道命令面：配置读写（令牌不回明文）/ 连通性检查 / 推送 / 拉取 / 状态卡。
    // 用「本地目录通道」跑真实链路 —— 无需任何云资源即可端到端验证命令层。
    #[test]
    fn sync_cloud_commands_roundtrip_via_dir_channel() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();

        let dir = std::env::temp_dir().join(format!("fl_m2_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dir_s = dir.to_string_lossy().to_string();

        // ① 默认关闭
        let c0 = sync_cloud_config_get().unwrap();
        assert_eq!(c0.mode, "off");
        assert!(!c0.ready, "未配置 → 不可用");

        // ② 切到目录通道
        let c1 = sync_cloud_config_set("dir".into(), String::new(), dir_s.clone(), None).unwrap();
        assert_eq!(c1.mode, "dir");
        assert!(c1.ready, "目录非空即可用");
        assert!(!c1.token_set);
        assert_eq!(sync_cloud_config_get().unwrap().dir, dir_s, "配置应落库");

        // ③ 连通性：空目录 → 0 条目
        let ck = sync_cloud_check().unwrap();
        assert_eq!(ck.mode, "dir");
        assert_eq!(ck.items, 0);
        assert_eq!(ck.others, 0);
        assert!(!ck.device_id.is_empty());

        // ④ 推送 → 远端 1 份（本设备），他人条目仍为 0
        let p = sync_cloud_push().unwrap();
        assert!(p.count > 0, "空库也应至少含 settings 等参与表");
        assert!(p.key.ends_with(".jsonl"));
        assert!(p.size > 0);
        let ck2 = sync_cloud_check().unwrap();
        assert_eq!(ck2.items, 1);
        assert_eq!(ck2.others, 0, "自己的快照不计入他人条目");

        // ⑤ 拉取：远端只有自己的快照 → 计划为空，且**不应**产生空备份
        let before = sync_list_backups().unwrap().len();
        let pl = sync_cloud_pull().unwrap();
        assert_eq!(pl.planned, 0, "本设备快照应被排除");
        assert_eq!(pl.applied, 0);
        assert_eq!(
            sync_list_backups().unwrap().len(),
            before,
            "无可拉取内容时不应写库、不应产生备份"
        );

        // ⑥ 状态卡带出云通道信息（令牌只报「是否已设置」）
        let st = sync_status().unwrap();
        assert_eq!(st.cloud_mode, "dir");
        assert!(st.cloud_ready);
        assert_eq!(st.cloud_dir, dir_s);
        assert!(st.cloud_last_push.is_some(), "推送后应记录时间");
        assert!(!st.cloud_token_set);
        assert_eq!(st.cloud_peers, 0, "尚无他设备水位");

        // ⑦ 令牌语义：None=保持、Some("")=清空、设置后 ready 取决于完整性
        let set1 = sync_cloud_config_set(
            "cloud".into(),
            "https://relay.example.com/sync".into(),
            String::new(),
            Some("secret-abc".into()),
        )
        .unwrap();
        assert!(set1.token_set && set1.ready);
        let keep = sync_cloud_config_set(
            "cloud".into(),
            "https://relay.example.com/sync".into(),
            String::new(),
            None,
        )
        .unwrap();
        assert!(keep.token_set, "token=None 应保持原有令牌");
        let cleared = sync_cloud_config_set(
            "cloud".into(),
            "https://relay.example.com/sync".into(),
            String::new(),
            Some(String::new()),
        )
        .unwrap();
        assert!(!cleared.token_set, "token=Some(\"\") 应清空");
        assert!(!cleared.ready, "HTTP 模式缺令牌 → 不可用");

        // ⑧ 配置不全时 check 应给可读原因（而不是 panic 或空结果）
        let err = sync_cloud_check().unwrap_err();
        assert!(err.contains("令牌"), "错误文案应指出缺令牌: {err}");

        // 还原为 off，避免污染同一进程内其它用例（临时库是进程级共享的）
        sync_cloud_config_set("off".into(), String::new(), String::new(), Some(String::new())).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 真实库副本端到端冒烟（默认跳过）：验证「真实 schema + 全量数据 + 外键 + 触发器」下
    // 快照导出/导入/幂等确实可用。跑法：
    //   sqlite3 "<真实库>" ".backup '/tmp/fl_real/fundlens.db'"
    //   FUNDLENS_DATA_DIR=/tmp/fl_real cargo test --lib --no-default-features real_db_snapshot_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_db_snapshot_smoke() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::init_db(None).unwrap();
        invalidate_caches();

        let out_path =
            std::env::temp_dir().join(format!("fl_real_sync_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&out_path);
        let out = sync_export_snapshot(out_path.to_string_lossy().to_string()).unwrap();
        println!("[smoke] 快照条数={} 大小={}B", out.count, out.size);
        let text = std::fs::read_to_string(&out_path).unwrap();
        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
        println!("[smoke] 文件行数={}（含头行）", lines);
        assert!(out.count > 0, "真实库快照应非空");
        assert_eq!(lines, out.count + 1, "行数应 = 条数 + 头行");

        let st = sync_status().unwrap();
        println!(
            "[smoke] 设备={} 表数={} 待同步={} 累计变更={} 冲突={} 最近导出={:?}",
            st.device_id, st.tables_synced, st.pending_changes, st.total_changes, st.conflict_count, st.last_export_at
        );
        assert_eq!(st.tables_synced, 13);
        assert_eq!(st.pending_changes, 0, "导出后自上次导出的变更应为 0");

        let before = db::list_holdings(None).unwrap();
        let target = before.first().expect("真实库应有持仓").code.clone();
        let n_before = before.len();
        db::delete_fund(&target).unwrap();
        let n_del = db::list_holdings(None).unwrap().len();
        println!("[smoke] 删除 {target} 后持仓 {n_before} → {n_del}");
        assert!(n_del < n_before, "删除应减少持仓");

        let imp = sync_import_snapshot(out_path.to_string_lossy().to_string()).unwrap();
        println!(
            "[smoke] 导入：total={} applied={} conflicts={} device={}",
            imp.total, imp.applied, imp.conflicts, imp.device
        );
        assert!(imp.applied > 0, "导入应实际应用变更");
        let restored = db::list_holdings(None).unwrap().len();
        println!("[smoke] 导入后持仓={restored}");
        assert_eq!(restored, n_before, "导入后持仓应恢复到快照时刻");

        let imp2 = sync_import_snapshot(out_path.to_string_lossy().to_string()).unwrap();
        println!("[smoke] 二次导入：applied={} conflicts={}", imp2.applied, imp2.conflicts);
        assert_eq!(imp2.conflicts, 0, "自导入不应产生冲突");
        assert_eq!(db::list_holdings(None).unwrap().len(), restored);
        println!("[smoke] OK：真实库快照导出/导入/幂等 全部通过");
        let _ = std::fs::remove_file(&out_path);
    }

}

// ============================================================================
// 备份恢复的对抗性回归：损坏/非法源文件必须被拒且**不得破坏活动库**。
// 该性质原先靠 SQLite 备份 API 的事务语义隐式保证（且错误信息不可读），现由
// sync_restore_backup 的显式只读探测守在前面；这条测试把它锁成回归。
// ============================================================================
#[cfg(test)]
mod restore_guard_tests {
    use super::*;

    #[test]
    fn sync_restore_backup_rejects_invalid_file_and_keeps_db_intact() {
        let _g = crate::db::tests::lock_db_tests();
        crate::db::tests::init_temp_db();
        invalidate_caches();
        let _ = std::fs::remove_dir_all(crate::backup::backup_dir());

        db::insert_fund(&db::FundRow {
            code: "P900".into(),
            name: "守卫测试基金".into(),
            platform: "alipay".into(),
            official_nav: 3.5,
            report_period: None,
            disclosure_type: None,
            fund_type: String::new(),
            track_index: String::new(),
            valuation_applicable: true,
        })
        .unwrap();
        let before = db::with_conn(|c| c.query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0)))
            .unwrap();

        // 垃圾字节冒充备份
        let dir = crate::backup::backup_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("garbage.db"),
            b"not a sqlite database, just ascii garbage bytes padded out a little",
        )
        .unwrap();
        let err = sync_restore_backup("garbage.db".into()).expect_err("垃圾文件必须被拒");
        assert!(
            err.contains("不是有效的数据库备份"),
            "错误信息要可读、指向问题所在，实际: {err}"
        );

        // 活动库必须完好：行数不变、数据未被半覆盖、且连接仍可用
        let after = db::with_conn(|c| c.query_row("SELECT COUNT(*) FROM funds", [], |r| r.get(0)))
            .unwrap();
        assert_eq!((before, after), (1, 1), "被拒的恢复不得改动活动库");
        let nav: f64 = db::with_conn(|c| {
            c.query_row("SELECT official_nav FROM funds WHERE code='P900'", [], |r| r.get(0))
        })
        .unwrap();
        assert_eq!(nav, 3.5, "原数据必须原样保留");
    }
}
