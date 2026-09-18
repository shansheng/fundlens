//! strategy —— valuation_grid 网格策略决策引擎的 Rust 移植（信号建议层）
//!
//! 边界铁律（见移植方案 §5/§14）：
//! - engine/helpers/batch 为纯逻辑：不碰 DB/网络/时钟，只吃参数化的 `StrategyInput`；
//! - 时钟（today）由调用方注入；DB 读写经 db.rs 的 grid_* 表（store 语义）；
//! - strategy 层只读 positions/transactions，写仅限 grid_* 新表；绝不自动变更持仓（v9）。
//!
//! 移植对应源（Python valuation_grid）：
//! - config.rs   ← grid/config.py 常量表（铁律锁定项）
//! - model.rs    ← 输入/输出结构与 helpers._make_signal 等
//! - helpers.rs  ← grid/helpers.py 纯函数全集（波动率/趋势/止损/止盈/补仓/FIFO）
//! - engine.rs   ← grid/engine.py generate_signal（候选择优 + 空仓级联）
//! - batch.rs    ← 批次视图构建（逐笔 buy 流水 + genesis 残差，方案 §6.2）
//! - rebuy.rs    ← grid/pending_rebuy.py 延迟回补状态机（P2）
//! - history.rs  ← 信号历史/胜率/outcome 回填（P1）

pub mod batch;
pub mod config;
pub mod engine;
pub mod helpers;
pub mod model;

