// 估值引擎（Rust 版）— 与前端 src/valuation/engine.ts 算法保持一致。
// 本地自算：est_nav = official_nav * (1 + Σ 占比_i * (现价_i / 昨收_i - 1))
// 未披露部分按零波动近似。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisclosedHolding {
    pub stock_code: String,
    pub stock_name: String,
    pub weight: f64, // 占净值比例 0~1
    pub report_period: String,
    pub disclosure_type: String, // "top10" | "full"
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StockQuote {
    pub stock_code: String,
    pub name: String,
    pub price: f64,
    pub prev_close: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoldingValuation {
    pub stock_code: String,
    pub stock_name: String,
    pub weight: f64,
    pub price_return: f64,
    pub contribution: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FundValuationResult {
    pub fund_code: String,
    pub official_nav: f64,
    pub est_nav: f64,
    pub est_change_pct: f64,
    pub disclosure_type: String,
    pub disclosed_weight_sum: f64,
    pub holdings: Vec<HoldingValuation>,
    pub estimated: bool,
    pub reason: Option<String>,
    /// 基准指数代码（如 "000300"），仅当提供了基准行情时非空
    pub benchmark_code: Option<String>,
    /// 基准指数名称（如 "沪深300"）
    pub benchmark_name: Option<String>,
    /// 基准指数当日涨跌幅（价格/昨收 - 1）
    pub benchmark_return: f64,
    /// 未披露部分占比（1 - 已披露权重和），由基准指数近似
    pub benchmark_weight: f64,
    /// 平台实时估值涨跌幅（交叉验证用），无则 None
    pub platform_est_change_pct: Option<f64>,
    /// 交叉验证置信度：high=两源一致 / medium=小幅分歧 / low=明显分歧 / none=无法校验
    pub confidence: String,
    /// 两口径涨跌幅绝对差值（百分点），用于展示分歧幅度
    pub divergence: f64,
    /// 本地持仓穿透自算的涨跌幅（双源之一，始终带来源数值，便于前端并列展示）
    pub penetration_est_change_pct: Option<f64>,
    /// 多源共识估值涨跌幅：两源均在且分歧不大时取加权共识，否则 None（仅展示、不改头条取值）
    pub consensus_est_change_pct: Option<f64>,
    /// 估值口径：index=指数实时估值优先（指数型基金）/ penetration=本地持仓穿透自算 / none=无
    pub valuation_method: Option<String>,
}

pub struct ValuationInput {
    pub fund_code: String,
    pub official_nav: f64,
    pub holdings: Vec<DisclosedHolding>,
    pub quotes: std::collections::HashMap<String, StockQuote>,
    /// 通用基准指数行情（多为沪深300），用于近似主动基金未披露部分。
    pub benchmark: Option<StockQuote>,
    /// 基金实际跟踪指数行情（指数/ETF 类）。优先于 benchmark 用于近似未披露部分，
    /// 即「指数型基金按跟踪的指数涨跌计算」。无则退回 benchmark。
    /// 对所有指数型基金（含指数增强）都传入，使穿透口径的未披露部分按真实跟踪指数近似。
    pub tracked_index: Option<StockQuote>,
    /// 是否为「纯被动指数型基金」（排除指数增强）。为真且 tracked_index 有效时，头条估值
    /// 直接采用跟踪指数当日涨跌（指数实时估值优先），成分股穿透降为参考口径；
    /// 否则（主动基金 / 指数增强 / 拿不到跟踪指数行情）头条走通用本地穿透自算。
    pub pure_index: bool,
}

pub fn value_fund(input: ValuationInput) -> FundValuationResult {
    let ValuationInput {
        fund_code,
        official_nav,
        holdings,
        quotes,
        benchmark,
        tracked_index,
        pure_index,
    } = input;

    // 纯被动指数基金可用「跟踪指数代理」给出估值（无需持仓数据）；
    // 仅当既无披露持仓、又无法用指数代理时，才提前返回「无法估算」。
    // （此前对 holdings 为空一律早退，导致无披露的 ETF 联接基金永远拿不到估值。）
    let has_index_proxy = pure_index && tracked_index.as_ref().map_or(false, |t| t.prev_close > 0.0);
    if holdings.is_empty() && !has_index_proxy {
        return FundValuationResult {
            fund_code,
            official_nav,
            est_nav: official_nav,
            est_change_pct: 0.0,
            disclosure_type: "top10".into(),
            disclosed_weight_sum: 0.0,
            holdings: vec![],
            estimated: false,
            reason: Some("暂无披露持仓数据，无法估算".into()),
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
        };
    }

    if official_nav <= 0.0 {
        return FundValuationResult {
            fund_code: fund_code.clone(),
            official_nav,
            est_nav: official_nav,
            est_change_pct: 0.0,
            disclosure_type: holdings
            .first()
            .map(|h| h.disclosure_type.clone())
            .unwrap_or_else(|| "index".to_string()),
            disclosed_weight_sum: 0.0,
            holdings: vec![],
            estimated: false,
            reason: Some("缺少官方净值基准，无法估算".into()),
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
        };
    }

    let mut valued: Vec<HoldingValuation> = Vec::with_capacity(holdings.len());
    let mut portfolio_return = 0.0;
    let mut disclosed_weight_sum = 0.0; // 仅统计「有有效行情」的披露权重；缺行情权重归入基准近似

    for h in &holdings {
        let q = quotes.get(&h.stock_code);
        let (price_return, contribution, weight_counted) = match q {
            Some(q) if q.prev_close > 0.0 => {
                let pr = q.price / q.prev_close - 1.0;
                (pr, h.weight * pr, h.weight)
            }
            // 缺行情（如美股/北交所/停牌）：该持仓权重归入基准近似，而不是按 0 收益丢弃。
            // 否则主动 QDII 的海外重仓会被直接忽略，估值实质上退化为轻仓位基准，造成显著低估/高估。
            _ => (0.0, 0.0, 0.0),
        };
        portfolio_return += contribution;
        disclosed_weight_sum += weight_counted;
        valued.push(HoldingValuation {
            stock_code: h.stock_code.clone(),
            stock_name: h.stock_name.clone(),
            weight: h.weight,
            price_return,
            contribution,
        });
    }

    // 未披露部分 (1 - disclosed_weight_sum) 用指数当日涨跌幅近似，
    // 显著减少对现金/债券/非前十大仓位的"零波动"低估误差。
    // 指数/ETF 基金优先用其「实际跟踪指数」(tracked_index)，数学上远比通用沪深300贴近；
    // 主动基金或未提供 tracked_index 时退回通用基准 benchmark（多为沪深300）。
    let benchmark_weight = (1.0 - disclosed_weight_sum).max(0.0);
    let (benchmark_code, benchmark_name, benchmark_return) = match (&tracked_index, &benchmark) {
        (Some(t), _) if t.prev_close > 0.0 => {
            let r = t.price / t.prev_close - 1.0;
            (Some(t.stock_code.clone()), Some(t.name.clone()), r)
        }
        (_, Some(b)) if b.prev_close > 0.0 => {
            let r = b.price / b.prev_close - 1.0;
            (Some(b.stock_code.clone()), Some(b.name.clone()), r)
        }
        _ => (None, None, 0.0),
    };

    // 本地持仓穿透自算涨跌幅（参考口径）：披露部分穿透 + 未披露部分按基准指数近似。
    // 该口径对所有基金都计算，供指数基金作为「穿透参考」并列展示、供主动基金作为头条。
    let penetration_change_pct = portfolio_return + benchmark_weight * benchmark_return;

    // 纯被动指数型基金优先采用「跟踪指数实时估值」：头条直接用跟踪指数当日涨跌（跟踪误差极小，
    // 远比成分股穿透贴近）；成分股穿透降为参考口径填入 penetration_est_change_pct。
    // 指数增强 / 主动基金 / 拿不到跟踪指数行情：头条走通用本地穿透自算（未披露部分仍按跟踪指数近似）。
    let (est_change_pct, valuation_method) = if pure_index {
        match &tracked_index {
            Some(t) if t.prev_close > 0.0 => {
                let idx = t.price / t.prev_close - 1.0;
                (idx, Some("index".to_string()))
            }
            _ => (penetration_change_pct, Some("penetration".to_string())),
        }
    } else {
        (penetration_change_pct, Some("penetration".to_string()))
    };

    let est_nav = official_nav * (1.0 + est_change_pct);

    FundValuationResult {
        fund_code,
        official_nav,
        est_nav,
        est_change_pct,
        disclosure_type: holdings
            .first()
            .map(|h| h.disclosure_type.clone())
            .unwrap_or_else(|| "index".to_string()),
        disclosed_weight_sum,
        holdings: valued,
        estimated: true,
        reason: None,
        benchmark_code,
        benchmark_name,
        benchmark_return,
        benchmark_weight,
        platform_est_change_pct: None,
        confidence: "none".into(),
        divergence: 0.0,
        penetration_est_change_pct: Some(penetration_change_pct),
        consensus_est_change_pct: None,
        valuation_method,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionSummary {
    pub fund_code: String,
    pub market_value: f64,
    pub pnl: f64,
    pub pnl_pct: f64,
    /// 头条当日收益（盘中=估算，盘后/休市=实际；由调用方按 phase 填充）
    pub day_pnl: f64,
    /// 头条当日收益率（对应 day_pnl 的比率口径）
    pub day_pnl_pct: f64,
    /// 当日估算收益（金额）：份额 ×(估算净值 − 上一交易日收盘净值)
    pub day_pnl_est: f64,
    /// 当日实际收益（金额）：份额 ×(官方净值 − 上一交易日收盘净值)，未确认则为 0
    pub day_pnl_act: f64,
    /// 当日估算收益率（比率口径）
    pub day_pnl_pct_est: f64,
    /// 当日实际收益率（比率口径）
    pub day_pnl_pct_act: f64,
    /// 持仓占比 = 个基市值 / 组合总市值
    pub weight: f64,
    pub estimated: bool,
}

/// 组合进阶风险指标（基于各基金 nav_history 聚合的组合净值序列计算，见 compute_portfolio_risk）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortfolioRisk {
    /// 区间累计收益率（%）
    pub cumulative_return_pct: f64,
    /// 年化收益率（%）
    pub annualized_return_pct: f64,
    /// 年化波动率（%，日波动率 ×√252）
    pub annualized_vol_pct: f64,
    /// 最大回撤（%，负数）
    pub max_drawdown_pct: f64,
    /// 首末对齐点之间实际经历的 A 股交易日数（相邻对齐点 (a,b] 内交易日个数之和）。
    /// 稠密（每个交易日都有全部基金净值）时 = 点数 − 1；中间缺数时以真实交易日数为准。
    pub days: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortfolioSummary {
    pub total_market_value: f64,
    pub total_cost: f64,
    pub total_pnl: f64,
    pub total_pnl_pct: f64,
    /// 当日估算收益（投影：实时/本地自算估值，交易时段随行情跳动）
    pub est_day_pnl: f64,
    /// 当日实际收益（已确认：盘后=当日实际，休市=上一交易日实际；交易中未实现则为 0）
    pub act_day_pnl: f64,
    /// 当日估算收益率（比率口径，聚合层 = est_day_pnl / 昨收总市值）
    pub day_pnl_pct_est: f64,
    /// 当日实际收益率（比率口径，聚合层 = act_day_pnl / 昨收总市值）
    pub day_pnl_pct_act: f64,
    pub positions: Vec<PositionSummary>,
    /// 进阶风险指标（年化收益/波动/最大回撤等）；无数据时为 None
    pub risk: Option<PortfolioRisk>,
}

/// 传入 summarize_portfolio 的「单只持仓已结算指标」输入。
/// 个基的市值 / 盈亏 / 当日拆分值由调用方（commands）经 compute_position_metrics 计算后填入，
/// summarize 仅负责聚合与持仓占比、头条口径切换。
pub struct PositionForSummary {
    pub fund_code: String,
    pub market_value: f64,
    /// 昨收总市值（= 份额 × 昨收净值），用于组合当日收益率分母；与个基 day_pnl_pct 分母保持一致。
    pub prev_close_market_value: f64,
    pub cost_amount: f64,
    pub total_pnl: f64,
    pub total_pnl_pct: f64,
    pub day_pnl_est: f64,
    pub day_pnl_act: f64,
    pub day_pnl_pct_est: f64,
    pub day_pnl_pct_act: f64,
    pub estimated: bool,
}

/// 单只持仓「业界标准 + 当日估算」指标的纯计算（无副作用，便于单测）。
///
/// 基准净值 baseline_nav 统一取「上一交易日收盘净值」(prev_nav)，即「当日收益」的差值与分母基准。
/// 当 est_cache 未建立、prev_nav 缺失时，退化为 official_nav（最近可得收盘净值）。统一 prev_nav 基准可消除
/// 此前「盘中用 official_nav、盘后/休市用 prev_nav」带来的 15:00 边界参考市值跳变，以及陈旧 official_nav
/// 作为基准导致的当日收益失真。
///
/// 参考市值统一使用 anchored_est_nav（= baseline × (1 + est_ret)）：est_nav 以 official_nav 为锚，但
/// official_nav 可能因接口反爬而陈旧，故将当日估算涨跌幅 est_ret 重新锚定到可靠的昨收基准，使市值/当日估算/
/// 累计盈亏三者自洽。盘中 baseline=prev_nav 时，若 official_nav 新鲜（≈prev_nav），anchored_est_nav≈est_nav，
/// 与平台实时估值一致；official_nav 陈旧时则避免市值被拉向陈旧值。
///
/// 三态解析（phase 由 data::market_phase() 给出）：
/// - `intraday`：当日官方净值尚未发布；参考市值 = anchored_est_nav，当日估算 = 份额×(anchored_est_nav − baseline)。
///   当日实际收益列（`day_pnl_act`）始终为真实官方口径 = 份额×(official_nav − prev_nav)：盘中 official_nav 为
///   上一交易日收盘、prev_nav 为其昨收，差值即「上日实际」，前端标「上次」；绝不回填估算。
/// - `post_close`：当日估算 = 份额×(anchored_est_nav − baseline)。
///   参考市值 = 官方净值确为今日真值（nav_date == today）时用 official_nav（真实市值=份额×当日实际净值），
///   否则用 anchored_est_nav。`day_pnl_act` = 份额×(official_nav − prev_nav)：
///     · 官方净值确为今日真值（nav_date == today）：即真实「今日实际」，前端标「实际」。
///     · 官方净值尚未确认/陈旧（nav_date != today）：即最近一次确认的净值相对其昨收基准的真实变动，
///       前端标「上次」。这是诚实的「上一次」真实实际收益，虽可能冻结但不编造。
/// - `closed`（非交易日 / 开盘前）：无当日交易，当日估算 = 0；参考市值同 post_close（今日真值→官方净值，
///   否则 anchored_est_nav）。`day_pnl_act` 同样为真实官方口径（最新可得确认净值的变动），前端标「上次」。
///
/// 市值口径（用户明确约定）：刷新到「当日实际净值」（nav_date==today）后，市值 = 份额 × 当日实际净值；
/// 未刷新到今日真值时用估算口径避免陈旧 official_nav 冻结市值。
///
/// 关键原则：「当日」列永远只显示真实官方净值口径（今日实际 / 上日实际），估算只在「当日估算收益」列
/// （`day_pnl_est`）展示，二者严格分离、互不替代。曾因「基准被 est_cache 污染 + 误标实际」制造冻结假值，
/// 现已通过优先使用与 official_nav 同源写入的 funds.prev_nav 真实昨收基准、并辅以正确「实际/上次」标签消除。
pub struct PositionMetricsInput<'a> {
    pub shares: f64,
    pub cost_amount: f64,
    pub est_nav: f64,
    pub official_nav: f64,
    pub prev_nav: f64,
    pub nav_date: &'a str,
    pub phase: &'a str,
    pub today: &'a str,
}

#[derive(Debug, Clone)]
pub struct PositionMetrics {
    pub market_value: f64,
    pub baseline_nav: f64,
    pub prev_close_market_value: f64,
    pub total_pnl: f64,
    pub total_pnl_pct: f64,
    pub day_pnl_est: f64,
    pub day_pnl_act: f64,
    pub day_pnl_pct_est: f64,
    pub day_pnl_pct_act: f64,
    /// 估算净值按昨收基准重锚定后的值：= baseline_nav × (1 + est_ret)。
    /// 供 est_cache 跨日滑落时使用，避免将锚定陈旧 official_nav 的原始 est_nav 直接落库，
    /// 导致非交易日后昨收基准回退、累计涨跌丢失。
    pub anchored_est_nav: f64,
}

pub fn compute_position_metrics(i: &PositionMetricsInput) -> PositionMetrics {
    // 统一以昨收净值 prev_nav 作为基准；盘中不再特殊使用 official_nav，避免 official_nav 陈旧时
    // 盘中/盘后参考市值与当日收益出现跳变。无有效 prev_nav（首次落地/est_cache 未建立）时退化为 official_nav。
    let baseline_nav = if i.prev_nav > 0.0 {
        i.prev_nav
    } else {
        i.official_nav
    };

    // 真实当日估算涨跌幅：est_nav 以 official_nav 为锚，但 official_nav 可能因净值接口反爬而陈旧，
    // 与昨收基准(baseline_nav) 不一致。若直接用 (est_nav − baseline) 当盈亏，会把
    // 「official_nav 与昨收基准之间的陈旧漂移」误算成当日盈亏（典型表现：当日估算收益被大幅低估/偏负）。
    // 正确做法：取当日估算涨跌幅 = est_nav / official_nav − 1，再乘以昨收基准 baseline_nav。
    let est_ret = if i.official_nav > 0.0 {
        i.est_nav / i.official_nav - 1.0
    } else {
        0.0
    };
    // 锚定到昨收基准的估算净值：盘中 est_nav 本就以昨收=official_nav 为锚，二者天然一致
    // （anchored_est_nav = official_nav × (est_nav/official_nav) = est_nav）；盘后/休市改用它作参考市值，
    // 使参考市值、当日估算、累计盈亏三者自洽，消除陈旧 official_nav 漂移污染。
    let anchored_est_nav = baseline_nav * (1.0 + est_ret);

    // 参考市值：官方净值已确认为今日真值（nav_date==today，即用户已刷新到当日实际净值）时，
    // 市值 = 份额 × 当日实际净值（真实市值，与「当日实际收益」/累计盈亏同源自洽，用户明确口径）；
    // 否则（盘中 / 官方净值陈旧=「上次」）使用重锚定估算净值，避免陈旧 official_nav 把市值冻结。
    let reference_nav = if i.official_nav > 0.0 && i.nav_date == i.today {
        i.official_nav
    } else {
        anchored_est_nav
    };

    let market_value = i.shares * reference_nav;
    let prev_close_market_value = i.shares * baseline_nav;
    let total_pnl = market_value - i.cost_amount;
    // 收益率分母保护：成本基数为 0 / 负（现金分红「成本还原法」允许 basis 为负，见 db.rs 重放规则）
    // 时返回 0 而非用 0 或负成本相除（负成本会令收益率符号反转/爆炸）。阈值 1e-9 与份额归零阈值一致。
    let total_pnl_pct = if i.cost_amount > 1e-9 {
        total_pnl / i.cost_amount
    } else {
        0.0
    };

    // 当日估算收益：份额 × 当日估算涨跌幅 × 昨收基准（锚定修正，避免陈旧 official_nav 漂移污染）。
    // 等价于 shares × (anchored_est_nav − baseline_nav)，与参考市值/累计盈亏自洽。
    let day_pnl_est = if i.phase == "closed" {
        0.0
    } else {
        i.shares * (anchored_est_nav - baseline_nav)
    };

    // 当日实际收益口径：「当日」列永远只显示真实官方净值口径，绝不回填估算。
    // = 份额 ×(官方净值 − 昨收基准)。这是「今日实际」(nav_date==today 且非盘中) 或
    // 「上日实际」(官方净值尚未确认/陈旧，即最近一次确认的净值相对其昨收基准的真实变动) 的真实数值。
    // 标签由前端按 nav_date==today 区分「实际」/「上次」，二者都基于同一真实官方净值来源。
    // 说明：
    // - 陈旧官方净值会让「上次」数值冻结在最近一次确认的净值变动上——这是预期的、诚实的：
    //   它确为「上一次」真实实际收益，且已标「上次」而非「实际」，不会与今日实际混淆。
    // - 此前曾因「基准被 est_cache 污染 + 误标实际」而制造冻结假值；现已通过（1）优先使用与
    //   official_nav 同源写入的 funds.prev_nav 真实昨收基准、（2）正确「实际/上次」标签消除。
    // - 盘中无今日官方净值时，official_nav 为上一交易日收盘、prev_nav 为其昨收，差值即「上日实际」，
    //   与「当日估算收益」列（盘中实时浮动估算）严格区分，互不替代。
    let day_pnl_act = if i.official_nav > 0.0 && i.prev_nav > 0.0 {
        i.shares * (i.official_nav - i.prev_nav)
    } else {
        0.0
    };

    // 当日收益率分母恒为「昨收市值」（与组合层分母一致）。P0 口径：删除「退化到当前市值」的
    // 分支——当前市值含当日涨幅，作分母会在上涨日系统性低估收益率（与主流平台口径相反）。
    // 昨收市值缺失（首日建仓无基准/份额为 0）时收益率置 0，而非用含当日变动的市值凑一个假比率。
    let day_pnl_pct_est = if prev_close_market_value > 0.0 {
        day_pnl_est / prev_close_market_value
    } else {
        0.0
    };
    let day_pnl_pct_act = if prev_close_market_value > 0.0 {
        day_pnl_act / prev_close_market_value
    } else {
        0.0
    };

    PositionMetrics {
        market_value,
        baseline_nav,
        prev_close_market_value,
        total_pnl,
        total_pnl_pct,
        day_pnl_est,
        day_pnl_act,
        day_pnl_pct_est,
        day_pnl_pct_act,
        anchored_est_nav,
    }
}

pub fn summarize_portfolio(positions: &[PositionForSummary], phase: &str) -> PortfolioSummary {
    let mut total_market_value = 0.0;
    let mut prev_close_total_market_value = 0.0;
    let mut total_cost = 0.0;
    let mut total_pnl = 0.0;
    let mut est_day_pnl = 0.0;
    let mut act_day_pnl = 0.0;
    let mut out = Vec::with_capacity(positions.len());

    for p in positions {
        total_market_value += p.market_value;
        prev_close_total_market_value += p.prev_close_market_value;
        total_cost += p.cost_amount;
        total_pnl += p.total_pnl;
        est_day_pnl += p.day_pnl_est;
        act_day_pnl += p.day_pnl_act;
    }
    let weight_denom = if total_market_value > 0.0 {
        total_market_value
    } else {
        1.0
    };
    // 头条口径：盘中展示估算，盘后/休市展示实际（与业务一致）
    let headline_est = phase == "intraday";

    for p in positions {
        let (day_pnl, day_pnl_pct) = if headline_est {
            (p.day_pnl_est, p.day_pnl_pct_est)
        } else {
            (p.day_pnl_act, p.day_pnl_pct_act)
        };
        out.push(PositionSummary {
            fund_code: p.fund_code.clone(),
            market_value: p.market_value,
            pnl: p.total_pnl,
            pnl_pct: p.total_pnl_pct,
            day_pnl,
            day_pnl_pct,
            day_pnl_est: p.day_pnl_est,
            day_pnl_act: p.day_pnl_act,
            day_pnl_pct_est: p.day_pnl_pct_est,
            day_pnl_pct_act: p.day_pnl_pct_act,
            weight: p.market_value / weight_denom,
            estimated: p.estimated,
        });
    }

    PortfolioSummary {
        total_market_value,
        total_cost,
        total_pnl,
        // 分母保护同个基层：成本基数 <=1e-9（含被分红摊完的 0/负成本）时收益率置 0，不做符号反转。
        total_pnl_pct: if total_cost > 1e-9 {
            total_pnl / total_cost
        } else {
            0.0
        },
        est_day_pnl,
        act_day_pnl,
        // 组合当日收益率分母统一用「昨收总市值」，与支付宝/天天基金等主流平台一致；
        // 使用当前市值作分母会在上涨日低估收益率（分母同时被当日涨幅放大）。
        // 与个基层同口径（P0）：昨收总市值缺失时置 0，不再退化为当前总市值。
        day_pnl_pct_est: if prev_close_total_market_value > 0.0 {
            est_day_pnl / prev_close_total_market_value
        } else {
            0.0
        },
        day_pnl_pct_act: if prev_close_total_market_value > 0.0 {
            act_day_pnl / prev_close_total_market_value
        } else {
            0.0
        },
        positions: out,
        risk: None,
    }
}

/// 单只基金参与组合风险聚合的净值序列（份额恒定近似：用「当前份额 × 历史每日净值」重构组合历史市值）。
pub struct FundNavSeries {
    pub shares: f64,
    /// 按日期升序的 (交易日, 净值)。调用方应优先传入累计净值（acc_nav）以消除分红除息失真；
    /// 仅当累计净值缺失时才退化为单位净值。
    pub navs: Vec<(String, f64)>,
}

/// 基于各基金 nav_history 聚合的组合净值序列，计算进阶风险指标。
///
/// 方法：对每只基金取「当前有效份额 × 该基金历史每日单位净值」得到其历史市值，
/// 按共同交易日加总得到组合历史市值序列；再据此计算：
/// - 区间累计收益率 = 末值/初值 − 1
/// - 年化收益率 = (末值/初值)^(252/交易日数) − 1
/// - 年化波动率 = 日收益率标准差 × √252
/// - 最大回撤 = min(当日市值 / 历史峰值 − 1)
///
/// 数据不足（无可比对齐的交易日，或点数 < 2）时返回 None，由前端降级展示。
pub fn compute_portfolio_risk(series: &[FundNavSeries]) -> Option<PortfolioRisk> {
    if series.is_empty() {
        return None;
    }
    // 取所有序列「共同交易日」的交集，保证每只基金在该日都有净值，权重一致。
    let mut common: Option<std::collections::BTreeSet<String>> = None;
    for s in series {
        let set: std::collections::BTreeSet<String> = s.navs.iter().map(|(d, _)| d.clone()).collect();
        common = Some(match common {
            Some(c) => c.intersection(&set).cloned().collect(),
            None => set,
        });
    }
    let dates = common?;
    if dates.len() < 2 {
        return None;
    }
    // 每只基金的 日期→净值 查询表
    let maps: Vec<(f64, std::collections::HashMap<String, f64>)> = series
        .iter()
        .map(|s| {
            let m: std::collections::HashMap<String, f64> =
                s.navs.iter().map(|(d, n)| (d.clone(), *n)).collect();
            (s.shares, m)
        })
        .collect();

    // 组合历史市值序列（BTreeSet 升序遍历，日期天然升序）
    let mut pts: Vec<(String, f64)> = Vec::with_capacity(dates.len());
    for d in &dates {
        let mut v = 0.0;
        let mut ok = true;
        for (shares, m) in &maps {
            match m.get(d) {
                Some(n) if *n > 0.0 => v += shares * n,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            // 交集理论上都应存在，缺失则跳过该日
            continue;
        }
        pts.push((d.clone(), v));
    }
    if pts.len() < 2 {
        return None;
    }
    let first = pts[0].1;
    let last = pts[pts.len() - 1].1;
    if first <= 0.0 {
        return None;
    }

    // 交易日口径修正（P1）：days = 首末对齐点之间「实际经历的 A 股交易日数」，
    // 即相邻对齐点 (a, b] 区间内交易日个数之和，而非旧实现的「点数 / 点数−1」。
    // nav_history 只落交易日，正常稠密时 days = 点数 − 1；中间有基金缺数/间隔时以真实
    // 交易日数为准，年化收益用其折 252，避免把多日间隔当单日而系统性高估年化收益。
    let mut td_total: i64 = 0;
    let mut daily: Vec<f64> = Vec::with_capacity(pts.len() - 1);
    for w in pts.windows(2) {
        let gap = trading_days_between(&w[0].0, &w[1].0);
        td_total += gap;
        if w[0].1 > 0.0 {
            let r = w[1].1 / w[0].1 - 1.0;
            // 仅「单交易日间隔」的相邻收益才是真日收益（用于波动率）；
            // 跨多日的间隔收益无法分解为单日，跳过以免把稀疏缺口误算成低波动。
            if gap == 1 {
                daily.push(r);
            }
        }
    }
    let cumulative_return = last / first - 1.0;
    let annualized_return = if td_total > 0 {
        (last / first).powf(252.0 / td_total as f64) - 1.0
    } else {
        0.0
    };

    // 年化波动率 = 日收益率样本标准差 × √252（除以 n−1，与主流金融工具一致）。
    // 修正旧实现「总体标准差 /n」对波动的轻微低估。
    let annualized_vol = if daily.len() >= 2 {
        let mean = daily.iter().sum::<f64>() / daily.len() as f64;
        let var = daily
            .iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>()
            / (daily.len() as f64 - 1.0);
        var.sqrt() * (252.0_f64).sqrt()
    } else {
        0.0
    };

    // 最大回撤
    let mut peak = pts[0].1;
    let mut max_dd = 0.0;
    for (_, v) in &pts {
        if *v > peak {
            peak = *v;
        }
        if peak > 0.0 {
            let dd = v / peak - 1.0;
            if dd < max_dd {
                max_dd = dd;
            }
        }
    }

    Some(PortfolioRisk {
        cumulative_return_pct: cumulative_return * 100.0,
        annualized_return_pct: annualized_return * 100.0,
        annualized_vol_pct: annualized_vol * 100.0,
        max_drawdown_pct: max_dd * 100.0,
        days: td_total,
    })
}

/// 统计 (a, b] 开区间内的 A 股交易日个数（a、b 为 YYYY-MM-DD，须 b>a）。
/// 周末一定非交易日；法定休市日按已加载交易日历缓存判定（只读缓存，不触发 DB 加载，
/// 可安全用于已持 DB 连接锁之外的纯计算路径；缓存未命中的日期按开市处理）。
pub(crate) fn trading_days_between(a: &str, b: &str) -> i64 {
    let (Ok(ad), Ok(bd)) = (
        chrono::NaiveDate::parse_from_str(a, "%Y-%m-%d"),
        chrono::NaiveDate::parse_from_str(b, "%Y-%m-%d"),
    ) else {
        return 0;
    };
    if bd <= ad {
        return 0;
    }
    let mut d = ad + chrono::Duration::days(1);
    let mut n = 0i64;
    while d <= bd {
        if crate::data::is_trading_day_cached(d) {
            n += 1;
        }
        d += chrono::Duration::days(1);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn serialization_uses_camel_case() {
        let v = FundValuationResult {
            fund_code: "110011".into(),
            official_nav: 4.1961,
            est_nav: 4.25,
            est_change_pct: 0.0123,
            disclosure_type: "top10".into(),
            disclosed_weight_sum: 0.6,
            holdings: vec![HoldingValuation {
                stock_code: "600519".into(),
                stock_name: "贵州茅台".into(),
                weight: 0.099,
                price_return: 0.02,
                contribution: 0.00198,
            }],
            estimated: true,
            reason: None,
            benchmark_code: None,
            benchmark_name: None,
            benchmark_return: 0.0,
            benchmark_weight: 0.4,
            platform_est_change_pct: None,
            confidence: "none".into(),
            divergence: 0.0,
        penetration_est_change_pct: None,
        consensus_est_change_pct: None,
        valuation_method: None,
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"estNav\""), "expected estNav in {s}");
        assert!(s.contains("\"estChangePct\""), "expected estChangePct in {s}");
        assert!(s.contains("\"disclosedWeightSum\""), "expected disclosedWeightSum in {s}");
        assert!(s.contains("\"valuationMethod\""), "expected valuationMethod in {s}");
        assert!(s.contains("\"stockCode\""), "expected stockCode in {s}");
        assert!(s.contains("\"priceReturn\""), "expected priceReturn in {s}");
        assert!(s.contains("\"contribution\""), "expected contribution in {s}");
        // 确保没有残留 snake_case
        assert!(!s.contains("\"est_nav\""), "snake_case est_nav leaked: {s}");
        assert!(!s.contains("\"stock_code\""), "snake_case stock_code leaked: {s}");

        let q = StockQuote {
            stock_code: "00700".into(),
            name: "腾讯控股".into(),
            price: 441.0,
            prev_close: 461.6,
        };
        let qs = serde_json::to_string(&q).unwrap();
        assert!(qs.contains("\"prevClose\""), "expected prevClose in {qs}");
        assert!(qs.contains("\"stockCode\""), "expected stockCode in {qs}");
    }

    #[test]
    fn tracked_index_drives_index_fund_estimate() {
        // 中证白酒指数基金：披露 70% 茅台（当日 0%），未披露 30% 按跟踪指数 中证白酒 +2% 近似
        let holdings = vec![DisclosedHolding {
            stock_code: "600519".into(),
            stock_name: "贵州茅台".into(),
            weight: 0.7,
            report_period: "2026Q2".into(),
            disclosure_type: "top10".into(),
        }];
        let mut quotes = HashMap::new();
        quotes.insert(
            "600519".into(),
            StockQuote { stock_code: "600519".into(), name: "贵州茅台".into(), price: 1650.0, prev_close: 1650.0 },
        );
        let tracked = StockQuote {
            stock_code: "399997".into(),
            name: "中证白酒".into(),
            price: 11016.0,
            prev_close: 10800.0,
        };
        let r = value_fund(ValuationInput {
            fund_code: "161725".into(),
            official_nav: 1.0,
            holdings,
            quotes,
            benchmark: None,
            tracked_index: Some(tracked),
            pure_index: true,
        });
        assert!(r.estimated);
        assert_eq!(r.benchmark_code.as_deref(), Some("399997"));
        assert_eq!(r.benchmark_name.as_deref(), Some("中证白酒"));
        assert_eq!(r.valuation_method.as_deref(), Some("index"));
        // 指数实时估值优先：头条 = 跟踪指数当日涨跌 (11016/10800 - 1)
        assert!((r.est_change_pct - (11016.0 / 10800.0 - 1.0)).abs() < 1e-9, "got {}", r.est_change_pct);
        // 成分股穿透参考：0.7*0 + 0.3*0.02 = 0.006
        assert!((r.penetration_est_change_pct.unwrap_or(0.0) - 0.006).abs() < 1e-9, "got {:?}", r.penetration_est_change_pct);
    }

    #[test]
    fn tracked_index_preferred_over_benchmark() {
        // 同时传入跟踪指数与通用基准时，命名与取值应取跟踪指数（中证白酒），而非沪深300
        let holdings = vec![DisclosedHolding {
            stock_code: "600519".into(),
            stock_name: "贵州茅台".into(),
            weight: 1.0,
            report_period: "2026Q2".into(),
            disclosure_type: "top10".into(),
        }];
        let bench = StockQuote { stock_code: "000300".into(), name: "沪深300".into(), price: 102.0, prev_close: 100.0 };
        let tracked = StockQuote { stock_code: "399997".into(), name: "中证白酒".into(), price: 11550.0, prev_close: 11000.0 };
        let r = value_fund(ValuationInput {
            fund_code: "X".into(),
            official_nav: 1.0,
            holdings,
            quotes: HashMap::new(),
            benchmark: Some(bench),
            tracked_index: Some(tracked),
            pure_index: true,
        });
        assert_eq!(r.benchmark_code.as_deref(), Some("399997"));
        assert_eq!(r.benchmark_name.as_deref(), Some("中证白酒"));
        assert_eq!(r.valuation_method.as_deref(), Some("index"));
        // 指数型基金头条直接用跟踪指数涨跌 (11550/11000 - 1)
        let idx = 11550.0 / 11000.0 - 1.0;
        assert!((r.est_change_pct - idx).abs() < 1e-9, "got {}", r.est_change_pct);
    }

    #[test]
    fn index_fund_without_tracked_index_falls_back() {
        // 指数基金但拿不到跟踪指数行情：退回本地穿透自算（valuation_method=penetration）
        let holdings = vec![DisclosedHolding {
            stock_code: "600519".into(),
            stock_name: "贵州茅台".into(),
            weight: 1.0,
            report_period: "2026Q2".into(),
            disclosure_type: "top10".into(),
        }];
        let bench = StockQuote { stock_code: "000300".into(), name: "沪深300".into(), price: 102.0, prev_close: 100.0 };
        let r = value_fund(ValuationInput {
            fund_code: "X".into(),
            official_nav: 1.0,
            holdings,
            quotes: HashMap::new(),
            benchmark: Some(bench),
            tracked_index: None,
            pure_index: true,
        });
        assert_eq!(r.valuation_method.as_deref(), Some("penetration"));
        // 茅台无行情：其权重归入基准近似（而非按 0 丢弃），头条≈沪深300涨幅 2%。
        let bench_ret = 102.0 / 100.0 - 1.0;
        assert!((r.est_change_pct - bench_ret).abs() < 1e-9, "got {}", r.est_change_pct);
    }

    #[test]
    fn enhanced_index_fund_uses_penetration_not_pure_index() {
        // 指数增强基金（非纯被动）：即使跟踪指数 +2%，头条也不应直接取纯指数涨跌，
        // 而应走穿透口径（披露成分穿透 + 未披露部分按跟踪指数近似），以贴合其跟踪误差。
        let holdings = vec![DisclosedHolding {
            stock_code: "600519".into(),
            stock_name: "贵州茅台".into(),
            weight: 0.7,
            report_period: "2026Q2".into(),
            disclosure_type: "top10".into(),
        }];
        let mut quotes = HashMap::new();
        quotes.insert(
            "600519".into(),
            StockQuote { stock_code: "600519".into(), name: "贵州茅台".into(), price: 1650.0, prev_close: 1650.0 },
        );
        let tracked = StockQuote {
            stock_code: "399997".into(),
            name: "中证白酒".into(),
            price: 11016.0,
            prev_close: 10800.0,
        };
        let r = value_fund(ValuationInput {
            fund_code: "001104".into(), // 某中证白酒指数增强
            official_nav: 1.0,
            holdings,
            quotes,
            benchmark: None,
            tracked_index: Some(tracked),
            pure_index: false, // 指数增强：非纯被动
        });
        assert_eq!(r.valuation_method.as_deref(), Some("penetration"));
        // 头条 = 穿透：0.7*0(茅台当日0%) + 0.3*0.02(未披露按跟踪指数) = 0.006，而非 0.02
        assert!((r.est_change_pct - 0.006).abs() < 1e-9, "got {}", r.est_change_pct);
        // 未披露部分仍按真实跟踪指数近似（中证白酒 +2%），而非退化为沪深300
        assert_eq!(r.benchmark_code.as_deref(), Some("399997"));
    }

    #[test]
    fn no_disclosure_index_fund_still_estimates_via_tracked_index() {
        // 回归：无披露持仓的纯被动指数基金（如 014424 博时恒生医疗保健ETF联接，disclosures=0）
        // 此前因 holdings 为空被早退为 estimated=false；修复后应由「跟踪指数代理」给出估值。
        let tracked = StockQuote {
            stock_code: "HSHCI".into(),
            name: "恒生医疗保健指数".into(),
            price: 3738.79,
            prev_close: 3712.17,
        };
        let r = value_fund(ValuationInput {
            fund_code: "014424".into(),
            official_nav: 0.8945,
            holdings: vec![], // 无披露
            quotes: HashMap::new(),
            benchmark: None,
            tracked_index: Some(tracked),
            pure_index: true, // 被动指数/ETF 联接
        });
        assert!(r.estimated, "无披露的被动指数基金应能用指数代理估算");
        assert_eq!(r.valuation_method.as_deref(), Some("index"));
        // 头条 = 跟踪指数当日涨跌 (3738.79/3712.17 - 1) ≈ +0.7171%
        let idx = 3738.79 / 3712.17 - 1.0;
        assert!((r.est_change_pct - idx).abs() < 1e-9, "got {}", r.est_change_pct);
        // 估算净值 = 0.8945 × (1 + idx) ≈ 0.9010
        assert!((r.est_nav - 0.8945 * (1.0 + idx)).abs() < 1e-9, "got {}", r.est_nav);
    }

    #[test]
    fn no_disclosure_non_index_fund_still_unestimable() {
        // 反向保护：既无披露、又非指数代理（主动/混合基金）仍应 estimated=false。
        let r = value_fund(ValuationInput {
            fund_code: "003095".into(), // 中欧医疗健康混合
            official_nav: 1.0,
            holdings: vec![],
            quotes: HashMap::new(),
            benchmark: None,
            tracked_index: None,
            pure_index: false,
        });
        assert!(!r.estimated);
    }
}

#[test]
fn compute_position_metrics_intraday_baseline_is_prev_nav() {
    // 盘中：基准统一取昨收 prev_nav=3.95。est_ret=4.20/4.00−1=0.05；anchored_est=3.95×1.05=4.1475；
    // 参考市值=4147.5；当日估算=1000×(4.1475−3.95)=197.5（独立的「当日估算收益」列）。
    // 当日实际列 = 份额×(official_nav − prev_nav) = 1000×(4.00−3.95) = 50，即「上日实际」
    // （盘中无今日官方净值，official_nav 为上一交易日收盘、prev_nav 为其昨收），前端标「上次」，绝非估算。
    let m = compute_position_metrics(&PositionMetricsInput {
        shares: 1000.0,
        cost_amount: 4000.0,
        est_nav: 4.20,
        official_nav: 4.00,
        prev_nav: 3.95,
        nav_date: "2026-08-14",
        phase: "intraday",
        today: "2026-08-17",
    });
    assert!((m.baseline_nav - 3.95).abs() < 1e-9);
    assert!((m.market_value - 4147.5).abs() < 1e-9);
    assert!((m.day_pnl_est - 197.5).abs() < 1e-9); // 1000*(4.1475-3.95)
    assert!((m.day_pnl_act - 50.0).abs() < 1e-9); // 上日实际 = 1000*(4.00-3.95)，真实官方口径
    assert!((m.total_pnl - 147.5).abs() < 1e-9);
    assert!((m.total_pnl_pct - 0.036875).abs() < 1e-9);
}

#[test]
fn compute_position_metrics_post_close_actual_uses_official_nav() {
    // 盘后且有昨收基准(prev_nav>0)：当日实际走「官方净值口径」= 份额×(official_nav−prev_nav)，
    // 与当日估算(anchored_est_nav−prev_nav) 使用不同净值源，二者天然不同。
    // baseline=prev_nav=4.00；est_ret=4.20/4.10−1≈0.02439；anchored_est=4.00×1.02439≈4.09756；
    // 当日估算=1000×(4.09756−4.00)≈97.56；当日实际=1000×(4.10−4.00)=100.00。
    // 市值口径（用户约定）：nav_date==today 且 official_nav>0 → 市值=份额×当日实际净值=1000×4.10=4100
    // （而非估算口径 4097.56），保证刷新到当日实际净值后市值不再把今日收益重复叠加。
    let m = compute_position_metrics(&PositionMetricsInput {
        shares: 1000.0,
        cost_amount: 4000.0,
        est_nav: 4.20,
        official_nav: 4.10,
        prev_nav: 4.00,
        nav_date: "2026-08-17",
        phase: "post_close",
        today: "2026-08-17",
    });
    assert!((m.baseline_nav - 4.00).abs() < 1e-9);
    assert!((m.market_value - 4100.0).abs() < 1e-9); // 份额×当日实际净值（真实市值）
    assert!((m.day_pnl_est - 97.5609756).abs() < 1e-6); // 1000*(anchored_est - 4.00)
    assert!((m.day_pnl_act - 100.0).abs() < 1e-9); // 官方净值口径（与估算不同）
    // 比率分母 = shares*baseline = 4000
    assert!((m.day_pnl_pct_act - 0.025).abs() < 1e-9);
}

#[test]
fn compute_position_metrics_post_close_delayed_actual_via_prev_nav() {
    // 盘后/开盘前但官方净值接口被反爬、nav_date 仍停留昨日(08-14 < 08-17)：
    // baseline 取昨收净值(prev_nav=3.95)，当日实际=份额×(official_nav−prev_nav)=1000×(4.00−3.95)=50，
    // 即「上一次净值」(08-14 那日) 相对其昨收基准的实际收益；当日估算=份额×(anchored_est−baseline)。
    let m = compute_position_metrics(&PositionMetricsInput {
        shares: 1000.0,
        cost_amount: 4000.0,
        est_nav: 4.10,
        official_nav: 4.00,
        prev_nav: 3.95,
        nav_date: "2026-08-14",
        phase: "post_close",
        today: "2026-08-17",
    });
    assert!((m.baseline_nav - 3.95).abs() < 1e-9);
    assert!((m.day_pnl_est - 98.75).abs() < 1e-6); // 1000*(anchored_est(=3.95*1.025) - 3.95)，当日估算收益（独立列）
    // 陈旧官方净值(nav_date != today)：当日实际 = 份额×(official_nav − prev_nav) = 1000×(4.00−3.95) = 50，
    // 即「上一次净值」(08-14 那日) 相对其昨收基准的真实实际收益（非估算代理、非冻结假值）。
    assert!((m.day_pnl_act - 50.0).abs() < 1e-9);
}

#[test]
fn compute_position_metrics_post_close_no_prev_nav_uses_official_fallback() {
    // 盘后但 est_cache 首次落地、prev_nav 为 0（无昨收基准）：baseline 退化为 official_nav
    // （最近可得收盘净值）。当日估算=份额×(anchored_est−baseline)=1000×(4.10−4.00)=100；
    // 当日实际=份额×(official_nav−baseline)=0（baseline 已是官方净值，无当日增量）。
    // 真实场景：2026-08-17 周一首次运行，官方净值接口被反爬、prev_nav 全为 0。
    let m = compute_position_metrics(&PositionMetricsInput {
        shares: 1000.0,
        cost_amount: 4000.0,
        est_nav: 4.10,
        official_nav: 4.00,
        prev_nav: 0.0,
        nav_date: "2026-08-17",
        phase: "post_close",
        today: "2026-08-17",
    });
    assert!((m.baseline_nav - 4.00).abs() < 1e-9);
    assert!((m.day_pnl_est - 100.0).abs() < 1e-9); // 1000*(anchored_est - 4.00)
    assert!((m.day_pnl_act).abs() < 1e-9); // baseline=official → 当日实际无增量
}

#[test]
fn compute_position_metrics_closed_shows_prev_nav_actual() {
    // 休市（周末/休盘日）：无当日交易，当日估算=0；参考市值统一使用 anchored_est_nav。
    // est_ret=4.20/4.10−1≈0.02439；anchored_est=4.00×1.02439≈4.09756；
    // 市值=4097.56，避免直接取陈旧 official_nav=4.10 造成的跳变。
    // 当日实际列（day_pnl_act）仍须为真实官方口径 = 份额×(official_nav − prev_nav) = 1000×(4.10−4.00)=100，
    // 即最近一次确认净值(08-14) 相对其昨收基准(4.00) 的真实变动，前端标「上次」——诚实、冻结但不编造；
    // 绝不因 closed 回退为估算代理(0)，否则会丢掉「上一次实际」信息。
    let m = compute_position_metrics(&PositionMetricsInput {
        shares: 1000.0,
        cost_amount: 4000.0,
        est_nav: 4.20,
        official_nav: 4.10,
        prev_nav: 4.00,
        nav_date: "2026-08-14",
        phase: "closed",
        today: "2026-08-17",
    });
    assert!((m.day_pnl_est).abs() < 1e-9);
    assert!((m.day_pnl_act - 100.0).abs() < 1e-9); // 真实官方口径：上次确认净值相对昨收基准的实际变动
    assert!((m.day_pnl_pct_act - 0.025).abs() < 1e-9); // 100 / prev_close_mv(4000)
    assert!((m.market_value - 4097.5609756).abs() < 1e-6);
}

#[test]
fn summarize_portfolio_weight_and_headline() {
    // 组合聚合：持仓占比 + 头条口径（盘中取估算，盘后取实际）。
    let positions = vec![
        PositionForSummary {
            fund_code: "A".into(),
            market_value: 6000.0,
            prev_close_market_value: 5880.0,
            cost_amount: 5000.0,
            total_pnl: 1000.0,
            total_pnl_pct: 0.2,
            day_pnl_est: 120.0,
            day_pnl_act: 90.0,
            day_pnl_pct_est: 0.02,
            day_pnl_pct_act: 0.015,
            estimated: true,
        },
        PositionForSummary {
            fund_code: "B".into(),
            market_value: 4000.0,
            prev_close_market_value: 3960.0,
            cost_amount: 4000.0,
            total_pnl: 0.0,
            total_pnl_pct: 0.0,
            day_pnl_est: 40.0,
            day_pnl_act: 30.0,
            day_pnl_pct_est: 0.01,
            day_pnl_pct_act: 0.0075,
            estimated: true,
        },
    ];
    let s = summarize_portfolio(&positions, "intraday");
    assert!((s.total_market_value - 10000.0).abs() < 1e-9);
    assert!((s.total_pnl - 1000.0).abs() < 1e-9);
    assert!((s.est_day_pnl - 160.0).abs() < 1e-9);
    assert!((s.act_day_pnl - 120.0).abs() < 1e-9);
    // 头条=估算；占比 A=60% B=40%
    assert!((s.positions[0].day_pnl - 120.0).abs() < 1e-9);
    assert!((s.positions[0].weight - 0.6).abs() < 1e-9);
    assert!((s.positions[1].weight - 0.4).abs() < 1e-9);
    // 聚合比率 = est_day_pnl / 昨收总市值 = 160 / (5880+3960)
    let expected_pct = 160.0 / (5880.0 + 3960.0);
    assert!((s.day_pnl_pct_est - expected_pct).abs() < 1e-9);
}

#[test]
fn summarize_portfolio_uses_prev_close_not_current_market_value_for_return() {
    // P3 修复验证：组合当日收益率分母应为「昨收总市值」，而非「当前总市值」。
    // 构造一个上涨场景：当前市值 11000，昨收市值 10000，当日收益 1000。
    // 正确收益率 = 1000/10000 = 10%；若用当前市值则会被低估为 1000/11000 ≈ 9.09%。
    let positions = vec![PositionForSummary {
        fund_code: "UP".into(),
        market_value: 11000.0,
        prev_close_market_value: 10000.0,
        cost_amount: 9000.0,
        total_pnl: 2000.0,
        total_pnl_pct: 0.0,
        day_pnl_est: 1000.0,
        day_pnl_act: 1000.0,
        day_pnl_pct_est: 0.1,
        day_pnl_pct_act: 0.1,
        estimated: true,
    }];
    let s = summarize_portfolio(&positions, "intraday");
    assert!((s.day_pnl_pct_est - 0.10).abs() < 1e-9, "got {}", s.day_pnl_pct_est);
    assert!((s.day_pnl_pct_act - 0.10).abs() < 1e-9, "got {}", s.day_pnl_pct_act);
}

#[test]
fn compute_portfolio_risk_basic() {
    // 两只基金，份额恒定，共同 3 个连续交易日净值序列（2026-08-03/04/05 为周一~周三）。
    let series = vec![
        FundNavSeries {
            shares: 1000.0,
            navs: vec![
                ("2026-08-03".into(), 1.00),
                ("2026-08-04".into(), 1.10),
                ("2026-08-05".into(), 1.21),
            ],
        },
        FundNavSeries {
            shares: 500.0,
            navs: vec![
                ("2026-08-03".into(), 2.00),
                ("2026-08-04".into(), 2.00),
                ("2026-08-05".into(), 2.00),
            ],
        },
    ];
    let r = compute_portfolio_risk(&series).expect("risk should compute");
    // 组合市值：D1=1000*1+500*2=2000；D3=1000*1.21+500*2=2210 → 累计 +10.5%
    assert!((r.cumulative_return_pct - 10.5).abs() < 1e-6, "got {}", r.cumulative_return_pct);
    // days = 首末之间经历的交易日数（稠密 = 点数 − 1 = 2）
    assert_eq!(r.days, 2);
    // 最大回撤应为 0（单调上行）
    assert!(r.max_drawdown_pct <= 1e-6, "got {}", r.max_drawdown_pct);
}

#[test]
fn compute_portfolio_risk_handles_gap_trading_days() {
    // 中间缺数：仅周一(08-03)与周四(08-06)两个点，间隔 3 个交易日（周二/三/四）。
    // 修正点：days/年化按真实交易日 3 计；跨 3 日的收益不进入日波动率样本（vol=0）。
    let series = vec![FundNavSeries {
        shares: 1.0,
        navs: vec![
            ("2026-08-03".into(), 1.00),
            ("2026-08-06".into(), 1.10),
        ],
    }];
    let r = compute_portfolio_risk(&series).expect("risk should compute");
    assert_eq!(r.days, 3, "应计 3 个交易日，got {}", r.days);
    assert!((r.cumulative_return_pct - 10.0).abs() < 1e-6);
    assert_eq!(r.annualized_vol_pct, 0.0, "跨日收益不应计入日波动率");
    assert!(r.annualized_return_pct > 0.0);
}

#[test]
fn compute_portfolio_risk_insufficient_data() {
    let series = vec![FundNavSeries {
        shares: 1000.0,
        navs: vec![("2026-08-01".into(), 1.0)],
    }];
    assert!(compute_portfolio_risk(&series).is_none());
}
