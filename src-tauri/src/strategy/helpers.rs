//! strategy/helpers.rs —— valuation_grid/grid/helpers.py 纯函数全集移植
//!
//! 移植铁律（见任务书与移植方案 §5）：
//! - 本模块为纯逻辑：不碰 DB / 网络 / 系统时钟；所有"现在"一律来自 `StrategyInput.today`。
//! - 数值保真：所有 round(x, n) 走 `py_round`（银行家半偶舍入，区别于 Rust 默认半进位）。
//! - 源函数名/行号在每段注释标注，方便日后对拍。
//!
//! 依赖：`crate::strategy::model`（输入/输出结构）、`crate::strategy::config`（常量表）。

use chrono::NaiveDate;
use crate::strategy::config::*;
use crate::strategy::model::*;

// ============================================================
// 银行家半偶舍入（helpers.py 全局 round(x, n) 的精确等价）
// ============================================================

/// helpers.py: 全局 round(x, nd) —— Python 的 round 是 half-even（半偶）舍入，
/// 不是 Rust f64::round 的 half-away-from-zero。本函数用 10^nd 缩放 → 半偶 → 除回。
pub fn py_round(x: f64, nd: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let factor = 10f64.powi(nd);
    let scaled = x * factor;
    let r = round_half_even(scaled);
    r / factor
}

fn round_half_even(v: f64) -> f64 {
    let f = v.floor();
    let diff = v - f;
    if diff < 0.5 {
        f
    } else if diff > 0.5 {
        f + 1.0
    } else {
        // 恰好落在 0.5：看整数部分的奇偶（半偶舍入）
        if (f % 2.0).abs() < 1e-9 {
            f
        } else {
            f + 1.0
        }
    }
}

// ============================================================
// 日期工具（替代 Python datetime；纯函数，today 由调用方注入）
// ============================================================

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// 天数差 = (to - from).days，坏日期回退 999（与 helpers._count_trade_days_between 一致）
pub fn days_between(from: &str, to: &str) -> i32 {
    match (parse_date(from), parse_date(to)) {
        (Some(a), Some(b)) => (b - a).num_days() as i32,
        _ => 999,
    }
}

// ============================================================
// 波动率自适应阈值生成器
// helpers.py:51 _vol_adaptive_thresholds
// ============================================================

/// helpers.py:51 —— 根据波动率动态生成所有阈值。
/// 源里 sensitivity 由 `_get_vol_sensitivity(fund_code)` 缓存/读取；FundLens 已在上层解析好，
/// 这里直接收 `sensitivity: f64`（= input.vol_sensitivity），不再做任何文件/缓存读写。
#[derive(Clone, Debug)]
pub struct VolThresholds {
    pub dip_threshold: f64,
    pub tp_trigger: f64,
    pub stop_loss: f64,
    pub supplement_trigger: f64,
    pub supplement_loss_min: f64,
    pub consecutive_dip: f64,
    pub trend_weak: f64,
    pub disaster_loss: f64,
    pub disaster_daily: f64,
    pub supplement_tiers: Vec<(i32, f64, f64, f64)>,
    pub total_profit_tiers: Vec<(f64, f64)>,
    pub vol_based: bool,
    pub sensitivity: f64,
}

// helpers.py:51 _vol_adaptive_thresholds
pub fn vol_adaptive_thresholds(vol: Option<f64>, sensitivity: f64) -> VolThresholds {
    if vol.is_none() || vol.unwrap() <= 0.0 {
        return VolThresholds {
            dip_threshold: DEFAULT_DIP_THRESHOLD,
            tp_trigger: DEFAULT_TAKE_PROFIT_TRIGGER,
            stop_loss: DEFAULT_STOP_LOSS_BASE,
            supplement_trigger: DEFAULT_SUPPLEMENT_TRIGGER,
            supplement_loss_min: DEFAULT_SUPPLEMENT_LOSS_MIN,
            consecutive_dip: DEFAULT_CONSECUTIVE_DIP_TRIGGER,
            trend_weak: DEFAULT_TREND_WEAK_CUMULATIVE,
            disaster_loss: DEFAULT_DISASTER_LOSS,
            disaster_daily: DEFAULT_DISASTER_DAILY_DROP,
            supplement_tiers: SUPPLEMENT_TIERS.to_vec(),
            total_profit_tiers: TOTAL_PROFIT_SELL_TIERS.to_vec(),
            vol_based: false,
            sensitivity,
        };
    }

    let v = vol.unwrap() * sensitivity;

    let dip = (f64::max(-5.0, f64::min(-1.2, py_round(-v * DIP_BUY_VOL_MULTIPLE, 2)))).max(-5.0).min(-1.2);
    let tp = f64::max(1.0, f64::min(5.0, py_round(v * TAKE_PROFIT_VOL_MULTIPLE, 2)));
    let sl = f64::max(-10.0, f64::min(-3.0, py_round(-v * STOP_LOSS_VOL_MULTIPLE, 2)));
    let supp_trig = f64::max(-4.0, f64::min(-0.8, py_round(-v * SUPPLEMENT_TRIGGER_VOL_MULTIPLE, 2)));
    let supp_loss = f64::max(-10.0, f64::min(-2.0, py_round(-v * SUPPLEMENT_LOSS_VOL_MULTIPLE, 2)));
    let consec_dip = f64::max(-2.5, f64::min(-0.5, py_round(-v * CONSECUTIVE_DIP_VOL_MULTIPLE, 2)));
    let tw = f64::max(-4.0, f64::min(-1.0, py_round(-v * TREND_WEAK_VOL_MULTIPLE, 2)));
    let dis_loss = f64::max(-12.0, f64::min(-5.0, py_round(-v * DISASTER_LOSS_VOL_MULTIPLE, 2)));
    let dis_daily = f64::max(-7.0, f64::min(-3.0, py_round(-v * DISASTER_DAILY_VOL_MULTIPLE, 2)));

    let mut supp_tiers: Vec<(i32, f64, f64, f64)> = Vec::new();
    for &(tier_count, ratio, trig_mul, loss_mul) in SUPPLEMENT_TIERS_VOL.iter() {
        let t = f64::max(-4.0, f64::min(-0.8, py_round(-v * trig_mul, 2)));
        let l = f64::max(-12.0, f64::min(-2.0, py_round(-v * loss_mul, 2)));
        supp_tiers.push((tier_count, ratio, t, l));
    }

    let tp_tiers: Vec<(f64, f64)> = TOTAL_PROFIT_SELL_TIERS_VOL
        .iter()
        .map(|&(mul, pct)| (py_round(f64::max(0.3, f64::min(4.0, v * mul)), 2), pct))
        .collect();

    VolThresholds {
        dip_threshold: dip,
        tp_trigger: tp,
        stop_loss: sl,
        supplement_trigger: supp_trig,
        supplement_loss_min: supp_loss,
        consecutive_dip: consec_dip,
        trend_weak: tw,
        disaster_loss: dis_loss,
        disaster_daily: dis_daily,
        supplement_tiers: supp_tiers,
        total_profit_tiers: tp_tiers,
        vol_based: true,
        sensitivity,
    }
}

