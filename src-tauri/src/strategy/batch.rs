//! strategy/batch.rs —— 持仓批次视图构建（移植方案 §6.2）
//!
//! 移植铁律：本文件为纯函数，不碰 DB/网络/时钟。批次视图每次"派生"生成（不存储、不写 v9 账本），
//! 由逐笔 buy 流水 + genesis 残差构成。
//!
//! 对应源：positions.py 的批次视角 + engine.generate_signal 中 `pos["batches"]` 的用法；
//! 具体规则见移植方案 §6.2（v0.2 起流水已按金额÷当日净值回填份额，故可逐笔解析为主，
//! genesis 仅承载窗口前早期买入残差）。

use crate::strategy::model::Batch;

/// 一笔 buy 流水（输入用，字段对齐 model.Batch 的关键列）
///
/// - `txn_id`：原始交易 id；视图里批次 id 会加 `"txn:"` 前缀（与 model.Batch.id 约定一致）。
/// - `is_rebuy`：是否回补仓（由调用方按来源标记传入；genesis 与首笔不标记）。
pub struct BatchTxnLike {
    pub txn_id: String,
    pub buy_date: String,
    pub shares: f64,
    pub amount: f64,
    pub nav: f64,
    pub is_rebuy: bool,
}

/// 构建持仓批次视图（纯函数）。
///
/// 规则（移植方案 §6.2）：
/// 1. buy 流水中 `shares>0 且 nav>0` 的，按 buy_date **升序**逐笔成为批次：
///    - 第 1 笔 `is_supplement=false`，后续逐笔 `is_supplement=true`；
///    - 批次 id = `"txn:" + txn_id`；`is_rebuy` 透传；
///    - `peak_nav=None`（P0 不持久化，由 engine 用 batch.peak_nav 读取；如输入已含可另传）。
/// 2. genesis 残差 = `max(0, current_shares − Σ批次份额)`，仅当 `>0.0001` 时追加：
///    - id = `"genesis"`；`amount` 优先用 `genesis_cost`，否则按 `nav×shares` 兜底；
///    - `nav` 取最老 buy 批次的 nav，若没有任何 buy 流水则回落 `genesis_nav_fallback`；
///    - `peak_nav = genesis_peak`；`buy_date` 取最老 buy 批次日期（无流水则空）。
/// 3. 若该基金没有任何 buy 流水但 `current_shares>0`，同样生成 genesis 批次承载全部份额。
/// 4. 保证 `Σshares ≈ current_shares`（容差 0.01）：残差由 genesis 吸收。
///
/// `genesis_peak`：首次启用时以当时 nav_history max 初始化（OD-4），由调用方传入。
pub fn build_batch_view(
    buy_txns: &[BatchTxnLike],
    current_shares: f64,
    genesis_peak: Option<f64>,
    genesis_cost: Option<f64>,
    genesis_nav_fallback: f64,
) -> Vec<Batch> {
    let mut batches: Vec<Batch> = Vec::new();

    // 按 buy_date 升序（调用方保证有序亦可，这里再确保一次）
    let mut sorted: Vec<&BatchTxnLike> = buy_txns.iter().collect();
    sorted.sort_by(|a, b| a.buy_date.cmp(&b.buy_date));

    // 先算 genesis 是否存在：存在时（窗口前早期买入残差承载）首笔 txn 相对 genesis 属补仓。
    // Python 语义里 genesis=首次建仓、其后的 buy 均为 is_supplement；此处有 genesis 时首笔 txn
    // 也按补仓计（有界近似，见移植方案 OD-3：首批按此实现，回测可再校准）。
    let sum_txn_shares: f64 = sorted.iter().map(|t| if t.shares > 0.0 && t.nav > 0.0 { t.shares } else { 0.0 }).sum();
    let genesis_shares = (current_shares - sum_txn_shares).max(0.0);
    let has_genesis = genesis_shares > 0.0001;

    for (i, t) in sorted.iter().enumerate() {
        if t.shares > 0.0 && t.nav > 0.0 {
            batches.push(Batch {
                id: format!("txn:{}", t.txn_id),
                buy_date: t.buy_date.clone(),
                amount: t.amount,
                shares: t.shares,
                nav: t.nav,
                is_supplement: has_genesis || i > 0,
                is_rebuy: t.is_rebuy,
                peak_nav: None,
                note: String::new(),
            });
        }
    }

    if has_genesis {
        let oldest_nav = sorted.first().map(|b| b.nav).unwrap_or(genesis_nav_fallback);
        let oldest_date = sorted.first().map(|b| b.buy_date.clone()).unwrap_or_default();
        let amount = genesis_cost.unwrap_or_else(|| oldest_nav * genesis_shares);
        batches.push(Batch {
            id: "genesis".to_string(),
            buy_date: oldest_date,
            amount,
            shares: genesis_shares,
            nav: oldest_nav,
            is_supplement: false,
            is_rebuy: false,
            peak_nav: genesis_peak,
            note: String::new(),
        });
    }

    batches
}
