// Tauri 命令层 — 实现 SPEC.md 第 5 节约定的 11 条命令。
// 命令签名与前端 src/api.ts 的 invoke 调用保持一致。
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::db;
use crate::data;
use crate::valuation::{self, PositionForSummary};

/// 盘中实时估值的进程内缓存（避免每次刷新都重新联网）。
/// 仅在交易时段使用；TTL 内且 gztime 为当日则直接命中。
struct CachedEst {
    est_nav: f64,
    est_change_pct: f64,
    prev_nav: f64,
    gztime: String,
    fetched_at: i64,
}
static EST_CACHE: LazyLock<Mutex<HashMap<String, CachedEst>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const EST_TTL_SECS: i64 = 60;

/// 批量获取盘中实时估值（带缓存）。交易时段外返回空（此时平台估值无意义）。
fn get_realtime_estimates(codes: &[String]) -> HashMap<String, data::FundEstimate> {
    let now = chrono::Local::now().timestamp();
    let mut result: HashMap<String, data::FundEstimate> = HashMap::new();
    let mut to_fetch: Vec<String> = Vec::new();
    {
        let cache = EST_CACHE.lock().unwrap();
        for c in codes {
            if let Some(e) = cache.get(c) {
                if now - e.fetched_at < EST_TTL_SECS && data::estimate_is_fresh(&e.gztime) {
                    result.insert(
                        c.clone(),
                        data::FundEstimate {
                            est_nav: e.est_nav,
                            est_change_pct: e.est_change_pct,
                            prev_nav: e.prev_nav,
                            gztime: e.gztime.clone(),
                        },
                    );
                    continue;
                }
            }
            to_fetch.push(c.clone());
        }
    }
    if !to_fetch.is_empty() {
        let fetched = data::fetch_fund_estimates(&to_fetch);
        let mut cache = EST_CACHE.lock().unwrap();
        for (c, est) in &fetched {
            cache.insert(
                c.clone(),
                CachedEst {
                    est_nav: est.est_nav,
                    est_change_pct: est.est_change_pct,
                    prev_nav: est.prev_nav,
                    gztime: est.gztime.clone(),
                    fetched_at: now,
                },
            );
            result.insert(c.clone(), est.clone());
        }
    }
    result
}

#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FundEstimateView {
    est_nav: f64,
    est_change_pct: f64,
    gztime: String,
}

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
    total_pnl: f64,
    total_pnl_pct: f64,
    estimated: bool,
    disclosure_type: String,
    disclosed_weight_sum: f64,
    /// 估值来源：realtime=盘中实时估值(平台) / local=本地自算 / none=无
    valuation_source: String,
    /// 交叉验证置信度：high/medium/low/none（穿透估值 vs 平台估值）
    confidence: String,
    /// QDII 延迟结算提示：如 "T+1·海外交易中" / "T+1·海外净值"；非 QDII 为 None
    delay_note: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewOut {
    summary: valuation::PortfolioSummary,
    positions: Vec<PositionRowOut>,
    trading: bool,
    /// 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / prev_day=休市(上一交易日实际)
    market_session: String,
    as_of: String,
}