// ============================================================
// 波动率状态机
// helpers.py:119 _classify_volatility
// ============================================================

// helpers.py:119 _classify_volatility
pub fn classify_volatility(vol: f64) -> String {
    if vol < VOL_LOW {
        "low_vol".to_string()
    } else if vol < VOL_NORMAL_HIGH {
        "normal_vol".to_string()
    } else if vol < VOL_EXTREME {
        "high_vol".to_string()
    } else {
        "extreme_vol".to_string()
    }
}

// ============================================================
// 趋势上下文构建
// engine.py:230 _analyze_trend（综合趋势分析）
// ============================================================

/// engine.py:230 —— 综合趋势上下文。
/// 输入：today_change（今日涨跌%）、nav_hist（降序净值日，index0=最新）、today（注入"今天"）、
/// source（"estimation"/"nav"）。
///
/// 与源一致处理"今日重复计入"：源 generate_signal 在 source=="nav" 时会把 today_change 对应的
/// 那天从 nav_history / hist_changes 中剔除（engine.py:417-440）。这里统一做法：凡是 nav_hist 中
/// date == today 的记录都视为"今日那条"剔除（trend_navs），再用相邻净值差分还原 hist_changes。
///
/// `recent_changes` 近似 Python `val.recent_changes`（FundLens 无该独立字段，用同一套历史日收益）。
#[derive(Clone, Debug)]
pub struct TrendCtx {
    pub short_3d: Option<f64>,
    pub short_5d: Option<f64>,
    pub mid_10d: Option<f64>,
    pub long_20d: Option<f64>,
    pub volatility: Option<f64>,
    pub volatility_robust: Option<f64>,
    pub volume_proxy: Option<f64>,
    pub consecutive_down: i32,
    pub consecutive_up: i32,
    pub max_drawdown: f64,
    pub max_drawdown_60: f64,
    pub trend_label: String,
    pub recent_changes: Vec<f64>,
}

fn compound_return(changes: &[f64]) -> f64 {
    let mut product = 1.0;
    for &c in changes {
        product *= 1.0 + c / 100.0;
    }
    py_round((product - 1.0) * 100.0, 2)
}

// engine.py:230 _analyze_trend
pub fn analyze_trend(today_change: f64, nav_hist: &[NavDay], today: &str, source: &str) -> TrendCtx {
    // 1) 剔除"今日"记录（与源 generate_signal source=="nav" 的排除逻辑对齐）
    let trend_navs: Vec<&NavDay> = nav_hist.iter().filter(|h| h.date != today).collect();

    // 2) 由相邻净值差分还原历史日收益（降序 → 最新一天在 [0]）
    let mut hist_changes: Vec<f64> = Vec::new();
    for i in 1..trend_navs.len() {
        let prev = trend_navs[i - 1].nav;
        let cur = trend_navs[i].nav;
        if cur > 0.0 {
            hist_changes.push(py_round((prev / cur - 1.0) * 100.0, 2));
        }
    }

    let mut all_changes = vec![today_change];
    all_changes.extend_from_slice(&hist_changes);

    let short_3d = if all_changes.len() >= 3 {
        Some(compound_return(&all_changes[..3]))
    } else {
        Some(compound_return(&all_changes))
    };
    let short_5d = if all_changes.len() >= 5 {
        Some(compound_return(&all_changes[..5]))
    } else {
        None
    };

    let navs: Vec<f64> = trend_navs.iter().map(|h| h.nav).collect();
    let latest_is_today = trend_navs.first().map_or(false, |h| h.date == today);
    let nav0_adj = if navs.is_empty() {
        0.0
    } else if !latest_is_today {
        navs[0] * (1.0 + today_change / 100.0)
    } else {
        navs[0]
    };

    let mut mid_10d: Option<f64> = None;
    let mut long_20d: Option<f64> = None;
    if !navs.is_empty() {
        if navs.len() >= 10 {
            mid_10d = Some(py_round((nav0_adj / navs[9] - 1.0) * 100.0, 2));
        } else if navs.len() >= 2 {
            mid_10d = Some(py_round((nav0_adj / navs[navs.len() - 1] - 1.0) * 100.0, 2));
        }
        if navs.len() >= 20 {
            long_20d = Some(py_round((nav0_adj / navs[19] - 1.0) * 100.0, 2));
        } else if all_changes.len() >= 20 {
            long_20d = Some(compound_return(&all_changes[..20]));
        }
    }

    // 波动率（取 all_changes 前 20 条；源 helpers 用 all_changes[:20]）
    let vol_data: &[f64] = if all_changes.len() > 20 {
        &all_changes[..20]
    } else {
        &all_changes[..]
    };
    let mut volatility: Option<f64> = None;
    let mut volatility_robust: Option<f64> = None;
    if vol_data.len() >= 5 {
        let mean = vol_data.iter().sum::<f64>() / vol_data.len() as f64;
        let variance = vol_data.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / vol_data.len() as f64;
        volatility = Some(py_round(variance.sqrt(), 2));

        let mut sorted = vol_data.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        };
        let mut abs_devs: Vec<f64> = sorted.iter().map(|c| (c - median).abs()).collect();
        abs_devs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let m = abs_devs.len();
        let mad = if m % 2 == 1 {
            abs_devs[m / 2]
        } else {
            (abs_devs[m / 2 - 1] + abs_devs[m / 2]) / 2.0
        };
        volatility_robust = Some(py_round(mad * 1.4826, 2));
    }

    // 成交量代理
    let mut volume_proxy: Option<f64> = None;
    if all_changes.len() >= 5 {
        if let Some(vr) = volatility_robust {
            if vr > 0.0 {
                let recent_abs: Vec<f64> = all_changes[..5].iter().map(|c| c.abs()).collect();
                let mean_abs = recent_abs.iter().sum::<f64>() / recent_abs.len() as f64;
                volume_proxy = Some(py_round(mean_abs / vr, 2));
            }
        }
    }

    // 连涨/连跌计数
    let mut consecutive_down = 0i32;
    for &c in &all_changes {
        if c < 0.0 {
            consecutive_down += 1;
        } else {
            break;
        }
    }
    let mut consecutive_up = 0i32;
    for &c in &all_changes {
        if c > 0.0 {
            consecutive_up += 1;
        } else {
            break;
        }
    }

    // 回撤（20 日窗口）
    let mut max_drawdown = 0.0;
    if trend_navs.len() >= 5 {
        let mut navs_chrono: Vec<f64> = trend_navs.iter().map(|h| h.nav).collect();
        navs_chrono.reverse(); // 升序（最老在前）
        let peak = navs_chrono.first().copied().unwrap_or(0.0);
        let mut peak = peak;
        for &nv in &navs_chrono {
            if nv > peak {
                peak = nv;
            }
            if peak > 0.0 {
                let dd = (peak - nv) / peak * 100.0;
                if dd > max_drawdown {
                    max_drawdown = dd;
                }
            }
        }
    }
    max_drawdown = py_round(max_drawdown, 2);

    // 回撤（60 日窗口）
    let mut max_drawdown_60 = 0.0;
    if trend_navs.len() >= 10 {
        let mut navs_60: Vec<f64> = trend_navs.iter().map(|h| h.nav).collect();
        if navs_60.len() > 60 {
            navs_60.truncate(60);
        }
        navs_60.reverse();
        let peak_60 = navs_60.first().copied().unwrap_or(0.0);
        let mut peak_60 = peak_60;
        for &nv in &navs_60 {
            if nv > peak_60 {
                peak_60 = nv;
            }
            if peak_60 > 0.0 {
                let dd = (peak_60 - nv) / peak_60 * 100.0;
                if dd > max_drawdown_60 {
                    max_drawdown_60 = dd;
                }
            }
        }
    }
    max_drawdown_60 = py_round(max_drawdown_60, 2);

    // 趋势标签（v5.19：估值模式加 0.3 容差，仅影响标签不影响数值）
    let hyst = if source == "estimation" { 0.3 } else { 0.0 };
    let trend_label = if consecutive_down >= 3 {
        "连跌".to_string()
    } else if consecutive_up >= 3 {
        "连涨".to_string()
    } else if short_3d.map_or(false, |s| s < -(2.0 + hyst)) {
        "偏弱".to_string()
    } else if short_3d.map_or(false, |s| s > 2.0 + hyst) {
        "偏强".to_string()
    } else if mid_10d.map_or(false, |m| m < -(5.0 + hyst)) {
        "中期走弱".to_string()
    } else if mid_10d.map_or(false, |m| m > 5.0 + hyst) {
        "中期走强".to_string()
    } else {
        "震荡".to_string()
    };

    TrendCtx {
        short_3d: short_3d.map(|v| py_round(v, 2)),
        short_5d: short_5d.map(|v| py_round(v, 2)),
        mid_10d,
        long_20d,
        volatility,
        volatility_robust,
        volume_proxy,
        consecutive_down,
        consecutive_up,
        max_drawdown,
        max_drawdown_60,
        trend_label,
        recent_changes: hist_changes,
    }
}

