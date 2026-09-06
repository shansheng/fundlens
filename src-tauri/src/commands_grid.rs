//! commands_grid.rs —— 策略信号层命令（valuation_grid 引擎移植接线）
//!
//! 输入组装（方案 §7）：positions 按 fund_code 跨平台聚合（OD-1）→ nav_history /
//! 本地估值（value_fund，盘中）→ strategy::engine::compute_signal 纯计算 → 落
//! grid_signal / grid_signal_history（同码同日同源覆盖）→ 返回今日信号列表。
//! 铁律：只读 positions/transactions，写仅限 grid_* 新表；不自动改持仓（v9）。

use std::collections::HashMap;

use crate::db;
use crate::data;
use crate::strategy::model::*;
use crate::strategy::{batch, engine, helpers};
use crate::valuation;

fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ============================================================
// 输出结构（camelCase 给前端）
// ============================================================

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridConfigOut {
    pub fund_code: String,
    pub fund_name: Option<String>,
    pub fund_type: String,
    pub enabled: bool,
    pub max_position: Option<f64>,
    pub vol_sensitivity: Option<f64>,
    pub sell_fee_rate: Option<f64>,
    pub cooldown_sell_date: Option<String>,
    pub peak_nav: Option<f64>,
    pub shares: f64,
    pub cost_amount: f64,
    pub platforms: Vec<String>,
}

