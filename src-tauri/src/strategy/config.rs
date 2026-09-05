//! strategy/config.rs —— valuation_grid grid/config.py 常量表 1:1 移植
//!
//! 铁律（禁止篡改，见源 config.py v5.16 注释）：
//!   - L2 止损基准 35（STOP_LOSS_L2_SELL_PCT_BASE）
//!   - 止盈抑制阈值 65（tp_suppress_threshold）
//!   - 补仓上限 3（supplement_max_count）
//!   - 仓位乘数上限 1.40（size_mul_cap）
//!   - bull 模式已禁用，一律映射 neutral（v5.17）
//!   - bear 只调买入端：首建 55%、回补折扣 3.0%

/// 信号历史每码保留上限
pub const MAX_HISTORY_PER_FUND: usize = 90;

/// 自动校准结果缓存有效期（秒）= 6 小时
pub const VOL_SENS_CACHE_TTL_SECS: i64 = 3600 * 6;
/// 自动行情识别结果缓存有效期（秒）= 6 小时
pub const REGIME_CACHE_TTL_SECS: i64 = 3600 * 6;

pub const DEFAULT_VOL_SENSITIVITY: f64 = 1.0;

/// 行情模式参数（v5.17：bull = 基线，已禁用；只有 neutral/bear 生效）
#[derive(Clone, Debug)]
pub struct RegimeParams {
    pub first_build_ratio: f64,
    pub size_mul_cap: f64,
    pub tp_suppress_threshold: f64,
    pub supplement_max_count: i32,
    pub l2_stop_loss_base: f64,
    pub rebuy_discount: f64,
}

pub fn regime_params(regime: &str) -> RegimeParams {
    match regime {
        "bear" => RegimeParams {
            first_build_ratio: 0.55,
            size_mul_cap: 1.40,
            tp_suppress_threshold: 65.0,
            supplement_max_count: 3,
            l2_stop_loss_base: 35.0,
            rebuy_discount: 0.030,
        },
        // "bull"（v5.17 已禁用）与任何未知值都回退 neutral 基线
        _ => RegimeParams {
            first_build_ratio: 0.70,
            size_mul_cap: 1.40,
            tp_suppress_threshold: 65.0,
            supplement_max_count: 3,
            l2_stop_loss_base: 35.0,
            rebuy_discount: 0.015,
        },
    }
}

/// 归一化行情模式：bull 一律映射 neutral
pub fn normalize_regime(regime: &str) -> &'static str {
    match regime {
        "bear" => "bear",
        _ => "neutral",
    }
}

// ============================================================
// 以波动率倍数表达的核心阈值（vol × sensitivity 后相乘）
// ============================================================
pub const DIP_BUY_VOL_MULTIPLE: f64 = 1.8;
pub const SUPPLEMENT_TRIGGER_VOL_MULTIPLE: f64 = 1.2;
pub const SUPPLEMENT_LOSS_VOL_MULTIPLE: f64 = 2.2;
pub const CONSECUTIVE_DIP_VOL_MULTIPLE: f64 = 0.7;
/// v5.5: 大幅放宽止损线（原 4.5→6.0）
pub const STOP_LOSS_VOL_MULTIPLE: f64 = 6.0;
/// v5.5: 提高止盈门槛（原 1.5→2.5）
pub const TAKE_PROFIT_VOL_MULTIPLE: f64 = 2.5;
pub const TREND_WEAK_VOL_MULTIPLE: f64 = 1.5;
/// v5.5: 灾难线适度放宽（原 5.0→6.5）
pub const DISASTER_LOSS_VOL_MULTIPLE: f64 = 6.5;
pub const DISASTER_DAILY_VOL_MULTIPLE: f64 = 3.0;

// --- 固定默认值（波动率数据不足时兜底）---
pub const DEFAULT_DIP_THRESHOLD: f64 = -2.5;
pub const DEFAULT_TAKE_PROFIT_TRIGGER: f64 = 2.0;
/// v5.5: 默认止损线 -5.0→-7.0
pub const DEFAULT_STOP_LOSS_BASE: f64 = -7.0;
pub const DEFAULT_SUPPLEMENT_TRIGGER: f64 = -1.5;
pub const DEFAULT_SUPPLEMENT_LOSS_MIN: f64 = -3.0;
pub const DEFAULT_CONSECUTIVE_DIP_TRIGGER: f64 = -1.0;
pub const DEFAULT_TREND_WEAK_CUMULATIVE: f64 = -2.0;
/// v5.5: 灾难线 -9.0→-12.0
pub const DEFAULT_DISASTER_LOSS: f64 = -12.0;
/// v5.5: -5.0→-6.0
pub const DEFAULT_DISASTER_DAILY_DROP: f64 = -6.0;

/// v5.5: 冷却期 1→0 天（卖出后立即可重新入场）
pub const COOLDOWN_DAYS: i32 = 0;
pub const SUPPLEMENT_MAX_COUNT_DEFAULT: i32 = 3;
pub const SUPPLEMENT_MAX_COUNT_HARD_CAP: i32 = 5;