// ============================================================
// 动量因子计算
// helpers.py:134 _calc_momentum_score
// ============================================================

// helpers.py:134 _calc_momentum_score
pub fn calc_momentum_score(tc: &TrendCtx) -> f64 {
    let s5 = tc.short_5d;
    let m10 = tc.mid_10d;
    let l20 = tc.long_20d;

    let norm = |x: Option<f64>, scale: f64| -> f64 {
        match x {
            None => 0.0,
            Some(v) => (v / scale).tanh(),
        }
    };

    let score = 0.5 * norm(s5, 4.0) + 0.3 * norm(m10, 6.0) + 0.2 * norm(l20, 10.0);
    py_round(f64::max(-1.0, f64::min(1.0, score)), 3)
}

// ============================================================
// 动态阈值计算
// helpers.py:153 _calc_risk_multiplier
// ============================================================

// helpers.py:153 _calc_risk_multiplier
pub fn calc_risk_multiplier(tc: &TrendCtx) -> f64 {
    let mdd_20 = tc.max_drawdown;
    let mdd_60 = tc.max_drawdown_60;
    let mdd = f64::max(mdd_20, mdd_60);

    let mdd_term = if mdd <= 5.0 {
        0.0
    } else if mdd <= 10.0 {
        (mdd - 5.0) * 0.06
    } else {
        0.30 + (mdd - 10.0) * 0.03
    };

    let risk_mul = 1.0 + mdd_term;
    f64::max(0.85, f64::min(1.5, risk_mul))
}

/// helpers.py:170 _calc_dynamic_thresholds（移植为写入 model.ThresholdsSet）
///
/// 与源差异：
/// - `fund_code` 不再 parse（FundLens 单账户，fund_code 即真实 6 位代码）。
/// - `vol_sensitivity` 直接收 `input.vol_sensitivity`（已在上层解析）。
/// - `signal_stats` 为 Option：FundLens P0 暂无信号胜率历史，传 None 时 win_rate_adj=1.0（不做收紧）。
pub struct SignalStats {
    pub buy_win_rate: Option<f64>,
    pub buy_sample_count: i32,
}

