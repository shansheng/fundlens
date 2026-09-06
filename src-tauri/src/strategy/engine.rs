//! strategy/engine.rs —— valuation_grid/grid/engine.py generate_signal 决策移植
//!
//! 移植铁律：纯逻辑，不碰 DB/网络/时钟；today 由 `StrategyInput.today` 注入。
//!
//! 对应源：engine.py:391 `generate_signal`。保留两条主链路——
//!   1) 有持仓分支（遍历批次生成候选 → `_is_higher_priority` 候选择优）；
//!   2) 空仓分支（if/return 级联：大跌抄底→趋势建仓→温和回调→连跌低吸→冷却后建仓→观望）。
//!
//! 优先级谱系（源 P1→P8，数值越小越优先）原样保留；因 model.SignalCandidate.priority 为 i32，
//! 内部用 `python优先级 × 10` 存储（如 1.2→12）以保住 1<1.2<1.5<2 的精细次序，
//! 输出 GridSignal.priority 再除以 10 还原为 1..8 整数档位。
//!
//! P0 降级/占位（见任务书回报项 3）：
//!   - 延迟回补挂单（pending_rebuy）触发与创建均为 store/P2，本文件不实现（跳过）；
//!   - `_update_batch_peak_nav` 为写库副作用，改为直接读 `batch.peak_nav`；
//!   - `_build_market_analysis` / `_append_signal_history` 为展示/落库，跳过；
//!   - 信号胜率 `signal_stats` 在 P0 传 None（不收紧）。

use crate::strategy::config::*;
use crate::strategy::helpers::*;
use crate::strategy::model::*;

/// 用源 python 优先级浮点构造候选（内部 ×10 编码保序）
fn cand(fund_code: &str, name: &str, action: &str, py_pri: f64) -> SignalCandidate {
    SignalCandidate::new(fund_code, name, action, (py_pri * 10.0).round() as i32)
}

/// 买入金额（需要 max_position 才出金额；否则 amount=None，仅给方向/档位）
fn buy_amount(max_position: Option<f64>, ratio: f64, size_mul: f64) -> Option<f64> {
    max_position.map(|mp| py_round(mp * ratio * size_mul, 2))
}