/// 货基/理财白名单外校验（P1：策略只针对权益/指数/债券等真实波动基金）
fn is_money_fund(fund_type: &str, code: &str) -> bool {
    let ft = fund_type.to_lowercase();
    ft.contains("货币") || ft.contains("理财") || ft.contains("money")
        || (code.starts_with("002") || code.starts_with("005"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridSignalOut {
    pub fund_code: String,
    pub fund_name: Option<String>,
    pub signal_date: String,
    pub source: String,
    pub signal_name: String,
    pub action: String,
    pub priority: i32,
    pub reason: String,
    pub amount: Option<f64>,
    pub sell_shares: Option<f64>,
    pub sell_pct: Option<f64>,
    pub alert: bool,
    pub confidence: f64,
    pub est_change_pct: f64,
    pub current_nav: f64,
    pub total_profit_pct: Option<f64>,
    pub regime: String,
    pub target_batch_id: Option<String>,
    pub fifo_plan: Option<serde_json::Value>,
    pub platforms: Vec<String>,
    pub is_rebuy: bool,
    pub pending_rebuy_id: Option<i64>,
    pub rebuy_plan: Option<serde_json::Value>,
}

impl GridSignalOut {
    fn from_signal(s: &GridSignal, platforms: Vec<String>) -> GridSignalOut {
        GridSignalOut {
            fund_code: s.fund_code.clone(),
            fund_name: s.fund_name.clone(),
            signal_date: s.signal_date.clone(),
            source: s.source.clone(),
            signal_name: s.signal_name.clone(),
            action: s.action.clone(),
            priority: s.priority,
            reason: s.reason.clone(),
            amount: s.amount,
            sell_shares: s.sell_shares,
            sell_pct: s.sell_pct,
            alert: s.alert,
            confidence: s.confidence,
            est_change_pct: s.est_change_pct,
            current_nav: s.current_nav,
            total_profit_pct: s.total_profit_pct,
            regime: s.regime.clone(),
            target_batch_id: s.target_batch_id.clone(),
            fifo_plan: s
                .fifo_plan
                .as_ref()
                .and_then(|f| serde_json::to_value(f).ok()),
            platforms,
            is_rebuy: s.is_rebuy,
            pending_rebuy_id: s.pending_rebuy_id,
            rebuy_plan: s.rebuy_plan.as_ref().and_then(|p| serde_json::to_value(p).ok()),
        }
    }
}

// ============================================================
// 聚合视图：跨平台按 fund_code 聚合成一只"策略基金"（OD-1）
// ============================================================

struct FundAgg {
    name: Option<String>,
    fund_type: String,
    official_nav: f64,
    shares: f64,
    cost_amount: f64,
    platforms: Vec<String>,
}

/// 从持仓行（每 code×platform 一行）聚合到 code 级。
/// 元数据（name/type/official_nav）以 funds 表为准，缺失时取持仓行首条。
fn aggregate_by_code() -> HashMap<String, FundAgg> {
    let funds = db::list_funds().unwrap_or_default();
    let mut meta: HashMap<String, (String, String, f64)> = HashMap::new(); // code -> (name, type, official_nav)
    for f in &funds {
        meta.entry(f.code.clone())
            .or_insert_with(|| (f.name.clone(), f.fund_type.clone(), f.official_nav));
    }
    let mut out: HashMap<String, FundAgg> = HashMap::new();
    let holdings = db::list_holdings(None).unwrap_or_default();
    for h in &holdings {
        let e = out.entry(h.code.clone()).or_insert_with(|| FundAgg {
            name: meta.get(&h.code).map(|m| m.0.clone()).or(Some(h.name.clone())),
            fund_type: meta.get(&h.code).map(|m| m.1.clone()).unwrap_or_else(|| h.fund_type.clone()),
            official_nav: meta.get(&h.code).map(|m| m.2).unwrap_or(h.official_nav),
            shares: 0.0,
            cost_amount: 0.0,
            platforms: Vec::new(),
        });
        e.shares += h.shares;
        e.cost_amount += h.cost_amount;
        if !e.platforms.contains(&h.platform) && !h.platform.is_empty() {
            e.platforms.push(h.platform.clone());
        }
    }
    out
}

/// 纯函数：账本事件 → 当前 holding 批次（buy/reinvest 增，sell FIFO 扣）。
/// 只做内存投影，绝不写 positions（v9 铁律）。
fn project_lots(events: &[db::LotEventRow]) -> Vec<(String, String, f64, f64, f64)> {
    // (id, txn_date, shares, amount, nav)
    let mut lots: Vec<(String, String, f64, f64, f64)> = Vec::new();
    for ev in events {
        let shares = if ev.txn_type == "sell" { -ev.shares } else { ev.shares };
        if shares == 0.0 {
            continue;
        }
        if shares > 0.0 {
            let nav = if ev.price > 0.0 {
                ev.price
            } else if shares > 0.0 {
                ev.amount / shares
            } else {
                0.0
            };
            lots.push((format!("txn:{}", ev.id), ev.txn_date.clone(), shares, ev.amount, nav));
        } else {
            // sell：FIFO 从最早批次扣减
            let mut remaining = -shares;
            let mut i = 0;
            while remaining > 1e-9 && i < lots.len() {
                let take = lots[i].2.min(remaining);
                if take > 0.0 {
                    // 按份额等比例扣减该批次成本
                    let ratio = take / lots[i].2;
                    lots[i].2 -= take;
                    lots[i].3 -= lots[i].3 * ratio;
                    remaining -= take;
                }
                i += 1;
            }
        }
    }
    lots.retain(|l| l.2 > 1e-6);
    lots
}

/// 自动行情模式：所有启用码净值日收益等权合成的市场级趋势（config.py _auto_detect_regime）。
/// manual>auto>neutral；auto 失败/数据不足回 neutral。
fn resolve_regime(enabled_codes: &[String]) -> (String, bool) {
    // 手动指认优先
    if let Some(m) = db::grid_settings_get("regime_manual").unwrap_or(None) {
        if m == "bear" || m == "neutral" {
            return (m, false);
        }
    }
    let auto_on = db::grid_settings_get("regime_auto")
        .unwrap_or(None)
        .map_or(true, |v| v != "0");
    if !auto_on {
        return ("neutral".to_string(), false);
    }

    // 等权平均日收益 → 合成净值序列
    use std::collections::BTreeMap;
    let mut by_date: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for code in enabled_codes {
        if let Ok(rows) = db::list_nav_history_code(code, None) {
            // rows 降序 → 翻转升序
            let asc: Vec<(String, f64)> = rows.into_iter().rev().collect();
            for i in 1..asc.len() {
                let prev = asc[i - 1].1;
                let cur = asc[i].1;
                if prev > 0.0 {
                    let chg = (cur / prev - 1.0) * 100.0;
                    by_date.entry(asc[i].0.clone()).or_default().push(chg);
                }
            }
        }
    }
    if by_date.len() < 21 {
        return ("neutral".to_string(), true);
    }
    let mut nav = 1.0;
    let mut levels: Vec<NavDay> = Vec::new();
    for (date, changes) in &by_date {
        let avg: f64 = changes.iter().sum::<f64>() / changes.len() as f64;
        nav *= 1.0 + avg / 100.0;
        levels.push(NavDay { date: date.clone(), nav });
    }
    levels.reverse(); // 降序
    let today = today_str();
    let tc = helpers::analyze_trend(0.0, &levels, &today, "nav");
    let bear = tc
        .long_20d
        .map_or(false, |l| l < -10.0)
        && (tc.trend_label == "连跌" || tc.trend_label == "中期走弱");
    (if bear { "bear".to_string() } else { "neutral".to_string() }, true)
}

// ============================================================
// 命令
// ============================================================

#[tauri::command]
pub fn grid_list_config() -> Result<Vec<GridConfigOut>, String> {
    let cfgs = db::grid_list_config().map_err(|e| e.to_string())?;
    let agg = aggregate_by_code();
    let mut out = Vec::new();
    let mut configured: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in cfgs {
        let a = agg.get(&c.fund_code);
        configured.insert(c.fund_code.clone());
        out.push(GridConfigOut {
            fund_code: c.fund_code.clone(),
            fund_name: a.and_then(|x| x.name.clone()),
            fund_type: a.map(|x| x.fund_type.clone()).unwrap_or_default(),
            enabled: c.enabled == 1,
            max_position: c.max_position,
            vol_sensitivity: c.vol_sensitivity,
            sell_fee_rate: c.fee_schedule.as_deref().map(db::fee_schedule_sell_rate),
            cooldown_sell_date: c.cooldown_sell_date,
            peak_nav: c.peak_nav,
            shares: a.map(|x| x.shares).unwrap_or(0.0),
            cost_amount: a.map(|x| x.cost_amount).unwrap_or(0.0),
            platforms: a.map(|x| x.platforms.clone()).unwrap_or_default(),
        });
    }
    // 候选卡：持仓中但尚未配置的基金（enabled=false）——修复空库时"无卡可启"
    // 的 UX 死锁（此前 configs 只来自 grid_funds，表空则策略页无任何可选卡片）。
    // 货基/理财不列入候选（与启用白名单一致，避免列出无法启用的卡）。
    let mut candidates: Vec<(String, &FundAgg)> = agg
        .iter()
        .filter(|(code, a)| !configured.contains(*code) && !is_money_fund(&a.fund_type, code) && a.shares > 0.0)
        .map(|(code, a)| (code.clone(), a))
        .collect();
    // 按持仓成本降序：重仓在前，便于优先对主力基金开启
    candidates.sort_by(|x, y| y.1.cost_amount.partial_cmp(&x.1.cost_amount).unwrap_or(std::cmp::Ordering::Equal));
    for (code, a) in candidates {
        out.push(GridConfigOut {
            fund_code: code.clone(),
            fund_name: a.name.clone(),
            fund_type: a.fund_type.clone(),
            enabled: false,
            max_position: None,
            vol_sensitivity: None,
            sell_fee_rate: None,
            cooldown_sell_date: None,
            peak_nav: None,
            shares: a.shares,
            cost_amount: a.cost_amount,
            platforms: a.platforms.clone(),
        });
    }
    Ok(out)
}

/// 启用/停用策略基金（P1 白名单：货基/理财禁启用）
#[tauri::command]
pub fn grid_enable_fund(fund_code: String, enabled: bool, max_position: Option<f64>) -> Result<(), String> {
    if enabled {
        let funds = db::list_funds().map_err(|e| e.to_string())?;
        if let Some(f) = funds.iter().find(|f| f.code == fund_code) {
            if is_money_fund(&f.fund_type, &fund_code) {
                return Err(format!("{}（{}）是货币/理财基金，策略信号不适用", f.name, fund_code));
            }
        }
    }
    db::grid_upsert_fund(&fund_code, if enabled { 1 } else { 0 }, max_position)
        .map_err(|e| e.to_string())
}

/// 整行保存单只策略基金配置（P1：费率/灵敏度/冷却/上限）
#[tauri::command]
pub fn grid_save_fund(
    fund_code: String,
    max_position: Option<f64>,
    vol_sensitivity: Option<f64>,
    sell_fee_rate: Option<f64>,
    cooldown_sell_date: Option<String>,
) -> Result<(), String> {
    if vol_sensitivity.is_some_and(|v| !(0.3..=3.0).contains(&v)) {
        return Err("vol_sensitivity 须在 0.3 ~ 3.0 之间".to_string());
    }
    if sell_fee_rate.is_some_and(|v| !(0.0..=0.02).contains(&v)) {
        return Err("卖出费率须在 0 ~ 2% 之间".to_string());
    }
    db::grid_save_config(
        &fund_code,
        max_position,
        vol_sensitivity,
        sell_fee_rate,
        cooldown_sell_date,
    )
    .map_err(|e| e.to_string())
}

/// P1：回填信号 T+3/5/10 outcome + 胜率聚合（事件研究，buy 后涨/sell 后跌为"对"）
#[tauri::command]
pub fn grid_backfill_outcomes() -> Result<serde_json::Value, String> {
    let updated = db::grid_backfill_outcomes().map_err(|e| e.to_string())?;
    let stats = db::grid_outcome_stats().map_err(|e| e.to_string())?;
    let rows: Vec<serde_json::Value> = stats
        .into_iter()
        .map(|(action, n, win, avg3, avg5, avg10)| {
            serde_json::json!({
                "action": action,
                "count": n,
                "winCount": win,
                "winRate": if n > 0 { win as f64 / n as f64 } else { 0.0 },
                "avgT3": avg3,
                "avgT5": avg5,
                "avgT10": avg10,
            })
        })
        .collect();
    Ok(serde_json::json!({ "updated": updated, "stats": rows }))
}

/// P2：延迟回补挂单列表（全量含状态，前端挂单区展示）
#[tauri::command]
pub fn grid_list_pending(fund_code: Option<String>, limit: Option<i64>) -> Result<serde_json::Value, String> {
    let rows = db::grid_pending_list(fund_code.as_deref(), limit.unwrap_or(50)).map_err(|e| e.to_string())?;
    serde_json::to_value(&rows).map_err(|e| e.to_string())
}

/// P2：手动取消挂单（pending → cancelled）
#[tauri::command]
pub fn grid_pending_cancel(fund_code: String, id: i64) -> Result<(), String> {
    db::grid_pending_transition(&fund_code, id, "cancelled")
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn grid_get_settings() -> Result<serde_json::Value, String> {
    let manual = db::grid_settings_get("regime_manual").unwrap_or(None);
    let manual_ok = manual.as_deref().map_or(false, |m| m == "bear" || m == "neutral");
    let auto_on = db::grid_settings_get("regime_auto")
        .unwrap_or(None)
        .map_or(true, |v| v != "0");
    let cash = db::grid_settings_get("cash_available").unwrap_or(None);
    Ok(serde_json::json!({
        "regime": if manual_ok { manual.clone().unwrap() } else { "neutral".to_string() },
        "auto": auto_on,
        "manual": manual_ok,
        "manualRegime": if manual_ok { manual } else { None::<String> },
        "cashAvailable": cash,
    }))
}

#[tauri::command]
pub fn grid_set_regime(
    regime: String,
    auto: Option<bool>,
    manual: Option<bool>,
    cash_available: Option<f64>,
) -> Result<serde_json::Value, String> {
    let regime = if regime == "bear" { "bear" } else { "neutral" }; // bull 禁用 → neutral
    let manual_mode = manual.unwrap_or(false);
    if manual_mode {
        db::grid_settings_set("regime_manual", Some(regime)).map_err(|e| e.to_string())?;
    } else {
        db::grid_settings_set("regime_manual", None).map_err(|e| e.to_string())?;
        db::grid_settings_set("regime_auto", Some(if auto.unwrap_or(true) { "1" } else { "0" }))
            .map_err(|e| e.to_string())?;
    }
    // P3：可投现金预算（None=不动；Some(v) 且 v>0 → 设置；v==0 → 清空禁用闸门）
    if let Some(cash) = cash_available {
        if cash <= 0.0 {
            db::grid_settings_set("cash_available", None).map_err(|e| e.to_string())?;
        } else {
            db::grid_settings_set("cash_available", Some(&format!("{:.2}", cash)))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(serde_json::json!({
        "regime": regime,
        "auto": auto.unwrap_or(true),
        "manual": manual_mode,
        "manualRegime": if manual_mode { Some(regime.to_string()) } else { None::<String> },
    }))
}

#[tauri::command]
pub fn grid_signal_history(fund_code: Option<String>, limit: Option<i64>) -> Result<serde_json::Value, String> {
    let rows = db::grid_list_history(fund_code.as_deref(), limit.unwrap_or(30)).map_err(|e| e.to_string())?;
    serde_json::to_value(&rows).map_err(|e| e.to_string())
}

/// 今日信号徽标轻读（不做计算，供总览持仓表「信号」列）
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GridTodayBadgeOut {
    pub fund_code: String,
    pub signal_name: Option<String>,
    pub action: String,
    pub alert: bool,
}

#[tauri::command]
pub fn grid_today_signals(signal_date: Option<String>) -> Result<Vec<GridTodayBadgeOut>, String> {
    let date = signal_date.unwrap_or_else(today_str);
    let rows = db::grid_list_signals_today(Some(&date)).map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|r| GridTodayBadgeOut {
            fund_code: r.fund_code,
            signal_name: r.signal_name,
            action: r.action,
            alert: r.alert != 0,
        })
        .collect())
}

// ============================================================
// grid_compute_signals：主装配（策略输入组装 → 引擎 → 落库）
// ============================================================

#[tauri::command]
pub fn grid_compute_signals() -> Result<serde_json::Value, String> {
    let today = today_str();
    let cfgs = db::grid_list_config().map_err(|e| e.to_string())?;
    let enabled: Vec<db::GridFundCfg> = cfgs.into_iter().filter(|c| c.enabled == 1).collect();
    if enabled.is_empty() {
        return Ok(serde_json::json!({"signals": [], "regime": "neutral", "autoRegime": true, "computedAt": today}));
    }
    let codes: Vec<String> = enabled.iter().map(|c| c.fund_code.clone()).collect();

    // ── 行情模式（手动 > 自动；全部基金共用一次）──
    let (regime, auto_regime) = resolve_regime(&codes);

    // ── 聚合视图 + 披露/行情装配（复刻 get_overview 的批量拉取，仅限启用码）──
    let agg = aggregate_by_code();
    let phase = data::market_phase().to_string();
    let intraday = phase == "intraday";

    let mut disclosures_by_fund: HashMap<String, Vec<valuation::DisclosedHolding>> = HashMap::new();
    for (fc, dh) in db::list_disclosures_batch().unwrap_or_default() {
        if codes.contains(&fc) {
            disclosures_by_fund.entry(fc).or_default().push(dh);
        }
    }

    let mut bench_syms: Vec<String> = Vec::new();
    let mut stock_syms: Vec<String> = Vec::new();
    for code in &codes {
        if let Some(a) = agg.get(code) {
            let (sym, _, _) = data::pick_benchmark(&a.fund_type, a.name.as_deref().unwrap_or(""));
            if !bench_syms.contains(&sym) {
                bench_syms.push(sym);
            }
            if let Some(hs) = disclosures_by_fund.get(code) {
                for d in hs {
                    let digit: String = d.stock_code.chars().filter(|c| c.is_ascii_digit()).collect();
                    if !digit.is_empty() && !stock_syms.contains(&digit) {
                        stock_syms.push(digit);
                    }
                }
            }
        }
    }
    let mut all_syms = bench_syms.clone();
    for s in &stock_syms {
        if !all_syms.contains(s) {
            all_syms.push(s.clone());
        }
    }
    // 盘后才拉行情（盘中估算才需要）；失败静默降级 source=nav
    let all_quotes = if intraday { data::fetch_quotes(&all_syms).unwrap_or_default() } else { HashMap::new() };
    let mut index_quotes: HashMap<String, valuation::StockQuote> = HashMap::new();
    if intraday {
        let mut hk_syms: Vec<String> = Vec::new();
        for sym in &bench_syms {
            if sym.starts_with("hk") {
                hk_syms.push(sym.clone());
            } else {
                let digit: String = sym.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Some(q) = all_quotes.get(&digit) {
                    index_quotes.insert(sym.clone(), q.clone());
                }
            }
        }
        if !hk_syms.is_empty() {
            for (k, v) in data::fetch_hk_index_quotes(&hk_syms) {
                index_quotes.insert(k, v);
            }
        }
    }

    let mut signals: Vec<GridSignalOut> = Vec::new();
    for cfg in &enabled {
        let code = &cfg.fund_code;
        let Some(fa) = agg.get(code) else { continue };
        let name = fa.name.clone();

        // ── nav 历史（降序）与批次视图 ──
        let nav_rows = db::list_nav_history_code(code, None).unwrap_or_default();
        let nav_hist: Vec<NavDay> = nav_rows
            .iter()
            .map(|(d, n)| NavDay { date: d.clone(), nav: *n })
            .collect();

        let events = db::list_lot_events(code).unwrap_or_default();
        let lots = project_lots(&events);
        let buy_txns: Vec<batch::BatchTxnLike> = lots
            .iter()
            .map(|(id, date, shares, amount, nav)| batch::BatchTxnLike {
                txn_id: id.trim_start_matches("txn:").to_string(),
                buy_date: date.clone(),
                shares: *shares,
                amount: *amount,
                nav: *nav,
                is_rebuy: false,
            })
            .collect();
        let sum_lots_amount: f64 = lots.iter().map(|l| l.3).sum();
        let genesis_cost = Some((fa.cost_amount - sum_lots_amount).max(0.0));
        let fallback_nav = nav_hist.first().map(|n| n.nav).unwrap_or(0.0);
        let batches = batch::build_batch_view(
            &buy_txns,
            fa.shares,
            cfg.peak_nav,
            genesis_cost,
            fallback_nav,
        );

        // ── 输入字段：source / today_change / current_nav / confidence ──
        let (source, today_change, confidence) = if intraday {
            // 盘中：本地持仓穿透/指数代理估值（FundLens 估值层，弃 fundgz —— OD-5）
            let mut est_pct = 0.0;
            let mut est_ok = false;
            let disclosures = disclosures_by_fund.get(code).cloned().unwrap_or_default();
            let is_index = data::is_index_fund(&fa.fund_type, name.as_deref().unwrap_or(""));
            let pure_index = data::is_pure_index_fund(&fa.fund_type, name.as_deref().unwrap_or(""));
            let bench_sym = {
                let (sym, _, _) = data::pick_benchmark(&fa.fund_type, name.as_deref().unwrap_or(""));
                sym
            };
            if fa.official_nav > 0.0 {
                let fund_quotes: HashMap<String, valuation::StockQuote> = disclosures
                    .iter()
                    .map(|d| {
                        let digit: String = d.stock_code.chars().filter(|c| c.is_ascii_digit()).collect();
                        (digit.clone(), all_quotes.get(&digit).cloned())
                    })
                    .filter_map(|(k, v)| v.map(|q| (k, q)))
                    .collect();
                let bench = index_quotes.get(&bench_sym).cloned();
                let tracked_index = if is_index {
                    let (tsym, _, _) = data::resolve_tracked_index(&fa.fund_type, name.as_deref().unwrap_or(""), "")
                        .unwrap_or_else(|| (bench_sym.clone(), String::new(), String::new()));
                    index_quotes.get(&tsym).cloned()
                } else {
                    None
                };
                let v = valuation::value_fund(valuation::ValuationInput {
                    fund_code: code.clone(),
                    official_nav: fa.official_nav,
                    holdings: disclosures,
                    quotes: fund_quotes,
                    benchmark: bench,
                    tracked_index,
                    pure_index,
                });
                est_pct = v.est_change_pct;
                est_ok = v.estimated;
            }
            if est_ok {
                ("estimation".to_string(), est_pct, 0.75)
            } else {
                // 无法估值 → 降级为最近真实净值日涨跌（source=nav），不阻塞卖出/观望类信号
                let chg = if nav_hist.len() >= 2 { (nav_hist[0].nav / nav_hist[1].nav - 1.0) * 100.0 } else { 0.0 };
                ("nav".to_string(), chg, 0.85)
            }
        } else {
            // 盘后/休市：真实净值日涨跌（最新两交易日）
            let chg = if nav_hist.len() >= 2 { (nav_hist[0].nav / nav_hist[1].nav - 1.0) * 100.0 } else { 0.0 };
            ("nav".to_string(), chg, 0.85)
        };

        let current_nav = nav_hist.first().map(|n| n.nav).unwrap_or(fa.official_nav);
        let total_cost = if fa.cost_amount > 0.0 { fa.cost_amount } else { batches.iter().map(|b| b.amount).sum() };
        let total_profit_pct = if total_cost > 0.0 && current_nav > 0.0 {
            Some(((fa.shares * current_nav - total_cost) / total_cost) * 100.0)
        } else {
            None
        };

        // P2：活跃延迟回补挂单（最早创建的一条；引擎触发检查只消费最早）
        let pending_rebuy = db::grid_pending_list_active(&code)
            .ok()
            .and_then(|rows| rows.into_iter().next())
            .filter(|p| p.trigger_nav.is_some() && p.amount.is_some())
            .map(|p| RebuyOrder {
                id: p.id,
                trigger_nav: p.trigger_nav.unwrap_or(0.0),
                amount: p.amount.unwrap_or(0.0),
                sell_nav: p.sell_nav.unwrap_or(0.0),
                signal_label: p.signal_label.unwrap_or_else(|| "延迟回补".to_string()),
                source_signal: p.source_signal.unwrap_or_default(),
            });

        let input = StrategyInput {
            fund_code: code.clone(),
            fund_name: name.clone(),
            today: today.clone(),
            source: source.clone(),
            market_closed: !intraday,
            today_change,
            confidence,
            current_nav,
            nav_hist,
            batches,
            total_profit_pct,
            regime: regime.clone(),
            vol_sensitivity: cfg.vol_sensitivity.unwrap_or(1.0),
            sell_fee_rate: cfg.fee_schedule.as_deref().map(db::fee_schedule_sell_rate).unwrap_or(0.0),
            cooldown_sell_date: cfg.cooldown_sell_date.clone(),
            max_position: cfg.max_position,
            available_cash: None,
            pending_rebuy,
        };

        let sig = engine::compute_signal(&input);
        // P2 闭环：触发 → 挂单标记 triggered；卖出带 rebuy_plan → 创建新挂单（软上限内）
        if sig.is_rebuy {
            if let Some(pid) = sig.pending_rebuy_id {
                let _ = db::grid_pending_transition(&code, pid, "triggered");
            }
        } else if let Some(plan) = &sig.rebuy_plan {
            let label = if sig.signal_name.starts_with("延迟回补") {
                sig.signal_name.clone()
            } else {
                format!("延迟回补({})", sig.signal_name)
            };
            let _ = db::grid_pending_add(
                &code,
                plan.trigger_nav,
                plan.amount,
                plan.ratio,
                &sig.signal_name,
                &label,
                current_nav,
            );
        }
        let _ = db::grid_upsert_signal_and_history(&sig);
        signals.push(GridSignalOut::from_signal(&sig, fa.platforms.clone()));
    }

    // ── P3：组合日买入 20% 闸门（可投现金预算）──
    // 全部 buy 建议按 priority 累计；超出 预算×20% 的：可部分容纳则缩额，否则暂缓（amount=None 并注明）。
    let cash_available = db::grid_settings_get("cash_available")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<f64>().ok());
    let mut budget_cap: Option<f64> = None;
    let mut budget_used: f64 = 0.0;
    if let Some(cash) = cash_available {
        if cash > 0.0 {
            let cap = cash * 0.20;
            budget_cap = Some(cap);
            // 高优先（priority 小）先买；同优先级按代码稳定排序
            signals.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.fund_code.cmp(&b.fund_code))
            });
            for sig in signals.iter_mut() {
                if sig.action != "buy" {
                    continue;
                }
                let Some(amt) = sig.amount else { continue };
                if amt <= 0.0 {
                    continue;
                }
                if budget_used + amt > cap {
                    let allowed = (cap - budget_used).max(0.0);
                    if allowed >= 100.0 {
                        sig.amount = Some((allowed * 100.0).round() / 100.0);
                        sig.reason.push_str(&format!(
                            "（现金闸门：日买入{:.0}元上限，缩至{:.0}元）",
                            cap, allowed
                        ));
                        budget_used = cap;
                    } else {
                        sig.amount = None;
                        sig.reason.push_str(&format!("（现金闸门：日买入{:.0}元预算已用尽，暂缓）", cap));
                    }
                } else {
                    budget_used += amt;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "signals": signals,
        "regime": regime,
        "autoRegime": auto_regime,
        "computedAt": today,
        "budgetCap": budget_cap,
        "budgetUsed": budget_used,
    }))
}