// helpers.py:170 _calc_dynamic_thresholds
pub fn calc_dynamic_thresholds(
    tc: &TrendCtx,
    confidence: f64,
    source: &str,
    vol_sensitivity: f64,
    signal_stats: Option<&SignalStats>,
) -> (ThresholdsSet, f64) {
    let risk_mul = calc_risk_multiplier(tc);
    let vol = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(1.0));
    let vol_state = classify_volatility(vol);

    let va = vol_adaptive_thresholds(Some(vol), vol_sensitivity);

    let mut dip_threshold = py_round(va.dip_threshold * risk_mul, 2);
    let mut tp_trigger = py_round(va.tp_trigger, 2);
    let mut stop_loss_adj = py_round(va.stop_loss * risk_mul, 2);

    if source == "estimation" && confidence < 0.75 {
        tp_trigger = py_round(tp_trigger + 0.5, 2);
    }

    let mut supplement_tiers_adj: Vec<(i32, f64, f64, f64)> = va
        .supplement_tiers
        .iter()
        .map(|&(count, ratio, trigger, loss_min)| {
            (count, ratio, py_round(trigger * risk_mul, 2), py_round(loss_min * risk_mul, 2))
        })
        .collect();

    let trail_dd = f64::max(TRAIL_DD_MIN, f64::min(TRAIL_DD_MAX, TRAIL_DD_BASE * risk_mul));

    // 信号胜率自适应（P0 无历史则跳过）
    let mut win_rate_adj = 1.0;
    if let Some(stats) = signal_stats {
        if let Some(wr) = stats.buy_win_rate {
            if wr < WIN_RATE_TIGHTEN_THRESHOLD && stats.buy_sample_count >= 5 {
                win_rate_adj = WIN_RATE_TIGHTEN_FACTOR;
                dip_threshold = py_round(dip_threshold * win_rate_adj, 2);
                supplement_tiers_adj = supplement_tiers_adj
                    .iter()
                    .map(|&(c, r, t, l)| (c, r, py_round(t * win_rate_adj, 2), py_round(l * win_rate_adj, 2)))
                    .collect();
            }
        }
    }

    if vol_state == "low_vol" {
        dip_threshold = py_round(dip_threshold * 0.85, 2);
        tp_trigger = py_round(tp_trigger * 0.85, 2);
    }

    dip_threshold = f64::max(-6.0, dip_threshold);
    stop_loss_adj = f64::max(-12.0, stop_loss_adj);

    let rebuy_step = if vol > 0.0 {
        f64::max(0.8, vol * 0.8)
    } else {
        SUPPLEMENT_REBUY_STEP_PCT
    };

    let thresholds = ThresholdsSet {
        dip_threshold: py_round(dip_threshold, 2),
        tp_trigger: py_round(tp_trigger, 2),
        stop_loss_adj: py_round(stop_loss_adj, 2),
        supplement_tiers: supplement_tiers_adj,
        trail_dd: py_round(trail_dd, 2),
        vol_state: vol_state.clone(),
        momentum_score: calc_momentum_score(tc),
        win_rate_adj: py_round(win_rate_adj, 2),
        rebuy_step: py_round(rebuy_step, 2),
        consecutive_dip_trigger: py_round(va.consecutive_dip, 2),
        supplement_trigger: py_round(va.supplement_trigger, 2),
        supplement_loss_min: py_round(va.supplement_loss_min, 2),
        trend_weak_cumulative: py_round(va.trend_weak, 2),
        disaster_loss_threshold: py_round(va.disaster_loss, 2),
        disaster_daily_drop: py_round(va.disaster_daily, 2),
        total_profit_sell_tiers: va.total_profit_tiers.clone(),
    };
    // model.ThresholdsSet 无 risk_multiplier 字段（model.rs 锁定），单独返回（对应源 dict 的 risk_multiplier）
    (thresholds, py_round(risk_mul, 2))
}

// ============================================================
// 统一止盈评分框架
// helpers.py:245 _calc_sell_score
// ============================================================

/// helpers.py:245 —— 止盈评分结果
#[derive(Clone, Debug)]
pub struct SellScore {
    pub score: f64,
    pub sell_pct: i32,
    pub signal_name: Option<String>,
    pub reason: String,
    pub profit_pct: f64,
    pub peak_profit: f64,
}

// helpers.py:245 _calc_sell_score
pub fn calc_sell_score(
    batch: &Batch,
    current_nav: f64,
    today_change: f64,
    tc: &TrendCtx,
    dst: &ThresholdsSet,
    fee_rate: f64,
    hold_days: i32,
    peak_profit: f64,
    nz_bonus: f64,
    tp_suppress_threshold: f64,
) -> SellScore {
    let profit_pct = if batch.nav > 0.0 {
        py_round((current_nav / batch.nav - 1.0) * 100.0, 2)
    } else {
        0.0
    };

    // 持仓不足15日且盈利<10% 不触发
    if hold_days < 15 && profit_pct < 10.0 {
        return SellScore {
            score: 0.0,
            sell_pct: 0,
            signal_name: None,
            reason: "持仓不足15日且盈利<10%，暂不止盈".to_string(),
            profit_pct,
            peak_profit,
        };
    }

    if profit_pct <= fee_rate * 2.0 {
        return SellScore {
            score: 0.0,
            sell_pct: 0,
            signal_name: None,
            reason: "盈利不足覆盖费率".to_string(),
            profit_pct,
            peak_profit,
        };
    }

    let vol = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(1.2));
    let profit_norm = f64::max(8.0, vol * 8.0);
    let profit_score = (profit_pct / profit_norm).tanh() * 40.0;

    let mut trail_score = 0.0;
    if peak_profit > 3.0 && peak_profit > profit_pct {
        let dd = peak_profit - profit_pct;
        let trail_dd_threshold = dst.trail_dd;
        if dd >= trail_dd_threshold {
            trail_score = f64::min(30.0, dd / trail_dd_threshold * 15.0);
        }
    }

    let momentum = dst.momentum_score;
    let momentum_score = f64::max(0.0, -momentum * 15.0);

    let mut liquidity_score = 0.0;
    let liquidity_trigger = f64::max(1.5, vol * TAKE_PROFIT_VOL_MULTIPLE);
    if today_change >= liquidity_trigger {
        liquidity_score = f64::min(15.0, (today_change - liquidity_trigger) * 5.0);
    }

    let fee_drag = -fee_rate * 5.0;

    let total_score = profit_score + trail_score + momentum_score + liquidity_score + fee_drag + nz_bonus;

    let (mut sell_pct, mut signal_name) = if total_score >= 75.0 {
        (100, Some("强势止盈".to_string()))
    } else if total_score >= 60.0 {
        (70, Some("止盈卖出".to_string()))
    } else if total_score >= 55.0 {
        (50, Some("分批止盈".to_string()))
    } else if total_score >= 53.0 {
        (30, Some("慢涨止盈".to_string()))
    } else {
        (0, None)
    };

    if today_change >= liquidity_trigger && sell_pct > 0 && sell_pct < 100 {
        sell_pct = f64::min(100.0, sell_pct as f64 + LIQUIDITY_PREMIUM_EXTRA_PCT) as i32;
    }

    let trend_label = &tc.trend_label;
    let suppress = (trend_label == "连涨" || trend_label == "偏强" || trend_label == "中期走强")
        && total_score < tp_suppress_threshold;

    let reason = if suppress {
        format!(
            "综合评分{:.0}但趋势为{}，强力抑制止盈让利润奔跑",
            total_score, trend_label
        )
    } else {
        format!(
            "综合评分{:.0}(盈利{:.0}+回撤{:.0}+动量{:.0}+流动性{:.0}+费率{:.0}+扭亏{:.0})",
            total_score, profit_score, trail_score, momentum_score, liquidity_score, fee_drag, nz_bonus
        )
    };

    if suppress {
        sell_pct = 0;
        signal_name = None;
    }

    SellScore {
        score: py_round(total_score, 1),
        sell_pct,
        signal_name,
        reason,
        profit_pct,
        peak_profit,
    }
}

