// 基金穿透（Look-through）聚合引擎 — 纯函数，与 IO 完全解耦，可直接单测。
// 设计原则（规划 v1.1 §2，P0 红线，实现不得走样）：
//   1. 穿透公式：股票 s 穿透市值 = Σᵢ(基金 i 市值 × wᵢₛ)，wᵢₛ=最新披露期占净值比例；
//   2. 不放大原则：披露权重直接使用，绝不按股票仓位归一化放大；
//   3. 未穿透桶显式单列（现金/债券/未披露），让用户永远看到「我知道多少、不知道多少」；
//   4. 覆盖率：基金覆盖率=min(1,Σw)，组合覆盖率=Σ(基金市值×基金覆盖率)/总市值；
//   5. 货基/理财/金额兜底持仓不产生任何个股行，全额计入未穿透；
//   6. 境外股票（5 位=港股 / 字母=美股）P0 整桶归「境外资产」，穿透市值照常计入；
//   7. 当日行业贡献 = Σ(股票穿透市值×当日涨跌幅)，仅交易时段传入行情时计算（近似值，未穿透不计入）。
// 只读分析层：本模块不回写任何表、不产生流水（v9 模型红线）。

use std::collections::HashMap;

use serde::Serialize;

use crate::db;

// ============ 行业分类：9 大类 L1（东财行业名 → 大类映射常量） ============

pub const SECTOR_UNPENETRATED: &str = "未穿透";
pub const SECTOR_OVERSEAS: &str = "境外资产";
pub const SECTOR_UNCLASSIFIED: &str = "未分类";