/// 主入口：等价 engine.py:391 generate_signal
pub fn compute_signal(input: &StrategyInput) -> GridSignal {
    let fund_code = &input.fund_code;
    let today = &input.today;
    let source = &input.source;
    let confidence = input.confidence;

    // ===== 趋势上下文（engine.py:442 _analyze_trend）=====
    let tc = analyze_trend(input.today_change, &input.nav_hist, today, source);

    // ===== 行情模式参数（engine.py:447-448）=====
    let rp = regime_params(&input.regime);

    // ===== 动态阈值（engine.py:458）=====
    let (mut dst, risk_multiplier) = calc_dynamic_thresholds(&tc, confidence, source, input.vol_sensitivity, None);

    // v5.13: 行情模式覆盖补仓档位首次建仓比例
    if input.regime != "neutral" && !dst.supplement_tiers.is_empty() {
        let mut t = dst.supplement_tiers.clone();
        t[0] = (t[0].0, rp.first_build_ratio, t[0].2, t[0].3);
        dst.supplement_tiers = t;
    }

    let vol_state = dst.vol_state.clone();
    let momentum = dst.momentum_score;

    let in_cooldown = is_in_cooldown(input.cooldown_sell_date.as_deref(), &input.nav_hist, today);

    // ============================================================
    // 延迟回补挂单触发检查（engine.py:498-537）——在所有其他判断之前：
    // 净值跌到挂单 trigger_nav 时立即触发回补，直接返回 buy 信号（priority 5）
    // ============================================================
    if let Some(pr) = &input.pending_rebuy {
        // 估算当前净值（有持仓用最早 holding 批次锚定；无持仓用最新净值×(1+今日涨跌)）
        let nav_for_check = if input.has_position() {
            let oldest_holding = input
                .batches
                .iter()
                .find(|b| b.is_holding())
                .map(|b| b.nav)
                .unwrap_or(0.0);
            if oldest_holding > 0.0 {
                estimate_current_nav(oldest_holding, input.today_change, &input.nav_hist, input.market_closed, today)
            } else {
                input.nav_hist.first().map(|n| n.nav * (1.0 + input.today_change / 100.0)).unwrap_or(0.0)
            }
        } else {
            input.nav_hist.first().map(|n| n.nav * (1.0 + input.today_change / 100.0)).unwrap_or(0.0)
        };
        if nav_for_check > 0.0 && nav_for_check <= pr.trigger_nav {
            // 触发！金额按持仓上限/可用空间截断（非空仓时）
            let mut amount = pr.amount;
            if input.has_position() {
                let total_cost: f64 = input.batches.iter().map(|b| b.amount).sum();
                let remaining_cap = input.max_position.map(|m| m - total_cost).unwrap_or(f64::MAX);
                amount = if remaining_cap > 0.0 { amount.min(remaining_cap) } else { 0.0 };
            }
            if amount >= 10.0 {
                let mut sig = cand(fund_code, pr.signal_label.as_str(), "buy", 5.0);
                sig.reason = format!(
                    "净值{:.4}≤触发价{:.4}(卖出价{:.4}), 延迟回补{:.0}元 (源自{})",
                    nav_for_check, pr.trigger_nav, pr.sell_nav, amount, pr.source_signal
                );
                let mut gs = to_grid_signal(&sig, input, Some(nav_for_check), None);
                gs.is_rebuy = true;
                gs.pending_rebuy_id = Some(pr.id);
                return gs;
            }
        }
    }

    // ===== 仓位乘数（engine.py:539）=====
    let mut size_mul = calc_size_multiplier(risk_multiplier, confidence, &tc.trend_label, momentum);
    size_mul = f64::min(size_mul, rp.size_mul_cap);

    // ===== 有持仓分支（engine.py:548）=====
    if input.has_position() {
        let mut batches_sorted = input.batches.clone();
        batches_sorted.sort_by(|a, b| a.buy_date.cmp(&b.buy_date));

        let oldest_nav = batches_sorted.first().map(|b| b.nav).unwrap_or(0.0);
        let current_nav = estimate_current_nav(
            oldest_nav,
            input.today_change,
            &input.nav_hist,
            input.market_closed,
            today,
        );

        let total_profit_pct = input
            .total_profit_pct
            .unwrap_or_else(|| calc_total_profit_pct(&batches_sorted, current_nav));

        // 同日卖出抑制
        let sold_today = input.cooldown_sell_date.as_deref() == Some(today.as_str());

        let mut best_signal: Option<SignalCandidate> = None;
        let mut all_signals: Vec<SignalCandidate> = Vec::new();
        let mut extra_alerts: Vec<String> = Vec::new();
        // FIFO 卖出计划经 SignalCandidate 不能直接携带（model 锁定无该字段），用局部变量透传
        let mut final_fifo_plan: Option<FifoPlan> = None;
        let supplement_count = batches_sorted
            .iter()
            .filter(|b| b.is_holding() && b.is_supplement)
            .count() as i32;

        let fee_rate = input.sell_fee_rate;
        let total_cost: f64 = batches_sorted.iter().filter(|b| b.is_holding()).map(|b| b.amount).sum();

        for batch in &batches_sorted {
            let hold_days = days_between(&batch.buy_date, today);
            let profit_pct = calc_batch_profit_pct(batch, current_nav);

            // --- 三级止损评估（engine.py:584）---
            let stop_eval = evaluate_stop_loss(
                profit_pct,
                dst.stop_loss_adj,
                hold_days,
                fee_rate,
                &tc,
                confidence,
                source,
                supplement_count,
                input.today_change,
                rp.l2_stop_loss_base,
                batch.is_rebuy,
            );

            if stop_eval.level.as_deref() == Some("L3") {
                let l3_sell_pct = stop_eval.sell_pct;
                let l3_sell_shares = py_round(batch.shares * l3_sell_pct as f64 / 100.0, 2);
                let mut sig = cand(fund_code, "极端止损(L3)", "sell", 1.0);
                sig.target_batch_id = Some(batch.id.clone());
                sig.sell_shares = Some(l3_sell_shares);
                sig.sell_pct = Some(l3_sell_pct as f64);
                sig.reason = stop_eval.reason;
                sig.fee_info = Some(format!(
                    "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                    fee_rate,
                    py_round(l3_sell_shares * current_nav * fee_rate / 100.0, 2),
                    py_round(l3_sell_shares * current_nav * (1.0 - fee_rate / 100.0)
                        - batch.amount * l3_sell_pct as f64 / 100.0, 2)
                ));
                all_signals.push(sig.clone());
                if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                    best_signal = Some(sig);
                }
                continue;
            } else if stop_eval.level.as_deref() == Some("L2") {
                let sell_shares = py_round(batch.shares * stop_eval.sell_pct as f64 / 100.0, 2);
                let mut sig = cand(fund_code, "常规止损(L2)", "sell", 1.0);
                sig.sub_priority = 1.0;
                sig.target_batch_id = Some(batch.id.clone());
                sig.sell_shares = Some(sell_shares);
                sig.sell_pct = Some(stop_eval.sell_pct as f64);
                sig.reason = stop_eval.reason;
                sig.fee_info = Some(format!(
                    "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                    fee_rate,
                    py_round(sell_shares * current_nav * fee_rate / 100.0, 2),
                    py_round(sell_shares * current_nav * (1.0 - fee_rate / 100.0)
                        - batch.amount * stop_eval.sell_pct as f64 / 100.0, 2)
                ));
                all_signals.push(sig.clone());
                if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                    best_signal = Some(sig);
                }
                continue;
            } else if stop_eval.level.as_deref() == Some("L1") {
                extra_alerts.push(stop_eval.reason);
            }

            // --- 灾难保护阀（未满7天）（engine.py:643）---
            if hold_days < 7 {
                let mut disaster_triggered = false;
                let mut disaster_reason = String::new();
                let disaster_sell_pct = f64::max(30.0, DISASTER_SELL_PCT_EXTREME - supplement_count as f64 * 10.0) as i32;

                let effective_disaster = f64::min(dst.disaster_loss_threshold, dst.stop_loss_adj * 1.5);
                if profit_pct <= effective_disaster {
                    disaster_triggered = true;
                    disaster_reason = format!(
                        "批次{}仅{}天, 亏损{:.1}%% ≤ 灾难线{:.1}%",
                        batch.id, hold_days, profit_pct, effective_disaster
                    );
                }

                if !disaster_triggered
                    && input.today_change <= dst.disaster_daily_drop
                    && tc.consecutive_down >= DISASTER_CONSECUTIVE_DOWN
                {
                    let sell_pct = f64::max(20.0, DISASTER_SELL_PCT_DAILY - supplement_count as f64 * 5.0) as i32;
                    disaster_reason = format!(
                        "批次{}仅{}天, 今日暴跌{:.1}%%+连跌, 灾难保护",
                        batch.id, hold_days, input.today_change
                    );
                    let sell_shares = py_round(batch.shares * sell_pct as f64 / 100.0, 2);
                    let mut sig = cand(fund_code, "灾难保护(未满7天)", "sell", 1.2);
                    sig.target_batch_id = Some(batch.id.clone());
                    sig.sell_shares = Some(sell_shares);
                    sig.sell_pct = Some(sell_pct as f64);
                    sig.reason = disaster_reason;
                    sig.fee_info = Some(format!("sell_fee_rate={}", fee_rate));
                    sig.alert = true;
                    sig.alert_msg = Some(format!("灾难保护卖出将产生{}%高费率", fee_rate));
                    all_signals.push(sig.clone());
                    if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                        best_signal = Some(sig);
                    }
                    continue;
                }

                if disaster_triggered {
                    let sell_pct = disaster_sell_pct;
                    let sell_shares = py_round(batch.shares * sell_pct as f64 / 100.0, 2);
                    let mut sig = cand(fund_code, "灾难保护(未满7天)", "sell", 1.2);
                    sig.target_batch_id = Some(batch.id.clone());
                    sig.sell_shares = Some(sell_shares);
                    sig.sell_pct = Some(sell_pct as f64);
                    sig.reason = disaster_reason;
                    sig.fee_info = Some(format!("sell_fee_rate={}", fee_rate));
                    sig.alert = true;
                    sig.alert_msg = Some(format!("灾难保护卖出将产生{}%高费率", fee_rate));
                    all_signals.push(sig.clone());
                    if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                        best_signal = Some(sig);
                    }
                    continue;
                }

                if profit_pct <= -3.0 {
                    extra_alerts.push(format!("批次{}亏损{:.1}%%但仅持有{}天", batch.id, profit_pct, hold_days));
                }

                // 短期深亏安全网
                if !disaster_triggered && profit_pct <= -8.0 {
                    let safety_sell_pct = 30;
                    let sell_shares_sn = py_round(batch.shares * safety_sell_pct as f64 / 100.0, 2);
                    let mut sig = cand(fund_code, "短期深亏止损(安全网)", "sell", 1.5);
                    sig.target_batch_id = Some(batch.id.clone());
                    sig.sell_shares = Some(sell_shares_sn);
                    sig.sell_pct = Some(safety_sell_pct as f64);
                    sig.reason = format!(
                        "批次{}仅{}天, 亏损{:.1}%%已超6%, 安全网减仓{}%",
                        batch.id, hold_days, profit_pct, safety_sell_pct
                    );
                    sig.fee_info = Some(format!("sell_fee_rate={}", fee_rate));
                    sig.alert = true;
                    sig.alert_msg = Some(format!(
                        "短期深亏安全网：仅持有{}天即亏损{:.1}%，建议减仓止损",
                        hold_days, profit_pct
                    ));
                    all_signals.push(sig.clone());
                    if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                        best_signal = Some(sig);
                    }
                }

                continue;
            }

            if hold_days < 7 {
                continue;
            }

            // --- 统一止盈评分（engine.py:714）---
            let peak_profit = calc_peak_profit(batch, &input.nav_hist);

            let mut nz_bonus = 0.0;
            if total_profit_pct > 0.0
                && batches_sorted.len() >= 2
                && batch.id == batches_sorted[0].id
            {
                nz_bonus = f64::min(12.0, total_profit_pct * 2.0);
                let oldest_hd = days_between(&batches_sorted[0].buy_date, today);
                if oldest_hd < 15 {
                    nz_bonus *= 0.5;
                }
            }

            let sell_eval = calc_sell_score(
                batch,
                current_nav,
                input.today_change,
                &tc,
                &dst,
                fee_rate,
                hold_days,
                peak_profit,
                nz_bonus,
                rp.tp_suppress_threshold,
            );

            if sell_eval.sell_pct > 0 && sell_eval.signal_name.is_some() {
                let sell_pct = sell_eval.sell_pct as f64;
                let mut effective_name = sell_eval.signal_name.clone().unwrap();
                if nz_bonus > 0.0 && batch.id == batches_sorted[0].id {
                    effective_name = format!("扭亏{}", effective_name);
                }

                let sell_shares = py_round(batch.shares * sell_pct / 100.0, 2);
                let est_gross = sell_shares * current_nav;
                let est_fee = py_round(est_gross * fee_rate / 100.0, 2);
                let est_net_profit = py_round(
                    est_gross * (1.0 - fee_rate / 100.0) - batch.amount * sell_pct / 100.0,
                    2,
                );

                let is_low_conf = source == "estimation" && confidence < 0.5;
                let is_suppressed = sold_today;
                let mut sig = cand(fund_code, &effective_name, "sell", 2.0);
                sig.sub_priority = f64::max(0.0, 10.0 - sell_eval.score);
                sig.target_batch_id = Some(batch.id.clone());
                sig.sell_shares = Some(sell_shares);
                sig.sell_pct = Some(sell_pct);
                let post = if is_low_conf {
                    "(待确认)"
                } else if is_suppressed {
                    "(今日已操作)"
                } else {
                    ""
                };
                let mut name_disp = effective_name;
                if !post.is_empty() {
                    name_disp.push_str(post);
                }
                sig.signal_name = name_disp;
                sig.action = if is_low_conf || is_suppressed { "hold".to_string() } else { "sell".to_string() };
                sig.reason = format!(
                    "持有{}天, 浮盈{:.1}%%, 峰值{:.1}%%, {}, 卖出{:.0}%{}",
                    hold_days,
                    sell_eval.profit_pct,
                    peak_profit,
                    sell_eval.reason,
                    sell_pct,
                    if is_low_conf {
                        format!(", 置信度{:.0}%偏低", confidence * 100.0)
                    } else {
                        String::new()
                    }
                );
                if is_suppressed {
                    sig.reason.push_str(" (今日已执行卖出，次日再评估)");
                }
                sig.fee_info = Some(format!(
                    "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                    fee_rate, est_fee, est_net_profit
                ));
                sig.alert = is_low_conf || is_suppressed;
                all_signals.push(sig.clone());
                if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                    best_signal = Some(sig);
                }
                continue;
            }

            // --- 趋势转弱卖出（engine.py:782）---
            let vol = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(1.0));
            let min_profit_buffer = calc_min_profit_buffer(fee_rate, vol);
            let mid_10d_val = tc.mid_10d;
            let short_5d_val = tc.short_5d;

            let mut has_trend_confirm = false;
            if tc.recent_changes.len() >= 3
                && tc.recent_changes[..3].iter().all(|&c| c < 0.0)
            {
                let cumulative_drop: f64 = tc.recent_changes[..3].iter().sum();
                let trend_weak_thresh = dst.trend_weak_cumulative * 1.5;
                if cumulative_drop <= trend_weak_thresh {
                    if short_5d_val.map_or(false, |s| s < -2.0)
                        || mid_10d_val.map_or(false, |m| m < -3.0)
                    {
                        has_trend_confirm = true;
                    }
                }
            }

            let volume_proxy = tc.volume_proxy;
            if has_trend_confirm {
                if let Some(vp) = volume_proxy {
                    if vp > 1.5 {
                        has_trend_confirm = true;
                    } else if vp < 0.5 {
                        has_trend_confirm = false;
                    }
                }
            }

            if profit_pct > min_profit_buffer && has_trend_confirm {
                let is_low_conf = source == "estimation" && confidence < 0.5;
                let is_suppressed = sold_today;
                let trend_sell_pct = if total_profit_pct < 1.0 {
                    70
                } else if total_profit_pct < 3.0 {
                    50
                } else {
                    30
                };
                let mut trend_sell_pct = trend_sell_pct;
                if let Some(vp) = volume_proxy {
                    if vp > 1.5 {
                        trend_sell_pct = f64::min(100.0, trend_sell_pct as f64 + 20.0) as i32;
                    }
                }
                let sell_shares_tw = py_round(batch.shares * trend_sell_pct as f64 / 100.0, 2);
                let mut name = "趋势转弱".to_string();
                if is_suppressed {
                    name.push_str("(今日已操作)");
                } else if is_low_conf {
                    name.push_str("(待确认)");
                }
                let mut sig = cand(fund_code, &name, "sell", 3.0);
                sig.target_batch_id = Some(batch.id.clone());
                sig.sell_shares = Some(sell_shares_tw);
                sig.sell_pct = Some(trend_sell_pct as f64);
                sig.reason = format!(
                    "持有{}天, 浮盈{:.1}%%, 总浮盈{:.1}%%, 趋势确认转弱, 减仓{}%{}",
                    hold_days,
                    profit_pct,
                    total_profit_pct,
                    trend_sell_pct,
                    if let Some(vp) = volume_proxy {
                        if vp > 1.5 {
                            format!("(放量{:.1}×)", vp)
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    }
                );
                if is_suppressed {
                    sig.reason.push_str(" (今日已执行卖出，次日再评估)");
                }
                sig.fee_info = Some(format!(
                    "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                    fee_rate,
                    py_round(sell_shares_tw * current_nav * fee_rate / 100.0, 2),
                    py_round(sell_shares_tw * current_nav * (1.0 - fee_rate / 100.0)
                        - batch.amount * trend_sell_pct as f64 / 100.0, 2)
                ));
                sig.alert = is_low_conf || is_suppressed;
                sig.action = if is_low_conf || is_suppressed { "hold".to_string() } else { "sell".to_string() };
                all_signals.push(sig.clone());
                if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                    best_signal = Some(sig);
                }
                continue;
            }
        }

        // --- 总仓位风控（engine.py:857）---
        if total_profit_pct < 0.0 && best_signal.is_none() {
            let oldest = &batches_sorted[0];
            let oldest_hd = days_between(&oldest.buy_date, today);
            let supp_count = supplement_count;
            let dyn_max_supp = f64::min(
                calc_dynamic_supplement_max(input.max_position, &batches_sorted) as f64,
                rp.supplement_max_count as f64,
            ) as i32;

            if total_profit_pct <= -15.0 && oldest_hd >= 7 {
                let oldest_fr = input.sell_fee_rate;
                let portfolio_sell_pct = if total_profit_pct <= -25.0 {
                    70
                } else if total_profit_pct <= -20.0 {
                    50
                } else {
                    35
                };
                let sell_shares = py_round(oldest.shares * portfolio_sell_pct as f64 / 100.0, 2);
                let mut sig = cand(fund_code, "灾难保底减仓", "sell", 1.0);
                sig.sub_priority = 2.0;
                sig.target_batch_id = Some(oldest.id.clone());
                sig.sell_shares = Some(sell_shares);
                sig.sell_pct = Some(portfolio_sell_pct as f64);
                sig.reason = format!(
                    "总浮亏{:.1}%% ≤ -15%灾难线, 最老批次减仓{}%",
                    total_profit_pct, portfolio_sell_pct
                );
                if sold_today {
                    sig.reason.push_str(" (今日已执行卖出，次日再评估)");
                    sig.action = "hold".to_string();
                }
                sig.fee_info = Some(format!(
                    "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                    oldest_fr,
                    py_round(sell_shares * current_nav * oldest_fr / 100.0, 2),
                    py_round(sell_shares * current_nav * (1.0 - oldest_fr / 100.0)
                        - oldest.amount * portfolio_sell_pct as f64 / 100.0, 2)
                ));
                sig.alert = true;
                sig.alert_msg = Some(format!("总仓位浮亏{:.1}%触发灾难保底减仓", total_profit_pct));
                all_signals.push(sig.clone());
                if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                    best_signal = Some(sig);
                }
            } else if oldest_hd < 20 {
                if total_profit_pct <= -8.0 {
                    extra_alerts.push(format!(
                        "总仓位浮亏{:.1}%%，但最老批次仅持有{}天(<20天豁免期)，暂不减仓",
                        total_profit_pct, oldest_hd
                    ));
                }
            } else if oldest_hd >= 20 {
                let trend_label = &tc.trend_label;
                let consecutive_down = tc.consecutive_down;
                let _short_5d_val = tc.short_5d;
                let mid_10d_val = tc.mid_10d;

                let mut trend_confirm_reasons: Vec<String> = Vec::new();
                if consecutive_down >= 6 {
                    trend_confirm_reasons.push(format!("连跌{}天", consecutive_down));
                }
                if mid_10d_val.map_or(false, |m| m <= -10.0) {
                    trend_confirm_reasons.push(format!("10日累跌{:.1}%", mid_10d_val.unwrap()));
                }
                if trend_label == "连跌" || trend_label == "中期走弱" {
                    trend_confirm_reasons.push(format!("趋势:{}", trend_label));
                }

                let mut trend_deteriorating = trend_confirm_reasons.len() >= 2;
                let remaining_supp = f64::max(0.0, dyn_max_supp as f64 - supp_count as f64);
                if remaining_supp == 0.0 && total_profit_pct <= -8.0 && trend_confirm_reasons.len() >= 1 {
                    trend_deteriorating = true;
                    trend_confirm_reasons.push(format!("补仓{}/{}已用尽", supp_count, dyn_max_supp));
                }

                if trend_deteriorating && oldest_hd >= 7 {
                    let oldest_fr = input.sell_fee_rate;
                    let portfolio_sell_pct = if total_profit_pct <= -12.0 {
                        50
                    } else if total_profit_pct <= -8.0 {
                        35
                    } else if total_profit_pct <= -6.0 {
                        25
                    } else {
                        0
                    };
                    if portfolio_sell_pct > 0 {
                        let sell_shares = py_round(oldest.shares * portfolio_sell_pct as f64 / 100.0, 2);
                        let mut sig = cand(fund_code, "趋势确认减仓", "sell", 1.0);
                        sig.sub_priority = 2.0;
                        sig.target_batch_id = Some(oldest.id.clone());
                        sig.sell_shares = Some(sell_shares);
                        sig.sell_pct = Some(portfolio_sell_pct as f64);
                        sig.reason = format!(
                            "总浮亏{:.1}%%, 持仓{}天, 趋势恶化确认({}), 最老批次减仓{}%",
                            total_profit_pct,
                            oldest_hd,
                            trend_confirm_reasons.join("+"),
                            portfolio_sell_pct
                        );
                        if sold_today {
                            sig.reason.push_str(" (今日已执行卖出，次日再评估)");
                            sig.action = "hold".to_string();
                        }
                        sig.fee_info = Some(format!(
                            "sell_fee_rate={}, estimated_fee={}, estimated_net_profit={}",
                            oldest_fr,
                            py_round(sell_shares * current_nav * oldest_fr / 100.0, 2),
                            py_round(sell_shares * current_nav * (1.0 - oldest_fr / 100.0)
                                - oldest.amount * portfolio_sell_pct as f64 / 100.0, 2)
                        ));
                        sig.alert = true;
                        sig.alert_msg = Some(format!("总仓位浮亏{:.1}%+趋势恶化，确认减仓", total_profit_pct));
                        all_signals.push(sig.clone());
                        if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                            best_signal = Some(sig);
                        }
                    }
                } else if total_profit_pct <= -8.0 {
                    extra_alerts.push(format!(
                        "总仓位浮亏{:.1}%%, 持仓{}天, 趋势信号({}), 暂未触发减仓",
                        total_profit_pct,
                        oldest_hd,
                        if trend_confirm_reasons.is_empty() {
                            "无".to_string()
                        } else {
                            trend_confirm_reasons.join(",")
                        }
                    ));
                }
            }
        }

        // --- 递进补仓（engine.py:991）---
        let dynamic_max_supp = f64::min(
            calc_dynamic_supplement_max(input.max_position, &batches_sorted) as f64,
            rp.supplement_max_count as f64,
        ) as i32;

        let (forbidden, forbid_reason) = is_supplement_forbidden(
            &tc,
            confidence,
            source,
            &vol_state,
            &batches_sorted,
            today,
        );

        if forbidden {
            if total_profit_pct < -3.0 {
                extra_alerts.push(format!("补仓被禁入: {}", forbid_reason));
            }
        } else if supplement_count < dynamic_max_supp
            && (total_cost < input.max_position.unwrap_or(f64::INFINITY))
            && !in_cooldown
        {
            if let Some(max_pos) = input.max_position {
                let rebuy_step = dst.rebuy_step;
                let (rate_blocked, rate_reason, tier_factor) = check_supplement_rate_limit(
                    &batches_sorted,
                    Some(total_profit_pct),
                    current_nav,
                    &input.nav_hist,
                    &tc,
                    rebuy_step,
                    today,
                );
                if rate_blocked {
                    if total_profit_pct < -3.0 {
                        extra_alerts.push(format!("补仓受节奏阀限制: {}", rate_reason));
                    }
                } else {
                    for &(tier_count, tier_ratio, tier_trigger, tier_loss_min) in &dst.supplement_tiers {
                        if supplement_count == tier_count {
                            if total_profit_pct <= tier_loss_min
                                && input.today_change <= tier_trigger
                            {
                                let risk_budget = max_pos - total_cost;
                                let effective_ratio = tier_ratio * tier_factor;
                                let mut supplement_amount =
                                    py_round(risk_budget * effective_ratio, 2);
                                let cap = max_pos * SUPPLEMENT_CAP_RATIO;
                                supplement_amount =
                                    py_round(f64::min(f64::min(supplement_amount, cap), risk_budget), 2);
                                supplement_amount = py_round(supplement_amount * size_mul, 2);

                                let min_efficiency =
                                    0.025 * (5000.0 / f64::max(1000.0, f64::max(1.0, max_pos)));
                                let efficiency = calc_cost_repair_efficiency(
                                    &batches_sorted,
                                    current_nav,
                                    supplement_amount,
                                );
                                if efficiency < min_efficiency
                                    && supplement_amount > 500.0
                                    && total_profit_pct > -5.0
                                {
                                    extra_alerts.push(format!(
                                        "补仓效率偏低({:.4}%/千元 < {:.4}%), 建议等待更大跌幅后补仓",
                                        efficiency, min_efficiency
                                    ));
                                    break;
                                }

                                if supplement_amount > 0.0 {
                                    let mut sig = cand(
                                        fund_code,
                                        &format!(
                                            "补仓(第{}次/上限{})",
                                            supplement_count + 1,
                                            dynamic_max_supp
                                        ),
                                        "buy",
                                        4.0,
                                    );
                                    sig.amount = Some(supplement_amount);
                                    sig.reason = format!(
                                        "总浮亏{:.1}%%, 今日跌{:.1}%%, 补仓{:.0}元(成本修复效率{:.4}%/千元)",
                                        total_profit_pct, input.today_change, supplement_amount, efficiency
                                    );
                                    all_signals.push(sig.clone());
                                    if best_signal.is_none()
                                        || is_higher_priority(&sig, best_signal.as_ref().unwrap())
                                    {
                                        best_signal = Some(sig);
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
            }
        }

        // --- 冷却期后加仓（engine.py:1056）---
        if !in_cooldown
            && input.cooldown_sell_date.is_some()
            && total_cost < input.max_position.unwrap_or(f64::INFINITY) * 0.8
            && total_profit_pct < -2.0
            && input.today_change <= 0.3
            && !forbidden
        {
            if let Some(max_pos) = input.max_position {
                let remaining = max_pos - total_cost;
                // v5.5 优化: 加仓比例从30%→50%（源为 *0.5）
                let rebuy_amount = py_round(
                    f64::min(remaining * 0.5, total_cost * 0.5) * size_mul,
                    2,
                );
                if rebuy_amount >= 100.0 {
                    let mut sig = cand(fund_code, "冷却期后加仓", "buy", 4.0);
                    sig.amount = Some(rebuy_amount);
                    sig.reason = format!(
                        "冷却期结束, 总浮亏{:.1}%%, 加仓{:.0}元",
                        total_profit_pct, rebuy_amount
                    );
                    all_signals.push(sig.clone());
                    if best_signal.is_none() || is_higher_priority(&sig, best_signal.as_ref().unwrap()) {
                        best_signal = Some(sig);
                    }
                }
            }
        }

        // --- 汇总（engine.py:1078）---
        if let Some(mut best) = best_signal {
            if !extra_alerts.is_empty() {
                if best.alert_msg.is_none() {
                    best.alert = true;
                    best.alert_msg = Some(extra_alerts.join("; "));
                } else {
                    best.alert_msg = Some(format!("{}; {}", best.alert_msg.unwrap(), extra_alerts.join("; ")));
                }
            }

            // FIFO 穿透降级（engine.py:1149-1187）—— 纯逻辑，保留
            let sell_signals: Vec<FifoSellSignal> = all_signals
                .iter()
                .filter(|s| s.action == "sell" && s.target_batch_id.is_some())
                .map(|s| FifoSellSignal {
                    target_batch_id: s.target_batch_id.clone().unwrap(),
                    sell_shares: s.sell_shares.unwrap_or(0.0),
                })
                .collect();

            if !sell_signals.is_empty() {
                let fifo_plan = build_fifo_sell_plan(
                    &batches_sorted,
                    &sell_signals,
                    current_nav,
                    fee_rate,
                    today,
                );
                let best_priority = best.priority;
                if fifo_plan.has_passthrough && best_priority >= 20 {
                    let loss_total = fifo_plan.passthrough_loss_total;
                    let total_est_profit = fifo_plan.total_estimated_profit;
                    let min_net_profit = f64::max(
                        PASSTHROUGH_MIN_NET_PROFIT_ABS,
                        total_cost * PASSTHROUGH_MIN_NET_PROFIT_RATIO,
                    );
                    let mut should_downgrade = false;
                    let mut downgrade_reason = String::new();
                    if total_est_profit < min_net_profit {
                        should_downgrade = true;
                        downgrade_reason = format!(
                            "净收益{:.0}元 < 门槛{:.0}元",
                            total_est_profit, min_net_profit
                        );
                    }
                    if !should_downgrade
                        && loss_total < 0.0
                        && total_est_profit > 0.0
                        && loss_total.abs() > total_est_profit * PASSTHROUGH_LOSS_RATIO_THRESHOLD
                    {
                        should_downgrade = true;
                        downgrade_reason = format!(
                            "穿透亏损{:.0}元 > 总利润{:.0}元×{:.0}%",
                            loss_total, total_est_profit, PASSTHROUGH_LOSS_RATIO_THRESHOLD * 100.0
                        );
                    }
                    if should_downgrade {
                        for sig_item in all_signals.iter_mut() {
                            if sig_item.action == "sell" && sig_item.priority >= 20 {
                                sig_item.action = "hold".to_string();
                                sig_item.signal_name.push_str("(穿透亏损过大)");
                                sig_item.reason.push_str(&format!(" → {}", downgrade_reason));
                                sig_item.alert = true;
                            }
                        }
                        // 重新择优
                        best_signal = None;
                        for s in &all_signals {
                            if best_signal.is_none()
                                || is_higher_priority(s, best_signal.as_ref().unwrap())
                            {
                                best_signal = Some(s.clone());
                            }
                        }
                        if let Some(b) = &mut best_signal {
                            best = b.clone();
                        }
                    }
                }
                final_fifo_plan = Some(fifo_plan);
            }

            let mut gs = to_grid_signal(&best, input, Some(current_nav), Some(total_profit_pct));
            gs.fifo_plan = final_fifo_plan;

            // v5.11 延迟回补建单建议（engine.py:1096-1131）：止盈卖出且趋势不弱 → 记录回补触发价，
            // 净值回落到 trigger_nav 才回补（避免 v5.10 买在止盈价被 L2 打穿的教训）
            const REBUY_NAMES: [&str; 8] = [
                "分批止盈", "慢涨止盈", "止盈卖出", "强势止盈",
                "扭亏分批止盈", "扭亏慢涨止盈", "扭亏止盈卖出", "扭亏强势止盈",
            ];
            let is_tp_sell = best.action == "sell"
                && REBUY_NAMES.iter().any(|sn| best.signal_name.starts_with(sn));
            if is_tp_sell {
                let trend = &tc.trend_label;
                let mut ratio = 0.0;
                let mut discount = 0.0;
                if rp.rebuy_discount > 0.0 {
                    if trend == "连涨" || trend == "偏强" || trend == "中期走强" {
                        ratio = 0.70;
                        discount = rp.rebuy_discount;
                    } else if trend == "震荡" && input.today_change >= -0.5 {
                        ratio = 0.40;
                        discount = rp.rebuy_discount + 0.010;
                    }
                    // 其余（连跌/偏弱/中期走弱）弱趋势不回补
                }
                if ratio > 0.0 {
                    let sell_amount_est = best.sell_shares.unwrap_or(0.0) * current_nav;
                    let mut rebuy_amount = sell_amount_est * ratio;
                    let rebuy_cap = input.max_position.map(|m| m - total_cost + sell_amount_est).unwrap_or(f64::MAX);
                    rebuy_amount = rebuy_amount.min(rebuy_cap * 0.8);
                    if rebuy_amount >= 100.0 {
                        let trigger_nav = ((current_nav * (1.0 - discount)) * 10000.0).round() / 10000.0;
                        gs.rebuy_plan = Some(RebuyPlan {
                            trigger_nav,
                            amount: (rebuy_amount * 100.0).round() / 100.0,
                            ratio,
                            trend: trend.clone(),
                            discount,
                        });
                    }
                }
            }
            return gs;
        }

        // 无触发 → 持有等待（engine.py:1206）
        let mut reason_parts = vec![
            format!("总浮盈{:.1}%", total_profit_pct),
            format!("今日{:.1}%", input.today_change),
        ];
        if !tc.trend_label.is_empty() {
            reason_parts.push(format!("趋势:{}", tc.trend_label));
        }
        reason_parts.push(format!("风险系数{:.2}×", risk_multiplier));
        if vol_state != "normal_vol" {
            reason_parts.push(format!("波动状态:{}", vol_state));
        }
        reason_parts.push("无触发条件".to_string());

        let mut hold = cand(fund_code, "持有等待", "hold", 8.0);
        hold.reason = reason_parts.join(", ");
        hold.alert = !extra_alerts.is_empty();
        if !extra_alerts.is_empty() {
            hold.alert_msg = Some(extra_alerts.join("; "));
        }
        return to_grid_signal(&hold, input, Some(current_nav), Some(total_profit_pct));
    }

    // ===== 空仓分支（engine.py:1221 起）=====
    let dip_threshold = dst.dip_threshold;
    let can_buy_empty = source == "nav" || confidence >= 0.55;

    // 极端波动禁止买入
    if vol_state == "extreme_vol" {
        let mut sig = cand(fund_code, "极端波动观望", "hold", 8.0);
        sig.reason = format!("波动率处于极端水平({})，暂停所有买入", vol_state);
        sig.alert = true;
        sig.alert_msg = Some("极端波动环境，仅允许止损操作".to_string());
        return to_grid_signal(&sig, input, Some(input.current_nav), None);
    }

    // 大跌抄底
    if input.today_change <= dip_threshold && !in_cooldown && can_buy_empty {
        let max_pos = input.max_position;
        let mut buy_amount_v = buy_amount(max_pos, 0.80, size_mul).unwrap_or(0.0);
        let mut vol_note = String::new();
        if let Some(vp) = tc.volume_proxy {
            if vp < 0.5 {
                buy_amount_v = py_round(buy_amount_v * 0.6, 2);
                vol_note = format!("(缩量{:.1}×, 减仓买入)", vp);
            }
        }
        let amount = if max_pos.is_some() { Some(buy_amount_v) } else { None };
        let mut sig = cand(fund_code, "大跌抄底", "buy", 6.0);
        sig.amount = amount;
        sig.reason = format!(
            "今日跌{:.1}% ≤ 动态阈值{:.1}%, 买入{:.0}元{}",
            input.today_change, dip_threshold, buy_amount_v, vol_note
        );
        return to_grid_signal(&sig, input, Some(input.current_nav), None);
    }

    // 趋势建仓
    let short_5d = tc.short_5d;
    let mid_10d = tc.mid_10d;
    let consecutive_down = tc.consecutive_down;

    if !in_cooldown && can_buy_empty {
        let mut build_signal: Option<SignalCandidate> = None;
        if mid_10d.map_or(false, |m| m <= TREND_BUILD_TRIGGER_10D) && input.today_change >= -0.5 {
            let amt = buy_amount(input.max_position, 0.55, size_mul);
            let v = amt.unwrap_or(0.0);
            let mut sig = cand(fund_code, "低位建仓", "buy", 6.0);
            sig.amount = amt;
            sig.reason = format!(
                "10日累跌{:.1}%, 今日企稳, 中期低位建仓{:.0}元",
                mid_10d.unwrap(),
                v
            );
            build_signal = Some(sig);
        } else if short_5d.map_or(false, |s| s <= TREND_BUILD_TRIGGER_5D) && input.today_change > 0.0 {
            let amt = buy_amount(input.max_position, 0.45, size_mul);
            let v = amt.unwrap_or(0.0);
            let mut sig = cand(fund_code, "反弹建仓", "buy", 6.0);
            sig.amount = amt;
            sig.reason = format!(
                "5日累跌{:.1}%, 今日反弹, 逢低建仓{:.0}元",
                short_5d.unwrap(),
                v
            );
            build_signal = Some(sig);
        } else if consecutive_down >= 3
            && input.today_change < 0.0
            && !tc.recent_changes.is_empty()
            && input.today_change.abs() < tc.recent_changes[0].abs() * 0.6
        {
            let amt = buy_amount(input.max_position, 0.35, size_mul);
            let v = amt.unwrap_or(0.0);
            let mut sig = cand(fund_code, "跌势放缓建仓", "buy", 7.0);
            sig.amount = amt;
            sig.reason = format!(
                "连跌{}天, 跌幅收窄, 试探建仓{:.0}元",
                consecutive_down, v
            );
            build_signal = Some(sig);
        }

        if let Some(bs) = build_signal {
            return to_grid_signal(&bs, input, Some(input.current_nav), None);
        }
    }

    // 温和回调建仓
    if !in_cooldown && can_buy_empty {
        let vol_for_mild = tc.volatility_robust.unwrap_or_else(|| tc.volatility.unwrap_or(1.0));
        let short_3d = tc.short_3d;
        if short_3d.map_or(false, |s| s < 0.0)
            && short_3d.map_or(false, |s| s.abs() > vol_for_mild)
            && vol_state != "extreme_vol"
        {
            let amt = buy_amount(input.max_position, 0.45, size_mul);
            let v = amt.unwrap_or(0.0);
            let mut sig = cand(fund_code, "温和回调建仓", "buy", 7.0);
            sig.amount = amt;
            sig.reason = format!(
                "3日累跌{:.1}%>波动率{:.1}%, 温和回调建仓{:.0}元",
                short_3d.unwrap(),
                vol_for_mild,
                v
            );
            return to_grid_signal(&sig, input, Some(input.current_nav), None);
        }
    }

    // 连跌低吸
    let consec_dip_thresh = dst.consecutive_dip_trigger;
    if input.today_change <= consec_dip_thresh
        && !tc.recent_changes.is_empty()
        && tc.recent_changes[0] < 0.0
        && !in_cooldown
        && can_buy_empty
    {
        let amt = buy_amount(input.max_position, 0.45, size_mul);
        let v = amt.unwrap_or(0.0);
        let mut sig = cand(fund_code, "连跌低吸", "buy", 7.0);
        sig.amount = amt;
        sig.reason = format!(
            "今日跌{:.1}% ≤ {:.1}%, 昨日跌{:.1}%, 连跌低吸{:.0}元",
            input.today_change,
            consec_dip_thresh,
            tc.recent_changes[0],
            v
        );
        return to_grid_signal(&sig, input, Some(input.current_nav), None);
    }

    // 冷却期后建仓
    if !in_cooldown
        && input.cooldown_sell_date.is_some()
        && input.today_change <= 0.3
        && can_buy_empty
    {
        let short_5d_cd = tc.short_5d;
        let consecutive_down_cd = tc.consecutive_down;
        let mut trend_ok = true;
        if short_5d_cd.map_or(false, |s| s <= -5.0) && consecutive_down_cd >= 3 {
            trend_ok = false;
        }
        if trend_ok {
            let amt = buy_amount(input.max_position, 0.50, size_mul);
            let v = amt.unwrap_or(0.0);
            let mut sig = cand(fund_code, "冷却期后建仓", "buy", 7.0);
            sig.amount = amt;
            sig.reason = format!(
                "冷却期结束, 今日{:.1}%, 重新建仓{:.0}元",
                input.today_change, v
            );
            return to_grid_signal(&sig, input, Some(input.current_nav), None);
        }
    }

    // 观望
    let mut obs_parts = vec![format!("今日{:.1}%", input.today_change)];
    if !tc.trend_label.is_empty() {
        obs_parts.push(format!("趋势:{}", tc.trend_label));
    }
    obs_parts.push(format!("波动状态:{}", vol_state));
    obs_parts.push("无触发条件".to_string());
    let mut sig = cand(fund_code, "观望", "hold", 8.0);
    sig.reason = obs_parts.join(", ");
    return to_grid_signal(&sig, input, Some(input.current_nav), None);
}

/// 将最优候选转为最终 GridSignal（折叠 alert_msg 进 reason，priority 还原为 1..8 档位）
fn to_grid_signal(
    best: &SignalCandidate,
    input: &StrategyInput,
    current_nav: Option<f64>,
    total_profit_pct: Option<f64>,
) -> GridSignal {
    let reason = match &best.alert_msg {
        Some(m) if !m.is_empty() => format!("{}; {}", best.reason, m),
        _ => best.reason.clone(),
    };
    GridSignal {
        fund_code: input.fund_code.clone(),
        fund_name: input.fund_name.clone(),
        signal_date: input.today.clone(),
        source: input.source.clone(),
        signal_name: best.signal_name.clone(),
        action: best.action.clone(),
        priority: best.priority / 10,
        reason,
        amount: best.amount,
        sell_shares: best.sell_shares,
        sell_pct: best.sell_pct,
        alert: best.alert,
        confidence: input.confidence,
        est_change_pct: input.today_change,
        current_nav: current_nav.unwrap_or(input.current_nav),
        total_profit_pct,
        regime: input.regime.clone(),
        target_batch_id: best.target_batch_id.clone(),
        fifo_plan: None,
        is_rebuy: false,
        pending_rebuy_id: None,
        rebuy_plan: None,
    }
}