// ============================================================
// 补仓成本修复效率
// helpers.py:344 _calc_cost_repair_efficiency
// ============================================================

// helpers.py:344 _calc_cost_repair_efficiency
pub fn calc_cost_repair_efficiency(batches: &[Batch], current_nav: f64, supplement_amount: f64) -> f64 {
    let total_cost: f64 = batches.iter().map(|b| b.amount).sum();
    let total_shares: f64 = batches.iter().map(|b| b.shares).sum();

    if total_shares <= 0.0 || current_nav <= 0.0 || supplement_amount <= 0.0 {
        return 0.0;
    }

    let avg_cost_before = total_cost / total_shares;
    let new_shares = supplement_amount / current_nav;
    let avg_cost_after = (total_cost + supplement_amount) / (total_shares + new_shares);

    let cost_drop_pct = (avg_cost_before - avg_cost_after) / avg_cost_before * 100.0;
    let efficiency = cost_drop_pct / (supplement_amount / 1000.0);
    py_round(efficiency, 4)
}

// ============================================================
// 动态补仓上限
// helpers.py:361 _calc_dynamic_supplement_max
// ============================================================

// helpers.py:361 _calc_dynamic_supplement_max
pub fn calc_dynamic_supplement_max(max_position: Option<f64>, batches: &[Batch]) -> i32 {
    let max_pos = max_position.unwrap_or(5000.0);
    let holding: Vec<&Batch> = batches.iter().filter(|b| b.is_holding()).collect();
    let first_amount = if let Some(sorted) = holding.first() {
        sorted.amount
    } else {
        max_pos * 0.3
    };

    if first_amount <= 0.0 {
        return SUPPLEMENT_MAX_COUNT_DEFAULT;
    }

    let dynamic_max = (max_pos / first_amount).ceil() as i32 - 1;
    f64::max(1.0, f64::min(SUPPLEMENT_MAX_COUNT_HARD_CAP as f64, dynamic_max as f64)) as i32
}

// ============================================================
// 三级止损体系
// helpers.py:382 _evaluate_stop_loss
// ============================================================

/// helpers.py:382 —— 止损评估结果
#[derive(Clone, Debug)]
pub struct StopLossEval {
    pub level: Option<String>,
    pub sell_pct: i32,
    pub reason: String,
}

// helpers.py:382 _evaluate_stop_loss
#[allow(clippy::too_many_arguments)]
pub fn evaluate_stop_loss(
    profit_pct: f64,
    stop_loss_adj: f64,
    hold_days: i32,
    fee_rate: f64,
    tc: &TrendCtx,
    confidence: f64,
    source: &str,
    supplement_count: i32,
    today_change: f64,
    l2_sell_pct_base: f64,
    is_rebuy_batch: bool,
) -> StopLossEval {
    // 源: if l2_sell_pct_base is None: l2_sell_pct_base = STOP_LOSS_L2_SELL_PCT_BASE
    // 这里收 f64，调用方传 0.0 表示"未指定"时回落常量。
    let l2_sell_pct_base = if l2_sell_pct_base == 0.0 {
        STOP_LOSS_L2_SELL_PCT_BASE
    } else {
        l2_sell_pct_base
    };

    if hold_days < 7 {
        return StopLossEval {
            level: None,
            sell_pct: 0,
            reason: "未满7天，走灾难保护通道".to_string(),
        };
    }

    let mut confidence_adj = 0.0;
    if source == "estimation" && confidence < 0.6 {
        confidence_adj = -1.0;
    }

    let effective_stop = stop_loss_adj - fee_rate + confidence_adj;
    let consec_down = tc.consecutive_down;

    // L3 极端止损
    let extreme_threshold = effective_stop * STOP_LOSS_L3_FACTOR;
    if profit_pct <= extreme_threshold
        || (profit_pct <= effective_stop && consec_down >= STOP_LOSS_L3_CONSEC_DOWN)
    {
        let mut reason = format!("极端止损: 浮亏{:.1}%%", profit_pct);
        if consec_down >= STOP_LOSS_L3_CONSEC_DOWN {
            reason.push_str(&format!("，连跌{}天", consec_down));
        }

        let mut l3_sell_pct = 100;
        if hold_days < 25 && today_change > -4.0 {
            l3_sell_pct = 70;
            reason.push_str(&format!(
                "，持仓{}天(<25天)+今日跌{:.1}%(>-4%)，减仓70%保留观察仓",
                hold_days, today_change
            ));
        } else {
            reason.push_str(&format!("，持仓{}天，清仓100%", hold_days));
        }

        return StopLossEval {
            level: Some("L3".to_string()),
            sell_pct: l3_sell_pct,
            reason,
        };
    }

    // 延迟回补仓位 L2 保护期
    if is_rebuy_batch && hold_days < 10 {
        return StopLossEval {
            level: None,
            sell_pct: 0,
            reason: format!("回补仓位{}天<10天保护期，跳过L2止损", hold_days),
        };
    }

    let mut l2_stop = effective_stop;
    let trend_label = &tc.trend_label;
    if trend_label == "震荡" {
        l2_stop = py_round(l2_stop * 1.25, 2);
    }

    if hold_days < 10 {
        if profit_pct <= l2_stop {
            if consec_down >= 3 {
                let l2_sell_pct = f64::max(30.0, l2_sell_pct_base - supplement_count as f64 * 10.0) as i32;
                return StopLossEval {
                    level: Some("L2".to_string()),
                    sell_pct: l2_sell_pct,
                    reason: format!(
                        "常规止损: 浮亏{:.1}%% ≤ 止损线{:.1}%，持仓{}天，连跌{}天确认趋势恶化，减仓{}%(已补仓{}次)",
                        profit_pct, l2_stop, hold_days, consec_down, l2_sell_pct, supplement_count
                    ),
                };
            } else {
                return StopLossEval {
                    level: Some("L1".to_string()),
                    sell_pct: 0,
                    reason: format!(
                        "止损预警: 浮亏{:.1}%%触及止损线{:.1}%，但持仓仅{}天且连跌{}天(<3天)，等待趋势确认",
                        profit_pct, l2_stop, hold_days, consec_down
                    ),
                };
            }
        }
    } else if hold_days <= 30 {
        l2_stop = py_round(l2_stop * 1.3, 2);
    }

    // L2 常规止损
    if profit_pct <= l2_stop {
        let l2_sell_pct = f64::max(30.0, l2_sell_pct_base - supplement_count as f64 * 10.0) as i32;
        return StopLossEval {
            level: Some("L2".to_string()),
            sell_pct: l2_sell_pct,
            reason: format!(
                "常规止损: 浮亏{:.1}%% ≤ 止损线{:.1}%，减仓{}%(已补仓{}次，保留反弹仓位)",
                profit_pct, l2_stop, l2_sell_pct, supplement_count
            ),
        };
    }

    // L1 预警
    let warning_threshold = l2_stop * STOP_LOSS_L1_FACTOR;
    if profit_pct <= warning_threshold {
        return StopLossEval {
            level: Some("L1".to_string()),
            sell_pct: 0,
            reason: format!(
                "止损预警: 浮亏{:.1}%%接近止损线{:.1}%%(预警线{:.1}%%)",
                profit_pct, l2_stop, warning_threshold
            ),
        };
    }

    StopLossEval {
        level: None,
        sell_pct: 0,
        reason: String::new(),
    }
}