/// L2 细分行业 = 东财行业名直出（industry_em 原样），零额外维护成本。
/// 本函数只做 L1 映射：精确命中优先，再走包含规则（对接口字段微调容错），未命中归「未分类」。
pub fn sector_l1_of(industry_em: &str) -> &'static str {
    let name = industry_em.trim();
    if name.is_empty() {
        return SECTOR_UNCLASSIFIED;
    }
    // 精确表：主流东财行业板块名（顺序无关；持续可补充）
    const EXACT: &[(&str, &str)] = &[
        // 医药医疗
        ("化学制药", "医药医疗"), ("中药", "医药医疗"), ("生物制品", "医药医疗"),
        ("医疗器械", "医药医疗"), ("医疗服务", "医药医疗"), ("医药商业", "医药医疗"),
        ("医疗美容", "医药医疗"), ("兽药", "医药医疗"), ("原料药", "医药医疗"),
        // 主要消费
        ("酿酒行业", "主要消费"), ("食品饮料", "主要消费"), ("农牧饲渔", "主要消费"),
        ("调味发酵品", "主要消费"), ("种业", "主要消费"), ("养殖业", "主要消费"),
        // 可选消费
        ("汽车整车", "可选消费"), ("汽车零部件", "可选消费"), ("汽车服务", "可选消费"),
        ("家用电器", "可选消费"), ("小家电", "可选消费"), ("黑色家电", "可选消费"),
        ("厨卫电器", "可选消费"), ("照明设备", "可选消费"), ("家用轻工", "可选消费"),
        ("旅游酒店", "可选消费"), ("旅游及景区", "可选消费"), ("商贸零售", "可选消费"),
        ("纺织服装", "可选消费"), ("服装家纺", "可选消费"), ("珠宝首饰", "可选消费"),
        ("美容护理", "可选消费"), ("专业连锁", "可选消费"), ("互联网电商", "可选消费"),
        // 科技 TMT
        ("半导体", "科技TMT"), ("电子元件", "科技TMT"), ("光学光电子", "科技TMT"),
        ("消费电子", "科技TMT"), ("电子化学品", "科技TMT"), ("通信设备", "科技TMT"),
        ("通信服务", "科技TMT"), ("软件开发", "科技TMT"), ("互联网服务", "科技TMT"),
        ("计算机设备", "科技TMT"), ("游戏", "科技TMT"), ("文化传媒", "科技TMT"),
        ("数字媒体", "科技TMT"), ("影视院线", "科技TMT"), ("出版", "科技TMT"),
        // 金融地产
        ("银行", "金融地产"), ("保险", "金融地产"), ("证券", "金融地产"),
        ("多元金融", "金融地产"), ("房地产", "金融地产"), ("房地产开发", "金融地产"),
        ("房地产服务", "金融地产"),
        // 高端制造
        ("工程机械", "高端制造"), ("通用设备", "高端制造"), ("专用设备", "高端制造"),
        ("仪器仪表", "高端制造"), ("电力设备", "高端制造"), ("光伏设备", "高端制造"),
        ("风电设备", "高端制造"), ("电池", "高端制造"), ("电机", "高端制造"),
        ("电网设备", "高端制造"), ("自动化设备", "高端制造"),
        // 周期资源
        ("煤炭行业", "周期资源"), ("钢铁行业", "周期资源"), ("有色金属", "周期资源"),
        ("工业金属", "周期资源"), ("贵金属", "周期资源"), ("能源金属", "周期资源"),
        ("小金属", "周期资源"), ("石油行业", "周期资源"), ("化肥行业", "周期资源"),
        ("化学原料", "周期资源"), ("化学制品", "周期资源"), ("化纤行业", "周期资源"),
        ("农药", "周期资源"), ("水泥建材", "周期资源"), ("玻璃玻纤", "周期资源"),
        ("非金属材料", "周期资源"), ("采掘服务", "周期资源"),
        // 基建公用·交运
        ("电力行业", "基建公用·交运"), ("燃气", "基建公用·交运"), ("水务", "基建公用·交运"),
        ("航运港口", "基建公用·交运"), ("铁路公路", "基建公用·交运"), ("物流行业", "基建公用·交运"),
        ("航空机场", "基建公用·交运"), ("工程建设", "基建公用·交运"), ("环境治理", "基建公用·交运"),
        // 国防军工
        ("航天航空", "国防军工"), ("船舶制造", "国防军工"),
    ];
    for (k, v) in EXACT {
        if name == *k {
            return v;
        }
    }
    // 包含规则：字段名微调容错（注意顺序，先具体后宽泛；「电力设备」已在精确表优先命中）
    const CONTAINS: &[(&str, &str)] = &[
        ("农药", "周期资源"),          // 须先于 contains("药") 兜底
        ("医院", "医药医疗"),
        ("药", "医药医疗"),
        ("医", "医药医疗"),
        ("酒", "主要消费"),
        ("食品", "主要消费"),
        ("农牧", "主要消费"),
        ("饲渔", "主要消费"),
        ("汽车", "可选消费"),
        ("家电", "可选消费"),
        ("旅游", "可选消费"),
        ("酒店", "可选消费"),
        ("零售", "可选消费"),
        ("纺织", "可选消费"),
        ("珠宝", "可选消费"),
        ("半导体", "科技TMT"),
        ("软件", "科技TMT"),
        ("计算机", "科技TMT"),
        ("通信", "科技TMT"),
        ("互联网", "科技TMT"),
        ("游戏", "科技TMT"),
        ("传媒", "科技TMT"),
        ("电子", "科技TMT"),
        ("数据", "科技TMT"),
        ("银行", "金融地产"),
        ("证券", "金融地产"),
        ("保险", "金融地产"),
        ("地产", "金融地产"),
        ("光伏", "高端制造"),
        ("风电", "高端制造"),
        ("电池", "高端制造"),
        ("工程机械", "高端制造"),
        ("仪器仪表", "高端制造"),
        ("机器人", "高端制造"),
        ("煤炭", "周期资源"),
        ("钢铁", "周期资源"),
        ("有色", "周期资源"),
        ("贵金属", "周期资源"),
        ("石油", "周期资源"),
        ("石化", "周期资源"),
        ("化学", "周期资源"),
        ("化肥", "周期资源"),
        ("化纤", "周期资源"),
        ("水泥", "周期资源"),
        ("玻璃", "周期资源"),
        ("电力", "基建公用·交运"),   // 「电力设备」已被精确表命中高端制造
        ("水务", "基建公用·交运"),
        ("燃气", "基建公用·交运"),
        ("物流", "基建公用·交运"),
        ("航运", "基建公用·交运"),
        ("港口", "基建公用·交运"),
        ("铁路", "基建公用·交运"),
        ("公路", "基建公用·交运"),
        ("机场", "基建公用·交运"),
        ("工程建设", "基建公用·交运"),
        ("航天", "国防军工"),
        ("军工", "国防军工"),
    ];
    for (k, v) in CONTAINS {
        if name.contains(k) {
            return v;
        }
    }
    SECTOR_UNCLASSIFIED
}

// ============ 聚合输入 / 输出数据结构 ============