// 组装层入口（commands.rs 调用）：输入组装 → engine → 落库 由上层负责，
// 此处仅导出纯逻辑接口，避免模块间循环依赖。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::model::{NavDay, StrategyInput};

    // ---- py_round：Python round = 半偶舍入 ----
    #[test]
    fn py_round_half_even_matches_python() {
        let h = helpers::py_round;
        // round(0.125,2)=0.12（2 为偶）；round(0.135,2)=0.14（3 为奇进位）
        assert_eq!(h(0.125, 2), 0.12);
        assert_eq!(h(0.135, 2), 0.14);
        assert_eq!(h(-0.125, 2), -0.12);
        // round(2.5)=2；round(3.5)=4
        assert_eq!(h(2.5, 0), 2.0);
        assert_eq!(h(3.5, 0), 4.0);
        assert_eq!(h(2.675, 2), 2.68); // 浮点常规样例
    }

    // ---- 波动率自适应阈值：黄金数值样例（vol=2.0, sensitivity=1.0）----
    #[test]
    fn vol_adaptive_thresholds_golden() {
        let t = helpers::vol_adaptive_thresholds(Some(2.0), 1.0);
        assert!((t.dip_threshold - (-3.6)).abs() < 1e-9); // -2*1.8=-3.6
        assert!((t.tp_trigger - 5.0).abs() < 1e-9); // min(5, 2*2.5)=5
        assert!((t.stop_loss - (-10.0)).abs() < 1e-9); // max(-10,min(-3,-12))=-10
        assert!(t.vol_based);
        // 兜底：vol None → 固定默认值
        let d = helpers::vol_adaptive_thresholds(None, 1.0);
        assert_eq!(d.dip_threshold, config::DEFAULT_DIP_THRESHOLD);
        assert!(!d.vol_based);
    }

    #[test]
    fn volatility_classify_boundaries() {
        assert_eq!(helpers::classify_volatility(0.7), "low_vol");
        assert_eq!(helpers::classify_volatility(1.2), "normal_vol");
        assert_eq!(helpers::classify_volatility(2.5), "high_vol");
        assert_eq!(helpers::classify_volatility(3.5), "extreme_vol");
    }

    // ---- batch_view：逐笔 + genesis 残差对账（Σ批次 ≈ current_shares）----
    #[test]
    fn batch_view_with_genesis_reconciles() {
        let buys = vec![
            batch::BatchTxnLike { txn_id: "1".into(), buy_date: "2025-08-01".into(), shares: 100.0, amount: 1000.0, nav: 1.0, is_rebuy: false },
            batch::BatchTxnLike { txn_id: "2".into(), buy_date: "2025-09-01".into(), shares: 50.0, amount: 600.0, nav: 1.2, is_rebuy: false },
        ];
        // current 190 份：两笔 150 + genesis 40（窗口前早期买入残差，peak=启用时 nav max）
        let batches = batch::build_batch_view(&buys, 190.0, Some(1.5), Some(400.0), 1.0);
        let sum: f64 = batches.iter().map(|b| b.shares).sum();
        assert!((sum - 190.0).abs() < 0.01);
        let genesis = batches.iter().find(|b| b.id == "genesis").expect("应有 genesis");
        assert!((genesis.shares - 40.0).abs() < 1e-6);
        assert_eq!(genesis.peak_nav, Some(1.5));
        // 无 genesis 时（残差=0），首笔非补仓、次笔补仓
        let b2 = batch::build_batch_view(&buys, 150.0, None, None, 1.0);
        assert!(b2.iter().all(|b| b.id != "genesis"));
        assert!(!b2[0].is_supplement);
        assert!(b2[1].is_supplement);
    }

    // ---- engine：空仓 + 平淡行情 → 观望（不 panic、reason 有内容）----
    fn nav_hist_flat(today: &str) -> Vec<NavDay> {
        // 过去 25 个交易日净值恒定 1.0（今日不含，避免被剔除逻辑影响）
        (1..=25)
            .map(|i| {
                let d = format!("2026-08-{:02}", i);
                NavDay { date: d, nav: 1.0 }
            })
            .chain(std::iter::once(NavDay { date: today.to_string(), nav: 1.0 }))
            .collect()
    }

    #[test]
    fn engine_empty_position_flat_market_holds() {
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-06".into(),
            source: "nav".into(),
            market_closed: true,
            today_change: 0.2,
            confidence: 0.9,
            current_nav: 1.0,
            nav_hist: nav_hist_flat("2026-09-06"),
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: None,
        };
        let sig = engine::compute_signal(&input);
        assert!(!sig.reason.is_empty());
        assert!(sig.priority >= 1 && sig.priority <= 8);
        // 无持仓 + 无跌幅触发 → 观望族（hold 或空仓低吸不会因微涨触发买入）
        assert_eq!(sig.signal_name, "观望");
        assert_eq!(sig.action, "hold");
    }

    #[test]
    fn engine_empty_position_big_dip_buys_with_amount() {
        // 大跌日（-3.5%）+ 无持仓 + max_position 齐备 → 应出买入建议且金额>0
        let mut hist = nav_hist_flat("2026-09-06");
        for i in (2..=25).step_by(2) {
            hist[i].nav = 0.99; // 制造轻微历史回撤，使波动>0、趋势不强
        }
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-06".into(),
            source: "nav".into(),
            market_closed: true,
            today_change: -3.5,
            confidence: 0.9,
            current_nav: 0.97,
            nav_hist: hist,
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: None,
        };
        let sig = engine::compute_signal(&input);
        assert!(sig.action == "buy" || sig.action == "hold", "实际: {} {}", sig.signal_name, sig.reason);
        if sig.action == "buy" {
            assert!(sig.amount.unwrap_or(0.0) > 0.0);
        }
    }

    // ---- regime 参数：bull 一律映射 neutral（铁律 v5.17）----
    #[test]
    fn regime_bull_maps_to_neutral() {
        let neutral = config::regime_params("neutral");
        let bull = config::regime_params("bull");
        assert_eq!(bull.first_build_ratio, neutral.first_build_ratio);
        assert_eq!(bull.l2_stop_loss_base, neutral.l2_stop_loss_base);
        let bear = config::regime_params("bear");
        assert!(bear.first_build_ratio < neutral.first_build_ratio); // 0.55<0.70
        assert!(bear.rebuy_discount > neutral.rebuy_discount); // 3.0%>1.5%
    }

    // ---- P2：延迟回补挂单触发（engine.py:498-537）----
    #[test]
    fn engine_pending_rebuy_triggers_buy() {
        // 无持仓、当前净值 1.0 ≤ 挂单 trigger_nav 1.05 → 直接回补 buy（priority 5、is_rebuy）
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-06".into(),
            source: "nav".into(),
            market_closed: true,
            today_change: 0.0,
            confidence: 0.9,
            current_nav: 1.0,
            nav_hist: nav_hist_flat("2026-09-06"),
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: Some(model::RebuyOrder {
                id: 42,
                trigger_nav: 1.05,
                amount: 1000.0,
                sell_nav: 1.10,
                signal_label: "延迟回补(分批止盈)".into(),
                source_signal: "分批止盈".into(),
            }),
        };
        let sig = engine::compute_signal(&input);
        assert_eq!(sig.action, "buy", "挂单触发应产出 buy，实际: {} {}", sig.signal_name, sig.reason);
        assert!(sig.is_rebuy);
        assert_eq!(sig.pending_rebuy_id, Some(42));
        assert_eq!(sig.priority, 5);
        assert!(sig.reason.contains("触发价1.0500"));
    }

    #[test]
    fn engine_pending_rebuy_not_triggered_when_nav_above() {
        // 当前净值 1.20 > trigger 1.05 → 不触发，走正常空仓决策
        let mut hist = nav_hist_flat("2026-09-06");
        hist[0].nav = 1.2; // 今日净值 1.2（与 current_nav 一致）
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-06".into(),
            source: "nav".into(),
            market_closed: true,
            today_change: 0.0,
            confidence: 0.9,
            current_nav: 1.2,
            nav_hist: hist,
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: Some(model::RebuyOrder {
                id: 42,
                trigger_nav: 1.05,
                amount: 1000.0,
                sell_nav: 1.10,
                signal_label: "延迟回补(分批止盈)".into(),
                source_signal: "分批止盈".into(),
            }),
        };
        let sig = engine::compute_signal(&input);
        assert!(!sig.is_rebuy, "净值未到触发价不应回补");
        assert!(sig.pending_rebuy_id.is_none());
    }

    // ---- 趋势同源去重（今日净值未发布时 today_change 与 hist_changes[0] 同源）----
    // 曾因只按 `date != today` 剔除而漏掉「今日净值尚未发布」这一路：
    // nav_hist[0].date != today → 不被剔除 → 最新一日在 all_changes 里计两次。

    /// 构造"今日（2026-09-19）净值未发布"的降序净值序列：
    /// 09-18 起四日连跌，最新在 [0]。
    fn nav_hist_downtrend() -> Vec<NavDay> {
        vec![
            NavDay { date: "2026-09-18".into(), nav: 0.98 },
            NavDay { date: "2026-09-17".into(), nav: 1.00 },
            NavDay { date: "2026-09-16".into(), nav: 1.02 },
            NavDay { date: "2026-09-15".into(), nav: 1.04 },
        ]
    }

    #[test]
    fn analyze_trend_dedups_same_source_today_change() {
        let hist = nav_hist_downtrend();
        // 盘前/盘后/休市（或盘中估值失败降级）分支：today_change 就取 nav 最新两日差分
        let chg = (0.98 / 1.00 - 1.0) * 100.0; // = -2.0
        let tc = helpers::analyze_trend(chg, &hist, "2026-09-19", "nav");
        // hist_changes = [-2.00, -1.96, -1.92]
        // 修复前 all_changes = [-2.0, -2.00, -1.96, -1.92] → 连跌计数 4（虚高一天）
        // 修复后 all_changes = [-2.00, -1.96, -1.92]        → 连跌计数 3
        assert_eq!(tc.consecutive_down, 3, "同源 today_change 不应重复计入（得 4 即回归）");
    }

    #[test]
    fn analyze_trend_keeps_estimated_today_change() {
        // 今日净值同样未发布，但 today_change 来自盘中估值（2.5%）→ 与 nav 差分不同源
        let hist = nav_hist_downtrend();
        let tc = helpers::analyze_trend(2.5, &hist, "2026-09-19", "estimation");
        // 估值涨幅必须计入：all_changes[0] = 2.5 > 0
        assert_eq!(tc.consecutive_up, 1, "估值来源的 today_change 不得被去重误删");
        assert_eq!(tc.consecutive_down, 0);
    }

    #[test]
    fn analyze_trend_keeps_today_change_when_nav_published() {
        // 今日净值**已发布**：nav_hist[0].date == today → trend_navs 已剔除它，
        // hist_changes[0] 是前一日的涨幅，与 today_change 不同源 → 必须计入
        let hist = vec![
            NavDay { date: "2026-09-19".into(), nav: 0.98 },
            NavDay { date: "2026-09-18".into(), nav: 1.00 },
            NavDay { date: "2026-09-17".into(), nav: 1.02 },
        ];
        let chg = (0.98 / 1.00 - 1.0) * 100.0; // -2.0
        let tc = helpers::analyze_trend(chg, &hist, "2026-09-19", "nav");
        // trend_navs = [1.00, 1.02] → hist_changes = [-1.96]
        // all_changes = [-2.0, -1.96] → 连跌 2
        assert_eq!(tc.consecutive_down, 2, "今日净值已发布时 today_change 应计入");
    }

    #[test]
    fn analyze_trend_nav0_adj_not_inflated_when_same_source() {
        // 同源时 nav0_adj 也不得放大：1.05 不该被乘成 1.1025（那不是任何真实净值）
        let mut hist = vec![
            NavDay { date: "2026-09-18".into(), nav: 1.05 },
            NavDay { date: "2026-09-17".into(), nav: 1.00 },
        ];
        for i in 0..10 {
            hist.push(NavDay { date: format!("2026-09-{:02}", 16 - i), nav: 1.00 });
        }
        let chg = (1.05 / 1.00 - 1.0) * 100.0; // = +5.0
        let tc = helpers::analyze_trend(chg, &hist, "2026-09-19", "nav");
        // navs[0] = 1.05、navs[9] = 1.00
        // 修复后 nav0_adj = 1.05    → mid_10d = (1.05/1.00-1)*100 = 5.00
        // 修复前 nav0_adj = 1.1025  → mid_10d = 10.25（凭空多算一个涨幅）
        assert_eq!(tc.mid_10d, Some(5.0), "同源时 nav0_adj 不应被 today_change 放大");
    }

    // ---- estimate_current_nav：休市不施加涨幅（原无持仓分支用裸乘，口径不一致）----

    #[test]
    fn estimate_current_nav_market_closed_returns_latest_verbatim() {
        let hist = nav_hist_downtrend(); // 最新 0.98
        let nav = helpers::estimate_current_nav(0.0, -2.0, &hist, true);
        // 休市：最新已发布净值即"当前净值"。
        // 若误按裸乘（0.98 × (1-2%)）会得 0.9604。
        assert!((nav - 0.98).abs() < 1e-12, "休市应原样返回最新净值，不得再乘涨幅");
    }

    #[test]
    fn estimate_current_nav_intraday_applies_today_change() {
        let hist = nav_hist_downtrend(); // 最新 0.98
        let nav = helpers::estimate_current_nav(0.0, 2.0, &hist, false);
        // 盘中：0.98 × 1.02 = 0.9996
        assert!((nav - 0.9996).abs() < 1e-9);
    }

    #[test]
    fn estimate_current_nav_empty_hist_falls_back_to_anchor() {
        // 无净值历史 → 用 oldest_nav 锚定
        let nav = helpers::estimate_current_nav(1.0, 3.0, &[], false);
        assert!((nav - 1.03).abs() < 1e-9);
    }

    #[test]
    fn engine_pending_rebuy_uses_latest_nav_when_market_closed() {
        // 休市 + 无持仓：最新已发布净值 1.05（前日 1.00 → 今日涨跌位为 +5%）
        // 挂单触发价 1.05。
        // 修复前无持仓分支裸乘 1.05×(1+5%) = 1.1025 > 1.05 → **漏触发**（挂单永远等不到）。
        let hist = vec![
            NavDay { date: "2026-09-18".into(), nav: 1.05 },
            NavDay { date: "2026-09-17".into(), nav: 1.00 },
        ];
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-19".into(),
            source: "nav".into(),
            market_closed: true,
            today_change: 5.0,
            confidence: 0.9,
            current_nav: 1.05,
            nav_hist: hist,
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: Some(model::RebuyOrder {
                id: 7,
                trigger_nav: 1.05,
                amount: 1000.0,
                sell_nav: 1.20,
                signal_label: "延迟回补(分批止盈)".into(),
                source_signal: "分批止盈".into(),
            }),
        };
        let sig = engine::compute_signal(&input);
        assert!(
            sig.is_rebuy,
            "休市净值不应放大：1.05 ≤ 触发价 1.05 应触发；实际: {} {}",
            sig.signal_name, sig.reason
        );
        assert_eq!(sig.pending_rebuy_id, Some(7));
    }

    // ---- 挂单触发必须受风控闸门约束（原实现先于一切闸门、命中即 return）----
    // 注：闸门里的 in_cooldown 项当前恒 false（config::COOLDOWN_DAYS = 0，
    // 系 v5.5 有意改为"卖出后立即可重新入场"），故此处针对低置信度项构造。

    #[test]
    fn engine_pending_rebuy_blocked_by_low_confidence_estimation() {
        // 盘中估值来源 + 低置信度（0.3 < 0.5）：即便净值已跌破触发价也不据此触发。
        // 原实现绕过闸门，同一输入必然 is_rebuy = true。
        let hist = vec![
            NavDay { date: "2026-09-18".into(), nav: 1.05 },
            NavDay { date: "2026-09-17".into(), nav: 1.00 },
        ];
        let input = StrategyInput {
            fund_code: "110011".into(),
            fund_name: Some("测试基金".into()),
            today: "2026-09-19".into(),
            source: "estimation".into(), // 盘中估值
            market_closed: false,
            today_change: -5.0,
            confidence: 0.3, // 低置信
            current_nav: 0.9975,
            // 盘中：1.05 × (1 - 5%) = 0.9975
            nav_hist: hist,
            batches: vec![],
            total_profit_pct: None,
            regime: "neutral".into(),
            vol_sensitivity: 1.0,
            sell_fee_rate: 0.0,
            cooldown_sell_date: None,
            max_position: Some(20000.0),
            available_cash: None,
            pending_rebuy: Some(model::RebuyOrder {
                id: 9,
                trigger_nav: 1.01, // 0.9975 ≤ 1.01，本应触发
                amount: 1000.0,
                sell_nav: 1.20,
                signal_label: "延迟回补(分批止盈)".into(),
                source_signal: "分批止盈".into(),
            }),
        };
        let sig = engine::compute_signal(&input);
        assert!(
            !sig.is_rebuy,
            "低置信度估值不应触发回补挂单；实际: {} {}",
            sig.signal_name, sig.reason
        );
        assert!(sig.pending_rebuy_id.is_none(), "被闸门拦下时不得消费挂单");
    }
}