// ============================================================
// 补仓禁入判断
// helpers.py:500 _is_supplement_forbidden
// ============================================================

// helpers.py:500 _is_supplement_forbidden
pub fn is_supplement_forbidden(
    tc: &TrendCtx,
    confidence: f64,
    source: &str,
    vol_state: &str,
    batches_sorted: &[Batch],
    today: &str,
) -> (bool, String) {
    if vol_state == "extreme_vol" {
        let vol = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(0.0));
        return (true, format!("波动率{}%处于极端水平，暂停补仓", vol));
    }

    let mid_10d = tc.mid_10d;
    let consecutive_down = tc.consecutive_down;
    let max_drawdown = tc.max_drawdown;
    let vol = tc.volatility.unwrap_or(0.0);

    if mid_10d.map_or(false, |m| m <= -10.0) && consecutive_down >= 5 {
        return (true, format!("10日累跌{}%且连跌{}天，趋势禁入", mid_10d.unwrap(), consecutive_down));
    }

    if max_drawdown >= 15.0 && vol >= 2.5 {
        return (true, format!("回撤{}%+波动率{}%，高风险禁入", max_drawdown, vol));
    }

    if source == "estimation" && confidence < 0.6 {
        return (true, format!("置信度{:.0}%偏低，盘中补仓禁入", confidence * 100.0));
    }

    // 豁免期内趋势恶化冻结补仓
    if !batches_sorted.is_empty() {
        let oldest = &batches_sorted[0];
        let oldest_hold_days = days_between(&oldest.buy_date, today);
        if oldest_hold_days < 20 {
            let short_5d = tc.short_5d;
            if consecutive_down >= 4 && short_5d.map_or(false, |s| s <= -7.0) {
                return (
                    true,
                    format!(
                        "豁免期内({}天)趋势恶化(连跌{}天+5日累跌{}%)，冻结补仓",
                        oldest_hold_days, consecutive_down, short_5d.unwrap()
                    ),
                );
            }
        }
    }

    (false, String::new())
}

// ============================================================
// 补仓节奏阀
// helpers.py:536 _check_supplement_rate_limit
// ============================================================

// helpers.py:536 _check_supplement_rate_limit
#[allow(clippy::too_many_arguments)]
pub fn check_supplement_rate_limit(
    batches: &[Batch],
    total_profit_pct: Option<f64>,
    current_nav: f64,
    nav_hist: &[NavDay],
    tc: &TrendCtx,
    rebuy_step: f64,
    today: &str,
) -> (bool, String, f64) {
    let holding_batches: Vec<&Batch> = batches.iter().filter(|b| b.is_holding()).collect();
    if holding_batches.is_empty() {
        return (false, String::new(), 1.0);
    }

    let trade_dates: Vec<&str> = nav_hist.iter().filter_map(|h| {
        if h.date.is_empty() { None } else { Some(h.date.as_str()) }
    }).collect();
    let today_str = today;

    let use_all_buys = total_profit_pct.map_or(false, |t| t < -3.0);

    let ref_batches: Vec<&Batch> = if use_all_buys {
        holding_batches.clone()
    } else {
        holding_batches.iter().filter(|b| b.is_supplement).cloned().collect()
    };

    let mut dynamic_gap = SUPPLEMENT_MIN_GAP_TRADE_DAYS;
    let vol = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(1.0));
    let short_3d = tc.short_3d.unwrap_or(0.0);
    let consecutive_down = tc.consecutive_down;

    if short_3d.abs() > vol * 2.0 && short_3d < 0.0 {
        dynamic_gap = f64::max(2.0, SUPPLEMENT_MIN_GAP_TRADE_DAYS as f64 - 1.0) as i32;
    } else if consecutive_down >= 5 {
        // 阴跌延长间隔：近5日每日|涨跌|<0.5 且均<0
        if let Some(last5) = recent_5_changes(nav_hist) {
            if last5.len() == 5 && last5.iter().all(|&c| c.abs() < 0.5 && c < 0.0) {
                dynamic_gap = f64::min(5.0, SUPPLEMENT_MIN_GAP_TRADE_DAYS as f64 + 2.0) as i32;
            }
        }
    }

    if !ref_batches.is_empty() {
        let latest = ref_batches
            .iter()
            .max_by_key(|b| &b.buy_date)
            .unwrap();
        let gap = count_trade_days_between(&latest.buy_date, today_str, &trade_dates);
        if gap < dynamic_gap {
            let scope = if use_all_buys { "所有买入" } else { "补仓" };
            return (
                true,
                format!("距上次{}仅{}个交易日(要求≥{})", scope, gap, dynamic_gap),
                1.0,
            );
        }

        let supplement_batches: Vec<&Batch> = holding_batches.iter().filter(|b| b.is_supplement).cloned().collect();
        if !supplement_batches.is_empty() {
            let latest_supp = supplement_batches.iter().max_by_key(|b| &b.buy_date).unwrap();
            let last_supp_nav = latest_supp.nav;
            if last_supp_nav > 0.0 && current_nav > 0.0 {
                let drop_from_last = (current_nav / last_supp_nav - 1.0) * 100.0;
                if drop_from_last > -rebuy_step {
                    return (
                        true,
                        format!(
                            "当前净值较上次补仓仅跌{:.1}%%(要求≥{:.1}%%)",
                            drop_from_last, rebuy_step
                        ),
                        1.0,
                    );
                }
            }
        }
    }

    let mut tier_factor = 1.0;
    let mid_10d = tc.mid_10d;
    if mid_10d.map_or(false, |m| m < -5.0) || consecutive_down >= 4 {
        tier_factor *= 0.7;
    }
    if vol > 2.2 {
        tier_factor *= 0.8;
    }

    (false, String::new(), tier_factor)
}