/// 单只基金输入。命令层负责：市值口径（份额×最新官方净值，货基/兜底用持仓金额）、
/// 最新披露期筛选（list_disclosures_batch 已保证每基金仅最新期）。
#[derive(Debug, Clone)]
pub struct LtFundInput {
    pub code: String,
    pub name: String,
    /// 基金市值（官方净值口径；与总览估算链路同源的披露权重在此市值上展开）
    pub market_value: f64,
    /// 货基/理财（fund_type 002/005）：整只计入未穿透，不产生任何个股行
    pub is_money_or_wealth: bool,
    /// 是否有真实 6 位代码且份额>0（金额兜底持仓=false → 整只计入未穿透）
    pub has_real_code: bool,
    /// 最新披露期（如 "2026Q2"）；无披露为 None
    pub report_period: Option<String>,
    /// 最新披露期持仓
    pub holdings: Vec<crate::valuation::DisclosedHolding>,
}

/// 行情轻量输入（交易时段才传入；key=纯数字股票代码，与 fetch_quotes 返回口径一致）
#[derive(Debug, Clone, Copy)]
pub struct QuoteLite {
    pub change_pct: f64, // price/prev_close − 1；prev_close≤0 时为 0
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndustrySlice {
    /// L1 大类名 / L2 细分行业名（东财行业名直出）
    pub key: String,
    pub market_value: f64,
    /// ÷ 组合总市值（两级分母一致）
    pub pct: f64,
    /// 当日贡献 = Σ(成分股票穿透市值×当日涨跌幅)；无行情（非交易时段/虚拟桶）为 None
    pub day_contribution: Option<f64>,
    /// 虚拟桶（未穿透/境外/未分类）——UI 用虚线样式区隔
    pub is_virtual: bool,
    /// L2 → 所属 L1；L1 行为 None
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StockFundWeight {
    pub fund_code: String,
    pub fund_name: String,
    /// 该股在该基金中的占净值比例
    pub weight: f64,
    /// 分到该股票的穿透市值
    pub contributed_mv: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StockRow {
    pub stock_code: String,
    pub stock_name: String,
    pub sector_l1: String,
    pub industry_l2: String,
    pub market_value: f64,
    /// 穿透权重 = market_value / 组合总市值
    pub pct: f64,
    pub fund_count: usize,
    /// 贡献基金明细（按贡献市值降序）
    pub funds: Vec<StockFundWeight>,
    pub day_change_pct: Option<f64>,
    pub day_contribution: Option<f64>,
    /// 隐性重仓预警（已裁定阈值）：同一股票经 ≥3 只基金持有且合计穿透占比 >5%
    pub hidden_warning: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FundInfoRow {
    pub code: String,
    pub name: String,
    pub market_value: f64,
    /// 基金覆盖率 = min(1, Σw)
    pub coverage: f64,
    pub report_period: Option<String>,
    /// 该基金未穿透市值 = 市值 ×(1 − 覆盖率)（现金/债券/未披露）
    pub unpenetrated_mv: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LookthroughResult {
    pub total_mv: f64,
    /// 组合覆盖率 = Σ(基金市值×基金覆盖率) ÷ 总市值
    pub coverage: f64,
    /// 报告期分布（如 "2026Q2×5"、"2026Q1×3"、"无披露×2"），按基金数降序
    pub report_periods: Vec<String>,
    /// L1 行业穿透（含未穿透/境外/未分类虚拟桶；Σpct=100%）
    pub industries_l1: Vec<IndustrySlice>,
    /// L2 细分穿透（东财行业名直出；虚拟桶自带同名/分市场子项）
    pub industries_l2: Vec<IndustrySlice>,
    /// 个股虚拟重仓表（按穿透市值降序）
    pub stocks: Vec<StockRow>,
    /// 个股集中度：前 5 / 前 10 穿透权重合计
    pub cr5: f64,
    pub cr10: f64,
    /// 各基金穿透明细（覆盖率/报告期/未穿透市值）
    pub funds: Vec<FundInfoRow>,
    pub unpenetrated_mv: f64,
    /// 是否带当日行情（仅交易时段 true；false 时前端隐藏当日列）
    pub has_quotes: bool,
    pub as_of: String,
}

// ============ 聚合纯函数 ============

struct StockAgg {
    name: String,
    mv: f64,
    funds: Vec<StockFundWeight>,
}

/// 股票市场判别：6 位数字=A / 5 位数字=港股 / 其余（字母）=美股
fn market_of(stock_code: &str) -> &'static str {
    let s = stock_code.trim();
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        "A"
    } else if s.len() == 5 && s.chars().all(|c| c.is_ascii_digit()) {
        "HK"
    } else {
        "US"
    }
}

pub fn aggregate(
    funds: &[LtFundInput],
    profiles: &HashMap<String, db::StockProfileRow>,
    quotes: &HashMap<String, QuoteLite>,
    has_quotes: bool,
    as_of: &str,
) -> LookthroughResult {
    let total_mv: f64 = funds.iter().map(|f| f.market_value).sum();

    // ---- 逐基金展开：个股穿透市值 + 未穿透桶 + 基金明细 ----
    let mut stock_aggs: HashMap<String, StockAgg> = HashMap::new();
    let mut funds_info: Vec<FundInfoRow> = Vec::new();
    let mut unpenetrated_mv = 0.0f64;
    let mut period_counts: HashMap<String, usize> = HashMap::new();

    for f in funds {
        let skip = f.is_money_or_wealth || !f.has_real_code || f.holdings.is_empty();
        if skip {
            // 口径 #5/#6：货基/理财/金额兜底/无披露 → 全额计入未穿透，绝不反推股票暴露
            unpenetrated_mv += f.market_value;
            if let Some(p) = &f.report_period {
                *period_counts.entry(p.clone()).or_insert(0) += 1;
            } else if f.holdings.is_empty() {
                *period_counts.entry("无披露".to_string()).or_insert(0) += 1;
            }
            funds_info.push(FundInfoRow {
                code: f.code.clone(),
                name: f.name.clone(),
                market_value: f.market_value,
                coverage: 0.0,
                report_period: f.report_period.clone(),
                unpenetrated_mv: f.market_value,
            });
            continue;
        }
        let sum_w: f64 = f.holdings.iter().map(|h| h.weight).sum();
        // 口径 #4：覆盖率 = min(1, Σw)——Σw 异常 >1 时截断，保证未穿透不为负
        let coverage = sum_w.min(1.0).max(0.0);
        unpenetrated_mv += f.market_value * (1.0 - coverage);
        if let Some(p) = &f.report_period {
            *period_counts.entry(p.clone()).or_insert(0) += 1;
        }
        for h in &f.holdings {
            let contributed = f.market_value * h.weight;
            let agg = stock_aggs.entry(h.stock_code.clone()).or_insert_with(|| StockAgg {
                name: h.stock_name.clone(),
                mv: 0.0,
                funds: Vec::new(),
            });
            agg.mv += contributed;
            agg.funds.push(StockFundWeight {
                fund_code: f.code.clone(),
                fund_name: f.name.clone(),
                weight: h.weight,
                contributed_mv: contributed,
            });
        }
        funds_info.push(FundInfoRow {
            code: f.code.clone(),
            name: f.name.clone(),
            market_value: f.market_value,
            coverage,
            report_period: f.report_period.clone(),
            unpenetrated_mv: f.market_value * (1.0 - coverage),
        });
    }

    // ---- 个股行（分类 + 当日涨跌/贡献 + 隐性重仓预警） ----
    let mut stocks: Vec<StockRow> = stock_aggs
        .into_iter()
        .map(|(code, agg)| {
            let (sector_l1, industry_l2) = match market_of(&code) {
                "A" => {
                    match profiles.get(&code) {
                        Some(p) if !p.industry_em.is_empty() => {
                            // L1 优先用画像映射结果；画像缺 L1 时即时映射（双保险）
                            let l1 = if p.sector_l1.is_empty() {
                                sector_l1_of(&p.industry_em).to_string()
                            } else {
                                p.sector_l1.clone()
                            };
                            (l1, p.industry_em.clone())
                        }
                        _ => (SECTOR_UNCLASSIFIED.to_string(), "待补行业".to_string()),
                    }
                }
                "HK" => (SECTOR_OVERSEAS.to_string(), "港股".to_string()),
                _ => (SECTOR_OVERSEAS.to_string(), "美股".to_string()),
            };
            let mut funds = agg.funds;
            funds.sort_by(|a, b| {
                b.contributed_mv
                    .partial_cmp(&a.contributed_mv)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let pct = if total_mv > 0.0 { agg.mv / total_mv } else { 0.0 };
            let day_change_pct = quotes.get(&code).map(|q| q.change_pct);
            let day_contribution = day_change_pct.map(|r| agg.mv * r);
            let fund_count = funds.len();
            StockRow {
                stock_code: code,
                stock_name: agg.name,
                sector_l1,
                industry_l2,
                market_value: agg.mv,
                pct,
                fund_count,
                funds,
                day_change_pct,
                day_contribution,
                // 已裁定阈值（规划 §10）：≥3 只基金持有且合计穿透占比 >5%
                hidden_warning: fund_count >= 3 && pct > 0.05,
            }
        })
        .collect();
    stocks.sort_by(|a, b| {
        b.market_value
            .partial_cmp(&a.market_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ---- 行业聚合（L1 / L2 两级，分母一致=组合总市值） ----
    let mut l1_mv: HashMap<String, (f64, f64)> = HashMap::new(); // (mv, day_contribution)
    let mut l2_mv: HashMap<(String, String), (f64, f64)> = HashMap::new(); // (l1, l2) → (mv, contribution)
    let mut add_l1 = |key: &str, mv: f64, contrib: Option<f64>| {
        let e = l1_mv.entry(key.to_string()).or_insert((0.0, 0.0));
        e.0 += mv;
        if let Some(c) = contrib {
            e.1 += c;
        }
    };
    for s in &stocks {
        let e = l1_mv.entry(s.sector_l1.clone()).or_insert((0.0, 0.0));
        e.0 += s.market_value;
        if let Some(c) = s.day_contribution {
            e.1 += c;
        }
        let e2 = l2_mv
            .entry((s.sector_l1.clone(), s.industry_l2.clone()))
            .or_insert((0.0, 0.0));
        e2.0 += s.market_value;
        if let Some(c) = s.day_contribution {
            e2.1 += c;
        }
    }
    // 未穿透虚拟桶（L2 子项 = 现金理财·未披露）
    {
        let e = l1_mv
            .entry(SECTOR_UNPENETRATED.to_string())
            .or_insert((0.0, 0.0));
        e.0 += unpenetrated_mv;
    }
    l2_mv.insert(
        (SECTOR_UNPENETRATED.to_string(), "现金理财·未披露".to_string()),
        (unpenetrated_mv, 0.0),
    );

    let mut industries_l1: Vec<IndustrySlice> = l1_mv
        .into_iter()
        .map(|(key, (mv, contrib))| {
            let is_virtual = matches!(
                key.as_str(),
                SECTOR_UNPENETRATED | SECTOR_OVERSEAS | SECTOR_UNCLASSIFIED
            );
            IndustrySlice {
                key,
                market_value: mv,
                pct: if total_mv > 0.0 { mv / total_mv } else { 0.0 },
                day_contribution: if is_virtual { None } else { Some(contrib) },
                is_virtual,
                parent: None,
            }
        })
        .collect();
    // 排序：市值降序；未穿透虚拟桶固定垫底（视觉区隔）
    industries_l1.sort_by(|a, b| {
        if (a.key == SECTOR_UNPENETRATED) != (b.key == SECTOR_UNPENETRATED) {
            return (b.key == SECTOR_UNPENETRATED).cmp(&(a.key == SECTOR_UNPENETRATED));
        }
        b.market_value
            .partial_cmp(&a.market_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut industries_l2: Vec<IndustrySlice> = l2_mv
        .into_iter()
        .map(|((parent, key), (mv, contrib))| {
            let is_virtual = matches!(
                parent.as_str(),
                SECTOR_UNPENETRATED | SECTOR_OVERSEAS | SECTOR_UNCLASSIFIED
            );
            IndustrySlice {
                key,
                market_value: mv,
                pct: if total_mv > 0.0 { mv / total_mv } else { 0.0 },
                day_contribution: if parent == SECTOR_UNPENETRATED {
                    None
                } else {
                    Some(contrib)
                },
                is_virtual,
                parent: Some(parent),
            }
        })
        .collect();
    industries_l2.sort_by(|a, b| {
        if (a.parent.as_deref() == Some(SECTOR_UNPENETRATED))
            != (b.parent.as_deref() == Some(SECTOR_UNPENETRATED))
        {
            return (b.parent.as_deref() == Some(SECTOR_UNPENETRATED))
                .cmp(&(a.parent.as_deref() == Some(SECTOR_UNPENETRATED)));
        }
        b.market_value
            .partial_cmp(&a.market_value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // ---- 集中度 / 组合覆盖率 / 报告期分布 ----
    let cr5: f64 = stocks.iter().take(5).map(|s| s.pct).sum();
    let cr10: f64 = stocks.iter().take(10).map(|s| s.pct).sum();
    let coverage = if total_mv > 0.0 {
        funds_info
            .iter()
            .map(|f| f.market_value * f.coverage)
            .sum::<f64>()
            / total_mv
    } else {
        0.0
    };
    let mut report_periods: Vec<String> = period_counts
        .into_iter()
        .map(|(p, n)| format!("{}×{}", p, n))
        .collect();
    report_periods.sort();

    LookthroughResult {
        total_mv,
        coverage,
        report_periods,
        industries_l1,
        industries_l2,
        stocks,
        cr5,
        cr10,
        funds: funds_info,
        unpenetrated_mv,
        has_quotes,
        as_of: as_of.to_string(),
    }
}

// ============ 不变量单测（规划 §4.3：写进测试的 5 条红线） ============

#[cfg(test)]
mod tests {
    use super::*;
    use crate::valuation::DisclosedHolding;

    fn h(code: &str, name: &str, w: f64) -> DisclosedHolding {
        DisclosedHolding {
            stock_code: code.to_string(),
            stock_name: name.to_string(),
            weight: w,
            report_period: "2026Q2".to_string(),
            disclosure_type: "top10".to_string(),
        }
    }

    fn fund(code: &str, name: &str, mv: f64, holdings: Vec<DisclosedHolding>) -> LtFundInput {
        LtFundInput {
            code: code.to_string(),
            name: name.to_string(),
            market_value: mv,
            is_money_or_wealth: false,
            has_real_code: true,
            report_period: Some("2026Q2".to_string()),
            holdings,
        }
    }

    fn profiles_for(entries: &[(&str, &str)]) -> HashMap<String, db::StockProfileRow> {
        entries
            .iter()
            .map(|(code, ind)| {
                (
                    code.to_string(),
                    db::StockProfileRow {
                        stock_code: code.to_string(),
                        name: String::new(),
                        industry_em: ind.to_string(),
                        sector_l1: sector_l1_of(ind).to_string(),
                        market: "A".to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn invariant_l1_weights_sum_to_100pct() {
        // 不变量 1：Σ(L1 行业 + 未穿透 + 境外 + 未分类) = 100% ± 1e-6
        let funds = vec![
            fund("110011", "易方达", 100_000.0, vec![h("600519", "贵州茅台", 0.06)]),
            fund("161725", "招商中证白酒", 50_000.0, vec![h("000858", "五粮液", 0.10)]),
        ];
        let r = aggregate(&funds, &profiles_for(&[("600519", "酿酒行业"), ("000858", "酿酒行业")]), &HashMap::new(), false, "t");
        let sum: f64 = r.industries_l1.iter().map(|s| s.pct).sum();
        assert!((sum - 1.0).abs() < 1e-6, "L1 合计 {sum} ≠ 100%");
        // 未穿透 = 100000×(1−0.06) + 50000×(1−0.10) = 94000 + 45000 = 139000
        let unpen = r.industries_l1.iter().find(|s| s.key == SECTOR_UNPENETRATED).unwrap();
        assert!((unpen.pct - (139_000.0 / 150_000.0)).abs() < 1e-9);
    }

    #[test]
    fn invariant_l2_sums_match_l1() {
        // 不变量 5：L2 细分权重合计 = 所属 L1 大类权重（两级自洽）
        let funds = vec![fund(
            "110011",
            "易方达",
            100_000.0,
            vec![h("600519", "贵州茅台", 0.10), h("300750", "宁德时代", 0.08)],
        )];
        let r = aggregate(
            &funds,
            &profiles_for(&[("600519", "酿酒行业"), ("300750", "电池")]),
            &HashMap::new(),
            false,
            "t",
        );
        for l1 in &r.industries_l1 {
            let l2_sum: f64 = r
                .industries_l2
                .iter()
                .filter(|s| s.parent.as_deref() == Some(l1.key.as_str()))
                .map(|s| s.market_value)
                .sum();
            assert!(
                (l2_sum - l1.market_value).abs() < 1e-6,
                "L1 {} 的 L2 合计 {l2_sum} ≠ {}",
                l1.key,
                l1.market_value
            );
        }
    }

    #[test]
    fn invariant_stock_mv_not_amplified() {
        // 不变量 2：单只股票穿透市值 ≤ 持有它的基金市值之和（不放大原则）
        let funds = vec![
            fund("110011", "A", 100_000.0, vec![h("600519", "贵州茅台", 0.10)]),
            fund("161725", "B", 80_000.0, vec![h("600519", "贵州茅台", 0.50)]),
            fund("005827", "C", 60_000.0, vec![h("600519", "贵州茅台", 0.90)]),
        ];
        let r = aggregate(&funds, &profiles_for(&[("600519", "酿酒行业")]), &HashMap::new(), false, "t");
        let s = r.stocks.iter().find(|s| s.stock_code == "600519").unwrap();
        // 10000 + 40000 + 54000 = 104000 ≤ 240000
        assert!((s.market_value - 104_000.0).abs() < 1e-6);
        assert_eq!(s.fund_count, 3);
        // 组合总市值 240000 → pct = 104000/240000
        assert!((s.pct - 104_000.0 / 240_000.0).abs() < 1e-9);
    }

    #[test]
    fn invariant_money_funds_never_emit_stock_rows() {
        // 不变量 4：货基/理财/金额兜底不产生任何个股行，全额计入未穿透
        let funds = vec![
            LtFundInput {
                code: "000198".into(),
                name: "天弘余额宝".into(),
                market_value: 20_000.0,
                is_money_or_wealth: true,
                has_real_code: true,
                report_period: None,
                holdings: vec![],
            },
            LtFundInput {
                code: "XX占位".into(),
                name: "金额兜底持仓".into(),
                market_value: 5_000.0,
                is_money_or_wealth: false,
                has_real_code: false,
                report_period: None,
                holdings: vec![h("600519", "贵州茅台", 0.9)],
            },
            fund("110011", "易方达", 75_000.0, vec![h("600519", "贵州茅台", 0.08)]),
        ];
        let r = aggregate(&funds, &profiles_for(&[("600519", "酿酒行业")]), &HashMap::new(), false, "t");
        assert_eq!(r.stocks.len(), 1, "货基/兜底不得产生个股行");
        assert_eq!(r.stocks[0].fund_count, 1);
        // 未穿透 = 货基 20000 + 兜底 5000 + 易方达 75000×(1−0.08) = 94000
        assert!((r.unpenetrated_mv - 94_000.0).abs() < 1e-6);
        assert_eq!(r.report_periods.iter().filter(|p| p.starts_with("无披露")).count(), 1);
    }

    #[test]
    fn invariant_coverage_clamped_when_weights_exceed_one() {
        // 不变量 3（推广）：Σw 异常 >1 时覆盖率截断为 1，未穿透不为负
        let funds = vec![fund(
            "110011",
            "易方达",
            100_000.0,
            vec![h("600519", "贵州茅台", 0.7), h("000858", "五粮液", 0.6)],
        )];
        let r = aggregate(&funds, &profiles_for(&[("600519", "酿酒行业"), ("000858", "酿酒行业")]), &HashMap::new(), false, "t");
        assert!((r.coverage - 1.0).abs() < 1e-9);
        assert!(r.unpenetrated_mv >= -1e-9, "未穿透不得为负");
        assert!((r.unpenetrated_mv - 0.0).abs() < 1e-9);
    }

    #[test]
    fn invariant_day_contribution_matches_manual_sum() {
        // 当日贡献 = Σ(穿透市值 × 当日涨跌幅)；仅交易时段（has_quotes）计算
        let funds = vec![fund(
            "110011",
            "易方达",
            100_000.0,
            vec![h("600519", "贵州茅台", 0.10), h("300750", "宁德时代", 0.08)],
        )];
        let mut quotes = HashMap::new();
        quotes.insert("600519".to_string(), QuoteLite { change_pct: 0.02 });
        quotes.insert("300750".to_string(), QuoteLite { change_pct: -0.01 });
        let r = aggregate(
            &funds,
            &profiles_for(&[("600519", "酿酒行业"), ("300750", "电池")]),
            &quotes,
            true,
            "t",
        );
        // 手算：10000×0.02 + 8000×(−0.01) = 200 − 80 = 120
        assert!((r.stocks[0].day_contribution.unwrap() + r.stocks[1].day_contribution.unwrap() - 120.0).abs() < 1e-6);
        let l1_total: f64 = r
            .industries_l1
            .iter()
            .filter_map(|s| s.day_contribution)
            .sum();
        assert!((l1_total - 120.0).abs() < 1e-6, "行业贡献合计与个股一致");
        assert!(r.has_quotes);
    }

    #[test]
    fn overseas_stocks_bucketed_and_classified() {
        // 口径 #7：境外股票（5 位=港股 / 字母=美股）整桶归「境外资产」，市值照常计入
        let funds = vec![fund(
            "014424",
            "QDII",
            100_000.0,
            vec![h("00700", "腾讯控股", 0.09), h("AAPL", "苹果", 0.05)],
        )];
        let r = aggregate(&funds, &HashMap::new(), &HashMap::new(), false, "t");
        let overseas = r.industries_l1.iter().find(|s| s.key == SECTOR_OVERSEAS).unwrap();
        assert!((overseas.market_value - 14_000.0).abs() < 1e-6);
        assert!(overseas.is_virtual);
        // L2 分市场自洽：港股 9000 + 美股 5000
        let hk = r.industries_l2.iter().find(|s| s.key == "港股").unwrap();
        let us = r.industries_l2.iter().find(|s| s.key == "美股").unwrap();
        assert!((hk.market_value - 9_000.0).abs() < 1e-6);
        assert!((us.market_value - 5_000.0).abs() < 1e-6);
    }

    #[test]
    fn hidden_warning_threshold_as_decided() {
        // 已裁定阈值：≥3 只基金持有且合计穿透占比 >5% 触发预警
        let funds = vec![
            fund("A1", "甲", 100_000.0, vec![h("600519", "贵州茅台", 0.09)]),
            fund("A2", "乙", 100_000.0, vec![h("600519", "贵州茅台", 0.09)]),
            fund("A3", "丙", 100_000.0, vec![h("600519", "贵州茅台", 0.09)]),
            // 2 只基金高权重但只有 2 只 → 不触发（基金数不足，即使穿透占比 >5%）
            fund("B1", "丁", 100_000.0, vec![h("000858", "五粮液", 0.15)]),
            fund("B2", "戊", 100_000.0, vec![h("000858", "五粮液", 0.15)]),
        ];
        let r = aggregate(&funds, &profiles_for(&[("600519", "酿酒行业"), ("000858", "酿酒行业")]), &HashMap::new(), false, "t");
        let maotai = r.stocks.iter().find(|s| s.stock_code == "600519").unwrap();
        // 3 只 × 9000 = 27000 / 500000 = 5.4% > 5%，且基金数 3 ≥ 3 → 触发
        assert!(maotai.hidden_warning, "3 只基金合计 5.4% 应触发预警");
        let wuliangye = r.stocks.iter().find(|s| s.stock_code == "000858").unwrap();
        assert!(!wuliangye.hidden_warning, "2 只基金即使 6% 也不触发（基金数<3）");
    }

    #[test]
    fn cr5_cr10_concentration() {
        let mut holdings = Vec::new();
        for i in 0..12 {
            holdings.push(h(&format!("60000{}", i), &format!("股{}", i), 0.05));
        }
        let funds = vec![fund("110011", "易方达", 100_000.0, holdings)];
        let r = aggregate(&funds, &HashMap::new(), &HashMap::new(), false, "t");
        // 12 只各 5000 → CR5 = 5×5000/100000 = 0.25；CR10 = 0.5
        assert!((r.cr5 - 0.25).abs() < 1e-9);
        assert!((r.cr10 - 0.50).abs() < 1e-9);
    }

    #[test]
    fn sector_mapping_covers_common_industries() {
        // 映射表抽查：主流东财行业名命中正确大类；「电力设备」不得误归基建（精确表优先）
        assert_eq!(sector_l1_of("酿酒行业"), "主要消费");
        assert_eq!(sector_l1_of("半导体"), "科技TMT");
        assert_eq!(sector_l1_of("光伏设备"), "高端制造");
        assert_eq!(sector_l1_of("电力设备"), "高端制造");
        assert_eq!(sector_l1_of("电力行业"), "基建公用·交运");
        assert_eq!(sector_l1_of("农药"), "周期资源"); // 不被 contains("药") 误归医药
        assert_eq!(sector_l1_of("化学制药"), "医药医疗");
        assert_eq!(sector_l1_of("航空机场"), "基建公用·交运");
        assert_eq!(sector_l1_of("航天航空"), "国防军工");
        assert_eq!(sector_l1_of("房地产开发"), "金融地产");
        assert_eq!(sector_l1_of("某未知行业"), SECTOR_UNCLASSIFIED);
        assert_eq!(sector_l1_of(""), SECTOR_UNCLASSIFIED);
    }
}
