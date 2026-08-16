// FundLens 估值引擎 — 本地自算基金盘中估值（语言无关纯函数）
// 算法：est_nav = official_nav * (1 + Σ 持仓股占比_i * (现价_i / 昨收_i - 1))
// 未披露部分（现金/债券/非前十大）按占净值 (1 - Σ占比) 视作零波动近似。

export type DisclosureType = 'top10' | 'full';

export interface DisclosedHolding {
  stockCode: string;
  stockName: string;
  /** 占基金净值比例，0~1 */
  weight: number;
  reportPeriod: string; // 如 '2026Q1'
  disclosureType: DisclosureType;
}

export interface StockQuote {
  stockCode: string;
  price: number; // 现价
  prevClose: number; // 昨收
}

export interface ValuationInput {
  fundCode: string;
  officialNav: number; // 上一交易日官方单位净值
  holdings: DisclosedHolding[];
  quotes: Map<string, StockQuote>; // 以 stockCode 为键
  /** 基准指数行情（沪深300），用于近似未披露部分 */
  benchmark?: StockQuote;
}

export interface HoldingValuation {
  stockCode: string;
  stockName: string;
  weight: number;
  priceReturn: number; // 个股当日涨跌幅 (现价/昨收 - 1)
  contribution: number; // 对基金净值的贡献 = weight * priceReturn
}

export interface FundValuationResult {
  fundCode: string;
  officialNav: number;
  estNav: number; // 估算净值
  estChangePct: number; // 估算涨跌幅
  disclosureType: DisclosureType;
  disclosedWeightSum: number; // 已披露占比之和（用于可靠性提示）
  holdings: HoldingValuation[];
  estimated: boolean;
  reason?: string; // 无法估算时的原因
  benchmarkCode?: string | null; // 基准指数代码（如 "000300"）
  benchmarkName?: string | null; // 基准指数名称（如 "沪深300"）
  benchmarkReturn?: number; // 基准指数当日涨跌幅
  benchmarkWeight?: number; // 未披露部分占比（1 - 已披露权重和）
  platformEstChangePct?: number | null; // 平台实时估值涨跌幅（交叉验证）
  confidence?: 'high' | 'medium' | 'low' | 'none'; // 交叉验证置信度
  divergence?: number; // 两口径涨跌幅绝对差（百分点）
  penetrationEstChangePct?: number | null; // 本地持仓穿透自算涨跌幅（双源之一，始终带来源数值）
  consensusEstChangePct?: number | null; // 多源共识估值涨跌幅；无则 null
}

const EPS = 1e-9;