/// 取最近 5 个历史日收益（与 analyze_trend.recent_changes 口径一致，最老在前返回）
fn recent_5_changes(nav_hist: &[NavDay]) -> Option<Vec<f64>> {
    let trend_navs: Vec<&NavDay> = nav_hist.iter().collect();
    let mut changes: Vec<f64> = Vec::new();
    for i in 1..trend_navs.len() {
        let prev = trend_navs[i - 1].nav;
        let cur = trend_navs[i].nav;
        if cur > 0.0 {
            changes.push(py_round((prev / cur - 1.0) * 100.0, 2));
        }
    }
    if changes.len() >= 5 {
        Some(changes[..5].to_vec())
    } else {
        None
    }
}

// helpers.py:599 _count_trade_days_between
pub fn count_trade_days_between(date_from: &str, date_to: &str, trade_dates: &[&str]) -> i32 {
    let d_from = match parse_date(date_from) {
        Some(d) => d,
        None => return 999,
    };
    let d_to = match parse_date(date_to) {
        Some(d) => d,
        None => return 999,
    };
    let mut count = 0i32;
    for td in trade_dates {
        if let Some(td_d) = parse_date(td) {
            if d_from < td_d && td_d <= d_to {
                count += 1;
            }
        }
    }
    count
}

// ============================================================
// 冷却期判断
// helpers.py:617 _is_in_cooldown
// ============================================================

// helpers.py:617 _is_in_cooldown（FundLens 仅用 cooldown_sell_date + COOLDOWN_DAYS 常量）
pub fn is_in_cooldown(cooldown_sell_date: Option<&str>, nav_hist: &[NavDay], today: &str) -> bool {
    if let Some(sell_date) = cooldown_sell_date {
        let trade_dates: Vec<&str> = nav_hist.iter().map(|h| h.date.as_str()).collect();
        let gap = count_trade_days_between(sell_date, today, &trade_dates);
        return gap < COOLDOWN_DAYS;
    }
    false
}

// ============================================================
// 仓位乘数
// helpers.py:637 _calc_size_multiplier
// ============================================================

// helpers.py:637 _calc_size_multiplier
pub fn calc_size_multiplier(
    risk_mul: f64,
    confidence: f64,
    trend_label: &str,
    momentum_score: f64,
) -> f64 {
    let size_mul_risk = 1.0 / f64::max(0.8, risk_mul);

    let size_mul_conf = if confidence < 0.6 {
        0.6
    } else if confidence < 0.75 {
        0.8
    } else {
        1.0
    };

    let weak_labels = ["中期走弱", "连跌", "偏弱"];
    let strong_labels = ["中期走强", "偏强", "连涨"];
    let size_mul_trend = if weak_labels.contains(&trend_label) {
        0.8
    } else if strong_labels.contains(&trend_label) {
        1.30
    } else {
        1.0
    };

    let mut momentum_adj = 1.0;
    if momentum_score < -0.5 {
        momentum_adj = 0.75;
    } else if momentum_score < -0.3 {
        momentum_adj = 0.85;
    }

    let raw = size_mul_risk * size_mul_conf * size_mul_trend * momentum_adj;
    py_round(f64::max(0.40, f64::min(1.40, raw)), 2)
}

// ============================================================
// 净值估算（收盘/盘中）
// helpers.py:671 _estimate_current_nav
// ============================================================

// helpers.py:671 _estimate_current_nav
pub fn estimate_current_nav(
    oldest_nav: f64,
    today_change: f64,
    nav_hist: &[NavDay],
    market_closed: bool,
    today: &str,
) -> f64 {
    let today_str = today;
    if market_closed && !nav_hist.is_empty() {
        let latest = &nav_hist[0];
        if latest.nav.is_finite() {
            if latest.date == today_str {
                return latest.nav;
            }
            return latest.nav;
        }
    }
    if !nav_hist.is_empty() && nav_hist[0].nav.is_finite() {
        let yesterday_nav = nav_hist[0].nav;
        return yesterday_nav * (1.0 + today_change / 100.0);
    }
    oldest_nav * (1.0 + today_change / 100.0)
}

// ============================================================
// 盈亏计算
// helpers.py:701 _calc_batch_profit_pct
// ============================================================

// helpers.py:701 _calc_batch_profit_pct
pub fn calc_batch_profit_pct(batch: &Batch, current_nav: f64) -> f64 {
    if batch.nav <= 0.0 {
        return 0.0;
    }
    py_round((current_nav / batch.nav - 1.0) * 100.0, 2)
}

// helpers.py:707 _calc_total_profit_pct
pub fn calc_total_profit_pct(batches: &[Batch], current_nav: f64) -> f64 {
    let total_cost: f64 = batches.iter().map(|b| b.amount).sum();
    if total_cost <= 0.0 {
        return 0.0;
    }
    let total_value: f64 = batches.iter().map(|b| b.shares * current_nav).sum();
    py_round((total_value / total_cost - 1.0) * 100.0, 2)
}

// helpers.py:715 _get_take_profit_sell_pct
pub fn get_take_profit_sell_pct(profit_pct: f64) -> i32 {
    for &(threshold, pct) in TAKE_PROFIT_TIERS.iter() {
        if profit_pct > threshold {
            return pct as i32;
        }
    }
    50
}

