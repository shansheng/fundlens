//! strategy/model.rs —— 策略引擎输入/输出数据结构
//!
//! 边界约定（见移植方案 §5）：engine/helpers 为纯逻辑，不直接碰 DB/网络/时钟；
//! `StrategyInput` 由上层（commands/store/valuation 组装层）填充，today 由调用方注入。

use serde::{Deserialize, Serialize};

// ============================================================
// 输入
// ============================================================

/// 净值日（按日期降序传入，index 0 = 最新）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NavDay {
    pub date: String, // YYYY-MM-DD
    pub nav: f64,
}

/// 持仓批次（holding 段；由 batch.rs 从 buy 流水 + genesis 残差派生，不存储）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Batch {
    /// 批次 id：buy 流水用 "txn:<transaction_id>"，genesis 用 "genesis"
    pub id: String,
    pub buy_date: String, // YYYY-MM-DD
    /// 成本金额（元，含申购费近似）
    pub amount: f64,
    pub shares: f64,
    /// 买入净值（金额/份额反推或流水 price）
    pub nav: f64,
    pub is_supplement: bool,
    pub is_rebuy: bool,
    /// genesis 用：启用以当时 nav_history max 初始化（grid_funds.peak_nav）
    pub peak_nav: Option<f64>,
    pub note: String,
}

impl Batch {
    pub fn is_holding(&self) -> bool {
        self.shares > 0.0 && self.amount > 0.0
    }
}

/// 单基金单日完整输入
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyInput {
    pub fund_code: String,
    pub fund_name: Option<String>,
    /// 注入的"今天"（YYYY-MM-DD），保证可测
    pub today: String,
    /// 数据源："estimation"（盘中估值）/ "nav"（盘后真实净值）
    pub source: String,
    /// 是否已收盘（盘后/休市 → today_change 已由净值日涨跌替换）
    pub market_closed: bool,
    /// 今日涨跌 %（盘中=est_change_pct；盘后=最新两交易日净值涨跌）
    pub today_change: f64,
    /// 估值置信度 0..1（FundLens value_fund.confidence）
    pub confidence: f64,
    /// 当前参考净值（官方净值优先，否则锚定估值 —— 与总览市值同源）
    pub current_nav: f64,
    /// 净值历史，降序（index0=最新）
    pub nav_hist: Vec<NavDay>,
    /// holding 批次，按 buy_date 升序
    pub batches: Vec<Batch>,
    /// 组合/基金级总盈亏%（None → 引擎按批次 _calc_total_profit_pct 兜底）
    pub total_profit_pct: Option<f64>,
    /// 生效行情模式：normalize_regime 后 "neutral"/"bear"
    pub regime: String,
    /// 生效波动率灵敏度（手动 > 自动缓存 > 1.0）
    pub vol_sensitivity: f64,
    /// 卖出费率 %（P0 单值；P1 按持有期表细化）
    pub sell_fee_rate: f64,
    /// 卖出冷却日（P2 落地前由调用方维护）
    pub cooldown_sell_date: Option<String>,
    /// 每基金投入上限（OD-2：出金额建议的必需项）
    pub max_position: Option<f64>,
    /// 全局可投现金（可选；缺省只给档位比例/方向）
    pub available_cash: Option<f64>,
}

impl StrategyInput {
    /// 是否持有该基金（引擎有/无持仓分支的分界）
    pub fn has_position(&self) -> bool {
        self.batches.iter().any(|b| b.is_holding())
    }

    /// 最近 N 条净值（不含今天的估值伪点；调用方保证 nav_hist 为真实净值历史）
    pub fn recent_nav(&self, n: usize) -> &[NavDay] {
        let end = n.min(self.nav_hist.len());
        &self.nav_hist[..end]
    }
}

// ============================================================
// 输出
// ============================================================

/// FIFO 卖出计划单步（helpers._build_fifo_sell_plan 的 fifo_steps）
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FifoStep {
    pub batch_id: String,
    pub buy_date: String,
    pub sell_shares: f64,
    pub batch_total_shares: f64,
    pub is_full_sell: bool,
    pub is_passthrough: bool,
    pub hold_days: i64,
    pub fee_rate: f64,
    pub profit_pct: f64,
    pub estimated_fee: f64,
    pub estimated_net_profit: f64,
    pub reason: String,
    pub note: String,
}

/// FIFO 卖出计划
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FifoPlan {
    pub total_shares: f64,
    pub batch_count: usize,
    pub steps: Vec<FifoStep>,
    pub total_estimated_fee: f64,
    pub total_estimated_profit: f64,
    pub has_passthrough: bool,
    pub passthrough_warning: Option<String>,
    pub passthrough_loss_total: f64,
    pub instruction: String,
}

/// 引擎候选信号（优先级择优的中间体，等价 helpers._make_signal）
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct SignalCandidate {
    pub fund_code: String,
    pub signal_name: String,
    pub action: String, // buy / sell / hold
    /// 数值越小优先级越高（P1 灾难保护 → P8 观望）
    pub priority: i32,
    pub sub_priority: f64,
    pub target_batch_id: Option<String>,
    pub amount: Option<f64>,
    pub sell_shares: Option<f64>,
    pub sell_pct: Option<f64>,
    pub reason: String,
    pub fee_info: Option<String>,
    pub alert: bool,
    pub alert_msg: Option<String>,
}

impl SignalCandidate {
    pub fn new(fund_code: &str, signal_name: &str, action: &str, priority: i32) -> Self {
        SignalCandidate {
            fund_code: fund_code.to_string(),
            signal_name: signal_name.to_string(),
            action: action.to_string(),
            priority,
            ..Default::default()
        }
    }
    pub fn with_sub(mut self, sub: f64) -> Self {
        self.sub_priority = sub;
        self
    }
}

/// 优先级比较（helpers._is_higher_priority）
pub fn is_higher_priority(new_sig: &SignalCandidate, current_best: &SignalCandidate) -> bool {
    if new_sig.priority != current_best.priority {
        return new_sig.priority < current_best.priority;
    }
    new_sig.sub_priority < current_best.sub_priority
}

/// 引擎最终输出（供 store 落库 + 前端展示）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GridSignal {
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
    pub fifo_plan: Option<FifoPlan>,
}

/// 持仓期峰值利润计算中间结果（保留以利对拍/调试）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThresholdsSet {
    pub dip_threshold: f64,
    pub tp_trigger: f64,
    pub stop_loss_adj: f64,
    pub supplement_tiers: Vec<(i32, f64, f64, f64)>,
    pub trail_dd: f64,
    pub vol_state: String,
    pub momentum_score: f64,
    pub win_rate_adj: f64,
    pub rebuy_step: f64,
    pub consecutive_dip_trigger: f64,
    pub supplement_trigger: f64,
    pub supplement_loss_min: f64,
    pub trend_weak_cumulative: f64,
    pub disaster_loss_threshold: f64,
    pub disaster_daily_drop: f64,
    pub total_profit_sell_tiers: Vec<(f64, f64)>,
}