export function valueFund(input: ValuationInput): FundValuationResult {
  const { fundCode, officialNav, holdings, quotes } = input;

  if (holdings.length === 0) {
    return {
      fundCode,
      officialNav,
      estNav: officialNav,
      estChangePct: 0,
      disclosureType: 'top10',
      disclosedWeightSum: 0,
      holdings: [],
      estimated: false,
      reason: '暂无披露持仓数据，无法估算',
      benchmarkCode: null,
      benchmarkName: null,
      benchmarkReturn: 0,
      benchmarkWeight: 0,
      platformEstChangePct: null,
      confidence: 'none',
      divergence: 0,
      penetrationEstChangePct: null,
      consensusEstChangePct: null,
    };
  }

  if (officialNav <= 0) {
    return {
      fundCode,
      officialNav,
      estNav: officialNav,
      estChangePct: 0,
      disclosureType: holdings[0].disclosureType,
      disclosedWeightSum: 0,
      holdings: [],
      estimated: false,
      reason: '缺少官方净值基准，无法估算',
      benchmarkCode: null,
      benchmarkName: null,
      benchmarkReturn: 0,
      benchmarkWeight: 0,
      platformEstChangePct: null,
      confidence: 'none',
      divergence: 0,
      penetrationEstChangePct: null,
      consensusEstChangePct: null,
    };
  }

  const valuedHoldings: HoldingValuation[] = [];
  let portfolioReturn = 0; // 已披露部分加权收益
  let disclosedWeightSum = 0;

  for (const h of holdings) {
    const q = quotes.get(h.stockCode);
    if (!q || q.prevClose <= 0) {
      // 缺行情：该持仓贡献按 0 处理（保守）
      valuedHoldings.push({
        stockCode: h.stockCode,
        stockName: h.stockName,
        weight: h.weight,
        priceReturn: 0,
        contribution: 0,
      });
      disclosedWeightSum += h.weight;
      continue;
    }
    const priceReturn = q.price / q.prevClose - 1;
    const contribution = h.weight * priceReturn;
    portfolioReturn += contribution;
    disclosedWeightSum += h.weight;
    valuedHoldings.push({
      stockCode: h.stockCode,
      stockName: h.stockName,
      weight: h.weight,
      priceReturn,
      contribution,
    });
  }

  // 未披露部分 (1 - disclosedWeightSum) 用基准指数当日涨跌幅近似
  const benchmarkWeight = Math.max(0, 1 - disclosedWeightSum);
  let benchmarkReturn = 0;
  let benchmarkCode: string | null = null;
  let benchmarkName: string | null = null;
  if (input.benchmark && input.benchmark.prevClose > 0) {
    benchmarkReturn = input.benchmark.price / input.benchmark.prevClose - 1;
    benchmarkCode = input.benchmark.stockCode === '000300' ? '000300' : input.benchmark.stockCode;
    benchmarkName = '沪深300';
  }
  portfolioReturn += benchmarkWeight * benchmarkReturn;

  const estNav = officialNav * (1 + portfolioReturn);
  const estChangePct = estNav / officialNav - 1;

  void EPS;

  return {
    fundCode,
    officialNav,
    estNav,
    estChangePct,
    disclosureType: holdings[0].disclosureType,
    disclosedWeightSum,
    holdings: valuedHoldings,
    estimated: true,
    benchmarkCode,
    benchmarkName,
    benchmarkReturn,
    benchmarkWeight,
    platformEstChangePct: null,
    confidence: 'none',
    divergence: 0,
  };
}

// 组合聚合：给定每只基金持仓份额 + 估算净值，计算总市值/成本/盈亏
export interface PositionForSummary {
  fundCode: string;
  shares: number;
  avgCost: number; // 份额加权成本净值
  estNav: number;
  estimated: boolean;
  officialNav: number;
}

export interface PortfolioSummary {
  totalMarketValue: number;
  totalCost: number;
  totalPnl: number;
  totalPnlPct: number;
  estDayPnl: number; // 当日估算收益（投影）
  actDayPnl: number; // 当日实际收益（mock 中近似等于估算口径）
  positions: { fundCode: string; marketValue: number; pnl: number; pnlPct: number; dayPnl: number; estimated: boolean }[];
}

export function summarizePortfolio(positions: PositionForSummary[]): PortfolioSummary {
  let totalMarketValue = 0;
  let totalCost = 0;
  let dayPnl = 0;
  const out: PortfolioSummary['positions'] = [];

  for (const p of positions) {
    const marketValue = p.shares * (p.estimated ? p.estNav : p.officialNav);
    const cost = p.shares * p.avgCost;
    const pnl = marketValue - cost;
    const day = p.shares * (p.estNav - p.officialNav); // 基于昨收官方净值
    totalMarketValue += marketValue;
    totalCost += cost;
    if (p.estimated) dayPnl += day;
    out.push({
      fundCode: p.fundCode,
      marketValue,
      pnl,
      pnlPct: cost > 0 ? pnl / cost : 0,
      dayPnl: p.estimated ? day : 0,
      estimated: p.estimated,
    });
  }

  const totalPnl = totalMarketValue - totalCost;
  return {
    totalMarketValue,
    totalCost,
    totalPnl,
    totalPnlPct: totalCost > 0 ? totalPnl / totalCost : 0,
    estDayPnl: dayPnl,
    actDayPnl: dayPnl,
    positions: out,
  };
}