// helpers.py:722 _get_slow_profit_sell_pct
pub fn get_slow_profit_sell_pct(profit_pct: f64) -> Option<i32> {
    for &(threshold, pct) in SLOW_PROFIT_TIERS.iter() {
        if profit_pct > threshold {
            return Some(pct as i32);
        }
    }
    None
}

// helpers.py:729 _calc_min_profit_buffer
pub fn calc_min_profit_buffer(fee_rate: f64, vol: f64) -> f64 {
    f64::max(1.5, fee_rate * 2.5 + f64::max(0.3, vol * 0.5))
}

// helpers.py:733 _get_trail_profit_sell_pct
pub fn get_trail_profit_sell_pct(peak_profit_pct: f64) -> i32 {
    for &(threshold, pct) in TRAIL_PROFIT_SELL_TIERS.iter() {
        if peak_profit_pct >= threshold {
            return pct as i32;
        }
    }
    30
}

// helpers.py:740 _calc_peak_profit（stored_peak = batch.peak_nav）
pub fn calc_peak_profit(batch: &Batch, nav_hist: &[NavDay]) -> f64 {
    let buy_nav = batch.nav;
    if buy_nav <= 0.0 {
        return 0.0;
    }

    let stored_peak = batch.peak_nav.unwrap_or(0.0);
    let peak_nav = if stored_peak > 0.0 && stored_peak > buy_nav {
        stored_peak
    } else {
        buy_nav
    };

    let buy_date = &batch.buy_date;
    let mut peak = peak_nav;
    for h in nav_hist {
        if h.date > *buy_date && h.nav > peak {
            peak = h.nav;
        }
    }

    py_round((peak / buy_nav - 1.0) * 100.0, 2)
}

// ============================================================
// FIFO 卖出计划
// helpers.py:779 _build_fifo_sell_plan
// ============================================================

/// helpers.py:779 的输入：单个卖出候选
pub struct FifoSellSignal {
    pub target_batch_id: String,
    pub sell_shares: f64,
}

// helpers.py:779 _build_fifo_sell_plan
/// fee_rate 用 input.sell_fee_rate 代替源的 get_sell_fee_rate(fund_code, hold_days)
pub fn build_fifo_sell_plan(
    batches_sorted: &[Batch],
    sell_signals: &[FifoSellSignal],
    current_nav: f64,
    fee_rate: f64,
    today: &str,
) -> FifoPlan {
    let mut target_sells: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for sig in sell_signals {
        let shares = sig.sell_shares;
        let entry = target_sells.entry(sig.target_batch_id.clone()).or_insert(0.0);
        if shares > *entry {
            *entry = shares;
        }
    }

    let batch_ids_ordered: Vec<String> = batches_sorted.iter().map(|b| b.id.clone()).collect();
    let mut last_target_idx = -1isize;
    for bid in target_sells.keys() {
        if let Some(idx) = batch_ids_ordered.iter().position(|b| b == bid) {
            if (idx as isize) > last_target_idx {
                last_target_idx = idx as isize;
            }
        }
    }

    let mut steps: Vec<FifoStep> = Vec::new();
    let mut total_fifo_shares = 0.0;

    for (i, batch) in batches_sorted.iter().enumerate() {
        if !batch.is_holding() {
            continue;
        }
        if (i as isize) > last_target_idx {
            break;
        }

        let bid = batch.id.clone();
        let hold_days = days_between(&batch.buy_date, today) as i64;

        let (shares, is_passthrough, reason) = if let Some(&t) = target_sells.get(&bid) {
            (t, false, String::new())
        } else {
            (
                batch.shares,
                true,
                "FIFO穿过（需先卖出此批次）".to_string(),
            )
        };

        let profit_pct = calc_batch_profit_pct(batch, current_nav);
        let est_gross = shares * current_nav;
        let est_fee = py_round(est_gross * fee_rate / 100.0, 2);
        let ratio = if batch.shares > 0.0 {
            shares / batch.shares
        } else {
            1.0
        };
        let est_net_profit = py_round(
            est_gross * (1.0 - fee_rate / 100.0) - batch.amount * ratio,
            2,
        );

        let is_full_sell = (shares - batch.shares).abs() < 0.01;

        steps.push(FifoStep {
            batch_id: bid,
            buy_date: batch.buy_date.clone(),
            sell_shares: py_round(shares, 2),
            batch_total_shares: py_round(batch.shares, 2),
            is_full_sell,
            is_passthrough,
            hold_days,
            fee_rate,
            profit_pct,
            estimated_fee: est_fee,
            estimated_net_profit: est_net_profit,
            reason,
            note: batch.note.clone(),
        });
        total_fifo_shares += shares;
    }

    let total_est_fee: f64 = steps.iter().map(|s| s.estimated_fee).sum();
    let total_est_profit: f64 = steps.iter().map(|s| s.estimated_net_profit).sum();
    let has_passthrough = steps.iter().any(|s| s.is_passthrough);

    let passthrough_loss_steps: Vec<&FifoStep> =
        steps.iter().filter(|s| s.is_passthrough && s.estimated_net_profit < 0.0).collect();
    let mut passthrough_warning: Option<String> = None;
    let mut passthrough_loss_total = 0.0;
    if !passthrough_loss_steps.is_empty() {
        passthrough_loss_total = passthrough_loss_steps.iter().map(|s| s.estimated_net_profit).sum();
        let ids: Vec<String> = passthrough_loss_steps.iter().map(|s| s.batch_id.clone()).collect();
        passthrough_warning = Some(format!(
            "注意: FIFO穿过的{}个批次({})预计亏损{:.2}元, 请确认是否值得为目标批次执行卖出",
            passthrough_loss_steps.len(),
            ids.join(", "),
            passthrough_loss_total
        ));
    }

    let instruction = format!("在支付宝输入卖出 {:.2} 份", total_fifo_shares);

    FifoPlan {
        total_shares: py_round(total_fifo_shares, 2),
        batch_count: steps.len(),
        steps,
        total_estimated_fee: py_round(total_est_fee, 2),
        total_estimated_profit: py_round(total_est_profit, 2),
        has_passthrough,
        passthrough_warning,
        passthrough_loss_total: py_round(passthrough_loss_total, 2),
        instruction,
    }
}
