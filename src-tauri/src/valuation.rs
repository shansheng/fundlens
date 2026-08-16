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
}

pub struct ValuationInput {
    pub fund_code: String,
    pub official_nav: f64,
    pub holdings: Vec<DisclosedHolding>,
    pub quotes: std::collections::HashMap<String, StockQuote>,
    /// 基准指数行情（用于近似未披露部分，提升穿透估值准确度）。无则按零波动近似。
    pub benchmark: Option<StockQuote>,
}

pub fn value_fund(input: ValuationInput) -> FundValuationResult {
    let ValuationInput {
        fund_code,
        official_nav,
        holdings,
        quotes,
        benchmark,
    } = input;

    if holdings.is_empty() {
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
        };
    }

    if official_nav <= 0.0 {
        return FundValuationResult {
            fund_code: fund_code.clone(),
            official_nav,
            est_nav: official_nav,
            est_change_pct: 0.0,
            disclosure_type: holdings[0].disclosure_type.clone(),
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
        };
    }

    let mut valued: Vec<HoldingValuation> = Vec::with_capacity(holdings.len());
    let mut portfolio_return = 0.0;
    let mut disclosed_weight_sum = 0.0;

    for h in &holdings {
        let q = quotes.get(&h.stock_code);
        let (price_return, contribution) = match q {
            Some(q) if q.prev_close > 0.0 => {
                let pr = q.price / q.prev_close - 1.0;
                (pr, h.weight * pr)
            }
            _ => (0.0, 0.0), // 缺行情：保守按 0 贡献
        };
        portfolio_return += contribution;
        disclosed_weight_sum += h.weight;
        valued.push(HoldingValuation {
            stock_code: h.stock_code.clone(),
            stock_name: h.stock_name.clone(),
            weight: h.weight,
            price_return,
            contribution,
        });
    }

    // 未披露部分 (1 - disclosed_weight_sum) 用基准指数当日涨跌幅近似，
    // 显著减少对现金/债券/非前十大仓位的"零波动"低估误差。
    let benchmark_weight = (1.0 - disclosed_weight_sum).max(0.0);
    let (benchmark_code, benchmark_name, benchmark_return) = match &benchmark {
        Some(b) if b.prev_close > 0.0 => {
            let r = b.price / b.prev_close - 1.0;
            // 基准身份随传入行情走（标的指数 / 国债指数 / 沪深300），不再硬编码
            (Some(b.stock_code.clone()), Some(b.name.clone()), r)
        }
        _ => (None, None, 0.0),
    };
    portfolio_return += benchmark_weight * benchmark_return;

    let est_nav = official_nav * (1.0 + portfolio_return);
    let est_change_pct = est_nav / official_nav - 1.0;

    FundValuationResult {
        fund_code,
        official_nav,
        est_nav,
        est_change_pct,
        disclosure_type: holdings[0].disclosure_type.clone(),
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
        penetration_est_change_pct: None,
        consensus_est_change_pct: None,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionSummary {
    pub fund_code: String,
    pub market_value: f64,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub day_pnl: f64,
    pub estimated: bool,
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
    pub positions: Vec<PositionSummary>,
}

pub struct PositionForSummary {
    pub fund_code: String,
    pub shares: f64,
    pub avg_cost: f64,
    pub est_nav: f64,
    pub estimated: bool,
    pub official_nav: f64,
    /// 支付宝截图导入等「无份额/净值、仅有持仓金额与收益」的场景，直接给出市值与盈亏覆盖
    pub explicit_market_value: Option<f64>,
    pub explicit_total_pnl: Option<f64>,
    pub explicit_day_pnl: Option<f64>,
}

pub fn summarize_portfolio(positions: &[PositionForSummary]) -> PortfolioSummary {
    let mut total_market_value = 0.0;
    let mut total_cost = 0.0;
    let mut total_pnl = 0.0;
    let mut day_pnl = 0.0;
    let mut out = Vec::with_capacity(positions.len());

    for p in positions {
        let nav = if p.estimated { p.est_nav } else { p.official_nav };
        let market_value = p.explicit_market_value.unwrap_or_else(|| p.shares * nav);
        let cost = p.shares * p.avg_cost;
        let pnl = p.explicit_total_pnl.unwrap_or_else(|| market_value - cost);
        let day = p.explicit_day_pnl.unwrap_or_else(|| p.shares * (p.est_nav - p.official_nav));
        total_market_value += market_value;
        total_cost += cost;
        total_pnl += pnl;
        day_pnl += day;
        out.push(PositionSummary {
            fund_code: p.fund_code.clone(),
            market_value,
            pnl,
            pnl_pct: if cost > 0.0 {
                pnl / cost
            } else if market_value > 0.0 {
                pnl / market_value
            } else {
                0.0
            },
            day_pnl: day,
            estimated: p.estimated,
        });
    }

    PortfolioSummary {
        total_market_value,
        total_cost,
        total_pnl,
        total_pnl_pct: if total_cost > 0.0 { total_pnl / total_cost } else { 0.0 },
        est_day_pnl: day_pnl,
        act_day_pnl: day_pnl,
        positions: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("\"estNav\""), "expected estNav in {s}");
        assert!(s.contains("\"estChangePct\""), "expected estChangePct in {s}");
        assert!(s.contains("\"disclosedWeightSum\""), "expected disclosedWeightSum in {s}");
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
}