#[tauri::command]
pub fn get_overview(platform: Option<String>) -> Result<OverviewOut, String> {
    // 加载全部持仓（单机单账户 = 本人），随后按平台过滤
    let mut holdings = db::list_holdings(None).map_err(|e| e.to_string())?;
    if let Some(p) = &platform {
        holdings.retain(|h| &h.platform == p);
    }
    let trading = data::is_trading_now();
    // 基准指数行情：按各基金「类型+名称」识别标的指数后，一次性批量拉取组合内所需的不同基准，
    // 供穿透估值近似未披露部分（指数/ETF联接用其真实跟踪指数，债券用国债指数，其余用沪深300）。
    let mut bench_syms: Vec<String> = Vec::new();
    for h in &holdings {
        let (sym, _, _) = data::pick_benchmark(&h.fund_type, &h.name);
        if !bench_syms.contains(&sym) {
            bench_syms.push(sym);
        }
    }
    let bench_quotes = data::fetch_quotes(&bench_syms).unwrap_or_default();
    // 交易时段：批量获取全部基金的盘中实时估值（带缓存，并行拉取），作为优先来源
    let real_codes: Vec<String> = holdings
        .iter()
        .filter(|h| h.code.len() == 6 && h.code.chars().all(|c| c.is_ascii_digit()))
        .map(|h| h.code.clone())
        .collect();
    // 工作日（含盘后）均拉取实时估值：盘后 gsz 即当日实际净值，gsz-dwjz 即当日实际收益；
    // 周末/节假日平台无更新，靠 estimate_is_fresh 过滤掉失效数据。
    let estimates = if data::is_weekday_now() {
        get_realtime_estimates(&real_codes)
    } else {
        HashMap::new()
    };
    // 是否至少有基金拉到了「当日」新鲜实时估值（用于区分盘后当日实际 vs 休市上一交易日）
    let mut realtime_fresh_today = false;

    let mut positions = Vec::new();
    let mut summary_input = Vec::new();
    let mut total_est_day_pnl = 0.0; // 当日估算收益（投影）
    let mut total_act_day_pnl = 0.0; // 当日实际收益（已确认/上一交易日实际）

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
    let all_stock_quotes = data::fetch_quotes(&all_stock_syms).unwrap_or_default();

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
        let quotes = fetch_quotes_for_batch(&disclosures, &all_stock_quotes);
        let v_local = if f.valuation_applicable {
            valuation::value_fund(valuation::ValuationInput {
                fund_code: f.code.clone(),
                official_nav: f.official_nav,
                holdings: disclosures.clone(),
                quotes,
                benchmark: {
                    let (_, bcode, _) = data::pick_benchmark(&f.fund_type, &f.name);
                    bench_quotes.get(&bcode).cloned()
                },
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
            }
        };
        // pos / f 已由持仓视图 h 构造（见循环顶部），此处直接进入估值逻辑
        // 优先使用实时估值（交易时段=预估，盘后=实际）：要求 gztime 为当日，
        // 此时 gsz 即当日（收盘后即实际）净值，dwjz 为上一交易日单位净值，
        // 当日收益 = 份额 × (gsz - dwjz)。无新鲜实时估值则回落本地自算，再否则无。
        let rt = estimates.get(&f.code).filter(|e| data::estimate_is_fresh(&e.gztime));
        let is_qdii = data::is_qdii_fund(&f.fund_type);
        let qdii_suppress = is_qdii && data::qdii_overseas_open(&f.name);
        let mut delay_note: Option<String> = None;
        let (mut v, valuation_source, realtime_day_pnl) = if let Some(rt) = rt {
            realtime_fresh_today = true;
            // QDII 特殊处理：境外交易中则平台 gsz 仍在形成、非终值，抑制「当日收益」(T+1)；
            // 境外已收盘则保留 gsz 变动，但标注 T+1·海外净值，避免误读为 A 股当日涨跌。
            let (rt_pnl, note) = if is_qdii {
                if qdii_suppress {
                    (None, Some("T+1·海外交易中".to_string()))
                } else {
                    (
                        Some(pos.shares * (rt.est_nav - rt.prev_nav)),
                        Some("T+1·海外净值".to_string()),
                    )
                }
            } else {
                (Some(pos.shares * (rt.est_nav - rt.prev_nav)), None)
            };
            delay_note = note;
            (
                valuation::FundValuationResult {
                    fund_code: f.code.clone(),
                    official_nav: f.official_nav,
                    est_nav: rt.est_nav,
                    est_change_pct: rt.est_change_pct,
                    disclosure_type: "realtime".into(),
                    disclosed_weight_sum: 0.0,
                    holdings: vec![],
                    estimated: true,
                    reason: Some(format!(
                        "{}实时估值 @ {}",
                        if trading { "盘中" } else { "盘后" },
                        rt.gztime
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
                },
                "realtime".to_string(),
                rt_pnl,
            )
        } else {
            let src = if v_local.estimated {
                "local".to_string()
            } else {
                "none".to_string()
            };
            // 克隆而非移动：v_local 后续仍用于穿透估值与平台估值的交叉验证
            (v_local.clone(), src, None)
        };
        // 交叉验证：穿透估值（含基准近似）vs 平台实时估值 → 置信度 + 多源共识
        let platform_pct = rt.map(|e| e.est_change_pct);
        let (conf, div, consensus) = compute_confidence(v_local.est_change_pct, platform_pct);
        v.platform_est_change_pct = platform_pct;
        // 穿透源涨跌幅始终带来源数值（即便头条用平台），便于前端并列展示双源
        v.penetration_est_change_pct = if v_local.estimated {
            Some(v_local.est_change_pct)
        } else {
            None
        };
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
        // 仅当「有真实 6 位代码 且 有份额」才走本地自算估值；
        // 支付宝截图导入（份额=0、以持仓金额展示）即使解析到代码也仍用金额/收益直接展示。
        let has_real_code = f.code.len() == 6
            && f.code.chars().all(|c| c.is_ascii_digit())
            && pos.shares > 0.0;
        let avg_cost = if pos.shares > 0.0 { pos.cost_amount / pos.shares } else { 0.0 };

        // 当日收益拆分为「估算」与「实际」两个独立值（不再混在一张卡片里）：
        // - est_day_pnl（估算）：交易中/盘后展示实时或本地自算估值，随行情跳动；休市无交易→0（前端显示「—」）。
        // - act_day_pnl（实际）：盘后=当日实际(gz-dwjz)；休市=上一交易日实际(yesterday_profit)；交易中未实现→0（前端显示「—」）。
        let (market_value, total_pnl, day_pnl, total_pnl_pct, est_day_pnl, act_day_pnl) = if has_real_code {
            let mv = pos.shares * if v.estimated { v.est_nav } else { f.official_nav };
            let cost = pos.shares * avg_cost;
            let tpnl = mv - cost;
            let est = if qdii_suppress {
                // QDII 海外交易中：平台 gsz 非终值，当日估算不展示（前端显示「—」）
                0.0
            } else {
                realtime_day_pnl.unwrap_or_else(|| {
                    if v.estimated { pos.shares * (v.est_nav - f.official_nav) } else { 0.0 }
                })
            };
            let act = if let Some(d) = realtime_day_pnl {
                if trading { 0.0 } else { d }
            } else if !trading {
                pos.yesterday_profit
            } else {
                0.0
            };
            (mv, tpnl, est, if cost > 0.0 { tpnl / cost } else { 0.0 }, est, act)
        } else {
            let mv = pos.holding_amount;
            let tpnl = pos.holding_profit;
            let d = pos.yesterday_profit;
            (mv, tpnl, d, if mv > 0.0 { tpnl / mv } else { 0.0 }, d, d)
        };
        total_est_day_pnl += est_day_pnl;
        total_act_day_pnl += act_day_pnl;

        summary_input.push(PositionForSummary {
            fund_code: f.code.clone(),
            shares: pos.shares,
            avg_cost,
            est_nav: v.est_nav,
            estimated: v.estimated,
            official_nav: f.official_nav,
            explicit_market_value: if has_real_code { None } else { Some(pos.holding_amount) },
            explicit_total_pnl: if has_real_code { None } else { Some(pos.holding_profit) },
            explicit_day_pnl: if has_real_code { None } else { Some(pos.yesterday_profit) },
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
            market_value,
            day_pnl,
            total_pnl,
            total_pnl_pct,
            estimated: v.estimated,
            disclosure_type: v.disclosure_type.clone(),
            disclosed_weight_sum: v.disclosed_weight_sum,
            valuation_source,
            confidence: v.confidence.clone(),
            delay_note: delay_note.clone(),
        });
    }

    let mut summary = valuation::summarize_portfolio(&summary_input);
    // 用「估算/实际」拆分值覆盖汇总（summarize 内部给的是估值口径占位，这里换成按时段拆分的真实值）
    summary.est_day_pnl = total_est_day_pnl;
    summary.act_day_pnl = total_act_day_pnl;
    positions.sort_by(|a, b| b.market_value.partial_cmp(&a.market_value).unwrap_or(std::cmp::Ordering::Equal));
    // 市场时段细分：交易中=当日预估；工作日且拉到当日实时(盘后)=当日实际；其余休市=上一交易日实际
    let market_session = if trading {
        "intraday".to_string()
    } else if data::is_weekday_now() && realtime_fresh_today {
        "post_close".to_string()
    } else {
        "prev_day".to_string()
    };
    // 自动落库：当日首查即记录组合日快照（幂等 upsert），周报/月报/盈亏日历的历史从启用起累积。
    // 单机单账户，快照始终以 scope=0（全账户聚合）存储，历史不丢；平台筛选仅影响展示层。
    record_daily_snapshot(0, summary.total_market_value, summary.total_cost, summary.total_pnl);

    Ok(OverviewOut {
        summary,
        positions,
        trading: data::is_trading_now(),
        market_session,
        as_of: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    })
}

/// 记录某账户（scope=0 表示全部账户聚合）当日的组合市值快照。
/// 当日盈亏 = 市值变动 − 当日净现金流（入金−出金），避免充值/取现被误算为收益。
fn record_daily_snapshot(scope: i64, total_mv: f64, total_cost: f64, total_pnl: f64) {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let prev = db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT total_market_value FROM snapshots WHERE account_id = ?1 AND snapshot_date < ?2 \
             ORDER BY snapshot_date DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![scope, today.clone()], |r| r.get::<usize, f64>(0))?;
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
    let day_pnl = total_mv - prev.unwrap_or(total_mv) - net_cash;
    let _ = db::record_snapshot(scope, &today, total_mv, total_cost, total_pnl, day_pnl);
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FundDetailOut {
    fund: FundMetaOut,
    valuation: valuation::FundValuationResult,
    quotes: Vec<QuoteView>,
    trading: bool,
    realtime_estimate: Option<FundEstimateView>,
    /// QDII 延迟结算提示：如 "T+1·海外交易中" / "T+1·海外净值"；非 QDII 为 None
    delay_note: Option<String>,
    /// 该基金的交易流水（买卖/分红/手动），供基金明细页「交易记录」区块展示
    transactions: Vec<TransactionOut>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteView {
    stock_code: String,
    stock_name: String,
    price: f64,
    prev_close: f64,
    price_return: f64,
}

#[tauri::command]
pub fn get_fund_detail(code: String) -> Result<FundDetailOut, String> {
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let f = funds.into_iter().find(|x| x.code == code).ok_or("基金不存在")?;
    let disclosures = db::list_disclosures(&code).unwrap_or_default();
    let quotes = fetch_quotes_for(&disclosures);
    let (bench_sym, bench_code, _) = data::pick_benchmark(&f.fund_type, &f.name);
    let benchmark_quote = data::fetch_quotes(&[bench_sym]).and_then(|mut m| m.remove(&bench_code));
    let mut v = if f.valuation_applicable {
        valuation::value_fund(valuation::ValuationInput {
            fund_code: code.clone(),
            official_nav: f.official_nav,
            holdings: disclosures.clone(),
            quotes: quotes.clone(),
            benchmark: benchmark_quote,
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
    let avg_cost = if pos.shares > 0.0 { pos.cost_amount / pos.shares } else { 0.0 };
    // 优先提供盘中实时估值（交易时段），与总览口径一致
    let realtime_estimate = if data::is_trading_now() {
        data::fetch_fund_estimate(&code).map(|e| FundEstimateView {
            est_nav: e.est_nav,
            est_change_pct: e.est_change_pct,
            gztime: e.gztime,
        })
    } else {
        None
    };
    // 交叉验证：穿透估值（含基准近似）vs 平台实时估值 → 置信度 + 多源共识
    let platform_pct = realtime_estimate.as_ref().map(|e| e.est_change_pct);
    let (conf, div, consensus) = compute_confidence(v.est_change_pct, platform_pct);
    v.platform_est_change_pct = platform_pct;
    v.penetration_est_change_pct = if v.estimated { Some(v.est_change_pct) } else { None };
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
    Ok(FundDetailOut {
        fund: FundMetaOut {
            code: f.code,
            name: f.name,
            platform: f.platform.clone(),
            platform_name: platform_name(&f.platform),
            shares: pos.shares,
            cost_amount: pos.cost_amount,
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
        trading: data::is_trading_now(),
        realtime_estimate,
        delay_note,
        transactions,
    })
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
                data::is_equity_fund(&ftype),
            );
            f.nav = nav.nav;
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
        platform,
        official_nav,
        report_period: None,
        disclosure_type: None,
        fund_type: String::new(),
        valuation_applicable: true,
    };
    db::insert_fund(&f).map_err(|e| e.to_string())?;
    // 以「手动基线」写入持仓（替换既有 import/manual_set 基线，避免重复计数），随后重算。
    // 单机单账户固定 account_id = 1。
    db::set_baseline(1, &code, shares, cost_amount, 0.0, 0.0, 0.0, 0.0, "manual_set")
        .map_err(|e| e.to_string())?;
    // 尝试用官方接口补全净值与基金类型（lsjz 提供净值；类型用 fundsuggest 的 FTYPE，更可靠）
    if let Some(nav) = data::fetch_official_nav(&code) {
        let ftype = data::fetch_fund_type(&code).unwrap_or_default();
        let _ = db::update_fund_nav(
            &code,
            nav.nav,
            &ftype,
            data::is_equity_fund(&ftype),
        );
    }
    Ok(())
}

#[tauri::command]
pub fn update_position(code: String, shares: f64, cost_amount: f64) -> Result<(), String> {
    // 以「手动基线」覆盖持仓（单机单账户固定 account_id = 1）
    db::set_baseline(1, &code, shares, cost_amount, 0.0, 0.0, 0.0, 0.0, "manual_set")
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_fund(code: String) -> Result<(), String> {
    db::delete_fund(&code).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_disclosures(code: String) -> Result<Vec<valuation::DisclosedHolding>, String> {
    db::list_disclosures(&code).map_err(|e| e.to_string())
}

#[tauri::command]
/// 拉取某基金最新披露持仓并写入本地库（先清空旧记录，避免重复叠加）。
/// 供单只 `fetch_disclosure` 与批量 `fetch_all_disclosures` 复用。
fn store_disclosure(code: &str) -> Result<usize, String> {
    // 拉取最新披露持仓并写入本地库（先清空旧记录，避免重复叠加）
    let (period, dtype, holdings) = data::fetch_disclosure(code).ok_or("拉取披露持仓失败")?;
    db::delete_disclosures(code).map_err(|e| e.to_string())?;
    for h in &holdings {
        let _ = db::upsert_disclosure(code, &h.stock_code, &h.stock_name, h.weight, &period, &dtype);
    }
    Ok(holdings.len())
}

#[tauri::command]
pub fn fetch_disclosure(code: String) -> Result<usize, String> {
    store_disclosure(&code)
}

/// 一键抓取所有基金的披露持仓：遍历本地全部基金，逐只拉取并写入。
/// 失败安全：单只失败仅计入 failed_codes，不中断整体；带礼貌间隔避免触发东财限流。
#[tauri::command]
pub fn fetch_all_disclosures() -> Result<FetchAllDisclosuresOut, String> {
    let funds = db::list_funds().map_err(|e| e.to_string())?;
    let mut ok = 0usize;
    let mut failed_codes: Vec<String> = Vec::new();
    for f in &funds {
        match store_disclosure(&f.code) {
            Ok(_) => ok += 1,
            Err(_) => failed_codes.push(f.code.clone()),
        }
        // 礼貌间隔，降低被东财接口限流的概率
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(FetchAllDisclosuresOut {
        total: funds.len(),
        ok,
        failed: failed_codes.len(),
        failed_codes,
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
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
/// 失败安全：网络异常/解析失败返回 Err（调用方提示用户重试）。
#[tauri::command]
pub fn refresh_nav_history(code: String) -> Result<usize, String> {
    let points = data::fetch_nav_history(&code, 0).ok_or("拉取历史净值失败（网络或接口异常）")?;
    db::upsert_nav_history(&code, &points).map_err(|e| e.to_string())
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
    let nav_points: Vec<NavPointOut> = navs
        .into_iter()
        .filter(|n| pass(&n.date))
        .map(|n| NavPointOut { date: n.date, nav: n.nav, acc_nav: n.acc_nav })
        .collect();

    let cost = db::get_cost_series(&code, acc).map_err(|e| e.to_string())?;
    let cost_points: Vec<CostPointOut> = cost
        .into_iter()
        .filter(|c| pass(&c.date))
        .map(|c| CostPointOut {
            date: c.date,
            cumulative_cost: c.cumulative_cost,
            unit_cost: c.unit_cost,
            shares: c.shares,
        })
        .collect();

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

#[derive(serde::Serialize)]
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
) -> Result<i64, String> {
    // 单机单账户固定 account_id = 1
    db::add_transaction(
        1,
        &txn_type,
        fund_code,
        shares,
        amount,
        price,
        &txn_date,
        &txn_time.unwrap_or_default(),
        note,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_transaction(id: i64) -> Result<(), String> {
    db::delete_transaction(id).map_err(|e| e.to_string())
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
    db::import_transactions(1, &norm, source_ref).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPoint {
    pub date: String,
    pub total_market_value: f64,
    pub total_cost: f64,
    pub total_pnl: f64,
    pub day_pnl: f64,
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
    pub scope: String, // 账户名或「全部账户」
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub start_mv: f64,
    pub end_mv: f64,
    pub delta_mv: f64,
    pub delta_pnl: f64,
    pub pnl_rate: f64, // 区间收益率（相对期初成本）
    pub positive_days: usize,
    pub negative_days: usize,
    pub series: Vec<SnapshotPoint>,
    pub best: Option<MoverOut>,
    pub worst: Option<MoverOut>,
    pub has_history: bool,
}

/// 取某账户（scope=0 全部）的快照序列，并定位「截至 end 之前、距 end 约 days 天」的期初快照。
fn load_report_snapshots(scope: i64) -> Vec<db::SnapshotRow> {
    db::list_snapshots(scope).unwrap_or_default()
}

fn build_period_report(scope: i64, scope_name: String, days: i64) -> PeriodReportOut {
    let snaps = load_report_snapshots(scope);
    if snaps.len() < 2 {
        return PeriodReportOut {
            scope: scope_name,
            start_date: snaps.last().map(|s| s.snapshot_date.clone()),
            end_date: snaps.last().map(|s| s.snapshot_date.clone()),
            start_mv: snaps.last().map(|s| s.total_market_value).unwrap_or(0.0),
            end_mv: snaps.last().map(|s| s.total_market_value).unwrap_or(0.0),
            delta_mv: 0.0,
            delta_pnl: 0.0,
            pnl_rate: 0.0,
            positive_days: 0,
            negative_days: 0,
            series: snaps
                .iter()
                .map(|s| SnapshotPoint {
                    date: s.snapshot_date.clone(),
                    total_market_value: s.total_market_value,
                    total_cost: s.total_cost,
                    total_pnl: s.total_pnl,
                    day_pnl: s.day_pnl,
                })
                .collect(),
            best: None,
            worst: None,
            has_history: false,
        };
    }
    let end = snaps.last().unwrap();
    // 期初：日期 <= end_date - days 的最近一条
    let end_dt = chrono::NaiveDate::parse_from_str(&end.snapshot_date, "%Y-%m-%d").ok();
    let start = snaps
        .iter()
        .take_while(|s| {
            if let (Some(e), Ok(d)) = (
                end_dt,
                chrono::NaiveDate::parse_from_str(&s.snapshot_date, "%Y-%m-%d"),
            ) {
                let diff = e - d;
                diff.num_days() >= days
            } else {
                false
            }
        })
        .last()
        .or_else(|| snaps.first());
    let start = start.unwrap();
    let delta_mv = end.total_market_value - start.total_market_value;
    let delta_pnl = end.total_pnl - start.total_pnl;
    let pnl_rate = if start.total_cost > 0.0 {
        delta_pnl / start.total_cost
    } else {
        0.0
    };
    // 区间序列（含期初到期末）
    let series: Vec<SnapshotPoint> = snaps
        .iter()
        .filter(|s| {
            if let (Some(e), Ok(d)) = (
                end_dt,
                chrono::NaiveDate::parse_from_str(&s.snapshot_date, "%Y-%m-%d"),
            ) {
                (e - d).num_days() >= days - 1
            } else {
                false
            }
        })
        .map(|s| SnapshotPoint {
            date: s.snapshot_date.clone(),
            total_market_value: s.total_market_value,
            total_cost: s.total_cost,
            total_pnl: s.total_pnl,
            day_pnl: s.day_pnl,
        })
        .collect();
    let positive_days = series.iter().filter(|p| p.day_pnl > 0.0).count();
    let negative_days = series.iter().filter(|p| p.day_pnl < 0.0).count();
    // 个基最佳/最差（按当前累计收益率），复用实时总览的持仓口径（报表固定全账户，传 None）
    let ov = get_overview(None).unwrap_or_else(|_| OverviewOut {
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
        scope: scope_name,
        start_date: Some(start.snapshot_date.clone()),
        end_date: Some(end.snapshot_date.clone()),
        start_mv: start.total_market_value,
        end_mv: end.total_market_value,
        delta_mv,
        delta_pnl,
        pnl_rate,
        positive_days,
        negative_days,
        series,
        best,
        worst,
        has_history: true,
    }
}

#[tauri::command]
pub fn get_weekly_report() -> Result<PeriodReportOut, String> {
    // 单机单账户，报表始终以 scope=0（全部账户聚合）生成；平台拆分属于后续增强。
    Ok(build_period_report(0, "全部账户".to_string(), 7))
}

#[tauri::command]
pub fn get_monthly_report() -> Result<PeriodReportOut, String> {
    Ok(build_period_report(0, "全部账户".to_string(), 30))
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
                })
            } else {
                None
            }
        })
        .collect())
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

/// 将纯数字股票代码映射为腾讯 gtimg 行情请求符号：
/// - 5 位（如 00700）→ 港股 hk 前缀
/// - 6 位且以 6 开头（如 600519）→ 上交所 sh 前缀
/// - 6 位且以 0/3 开头（如 000568/300750）→ 深交所 sz 前缀
fn to_quote_symbol(stock_code: &str) -> String {
    if stock_code.len() == 5 {
        format!("hk{}", stock_code)
    } else if stock_code.starts_with('6') {
        format!("sh{}", stock_code)
    } else {
        format!("sz{}", stock_code)
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