// 补仓档位（波动率版）：(次数, 预算比例, 当日跌幅vol倍数, 浮亏vol倍数)
// v5.13: 首次建仓 70%
pub const SUPPLEMENT_TIERS_VOL: [(i32, f64, f64, f64); 5] = [
    (0, 0.70, 1.0, 1.8),
    (1, 0.35, 1.4, 3.0),
    (2, 0.25, 1.8, 4.5),
    (3, 0.15, 2.2, 6.0),
    (4, 0.10, 2.6, 7.5),
];

// 固定补仓档位（兜底用）：(次数, 比例, 当日跌幅, 浮亏)
pub const SUPPLEMENT_TIERS: [(i32, f64, f64, f64); 5] = [
    (0, 0.70, -1.2, -2.5),
    (1, 0.35, -1.8, -4.0),
    (2, 0.25, -2.2, -6.5),
    (3, 0.15, -2.8, -9.0),
    (4, 0.10, -3.2, -11.0),
];

/// v5.5: 单次补仓上限 20%→35%
pub const SUPPLEMENT_CAP_RATIO: f64 = 0.35;

// 扭亏止盈档位（波动率版）
// v5.9: 门槛大幅提高（5.0→7.0 / 3.5→5.0）
pub const TOTAL_PROFIT_SELL_TIERS_VOL: [(f64, f64); 2] = [(7.0, 50.0), (5.0, 30.0)];
pub const TOTAL_PROFIT_SELL_TIERS: [(f64, f64); 2] = [(10.0, 50.0), (7.0, 30.0)];

pub const TREND_BUILD_TRIGGER_5D: f64 = -3.0;
pub const TREND_BUILD_TRIGGER_10D: f64 = -5.0;

// v5.5: 提高止盈门槛（12/8/5）
pub const TAKE_PROFIT_TIERS: [(f64, f64); 3] = [(12.0, 100.0), (8.0, 70.0), (5.0, 50.0)];

// v5.5: 慢涨止盈门槛同步提高
pub const SLOW_PROFIT_TIERS: [(f64, f64); 3] = [(12.0, 70.0), (8.0, 50.0), (6.0, 30.0)];

pub const DISASTER_CONSECUTIVE_DOWN: i32 = 3;
pub const DISASTER_SELL_PCT_EXTREME: f64 = 50.0;
pub const DISASTER_SELL_PCT_DAILY: f64 = 30.0;

/// v5.8: 补仓间隔 3→2 天
pub const SUPPLEMENT_MIN_GAP_TRADE_DAYS: i32 = 2;
pub const SUPPLEMENT_REBUY_STEP_PCT: f64 = 1.0;

// 回撤止盈
/// v5.5: 激活线 3.5→5.0
pub const TRAIL_PROFIT_ACTIVATE: f64 = 5.0;
/// v5.5: 回撤容忍 1.8→2.5
pub const TRAIL_DD_BASE: f64 = 2.5;
/// v5.5: 最小回撤 1.2→1.8
pub const TRAIL_DD_MIN: f64 = 1.8;
/// v5.5: 最大回撤 4.0→5.0
pub const TRAIL_DD_MAX: f64 = 5.0;
pub const TRAIL_PROFIT_SELL_TIERS: [(f64, f64); 3] = [(12.0, 70.0), (8.0, 50.0), (5.0, 30.0)];

// FIFO 穿透降级
pub const PASSTHROUGH_LOSS_DOWNGRADE: f64 = -50.0;
pub const PASSTHROUGH_MIN_NET_PROFIT_RATIO: f64 = 0.002;
pub const PASSTHROUGH_MIN_NET_PROFIT_ABS: f64 = 30.0;
pub const PASSTHROUGH_LOSS_RATIO_THRESHOLD: f64 = 0.6;

// 组合级
/// v5.5: 日买入上限 10%→20%
pub const DAILY_BUY_CAP_RATIO_BASE: f64 = 0.20;
pub const DAILY_BUY_CAP_RATIO_CONSERVATIVE: f64 = 0.12;
pub const DAILY_BUY_CAP_RATIO_AGGRESSIVE: f64 = 0.30;

// 波动率状态机
pub const VOL_LOW: f64 = 0.8;
pub const VOL_NORMAL_HIGH: f64 = 1.8;
pub const VOL_EXTREME: f64 = 3.0;

// 止损分级
pub const STOP_LOSS_L1_FACTOR: f64 = 0.7;
/// v5.5: L2 止损基准 50%→35%
pub const STOP_LOSS_L2_SELL_PCT_BASE: f64 = 35.0;
pub const STOP_LOSS_L3_FACTOR: f64 = 1.5;
/// v5.5: 连跌 5→7 天才触发极端止损
pub const STOP_LOSS_L3_CONSEC_DOWN: i32 = 7;

// 同赛道约束（v1 不做，占位保留）
pub const SECTOR_BUY_CAP_RATIO: f64 = 0.60;

// 信号胜率自适应
pub const WIN_RATE_TIGHTEN_THRESHOLD: f64 = 0.40;
pub const WIN_RATE_TIGHTEN_FACTOR: f64 = 1.10;

// 流动性溢价
pub const LIQUIDITY_PREMIUM_EXTRA_PCT: f64 = 15.0;
