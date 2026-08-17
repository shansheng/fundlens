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

/** 基金实际跟踪的指数行情（指数/ETF 类必备） */
export interface TrackedIndex {
  indexCode: string; // 如 '399997'
  indexName: string; // 如 '中证白酒'
  price: number; // 指数现价
  prevClose: number; // 指数昨收
}

export interface ValuationInput {
  fundCode: string;
  officialNav: number; // 上一交易日官方单位净值
  holdings: DisclosedHolding[];
  quotes: Map<string, StockQuote>; // 以 stockCode 为键
  /** 通用基准指数行情（多为沪深300），用于近似主动基金未披露部分 */
  benchmark?: StockQuote;
  /**
   * 基金实际跟踪指数行情（指数/ETF 类必备）。提供时，未披露部分（现金+其余成分）
   * 按该指数当日涨跌近似，而非通用沪深300 —— 即「指数型基金按跟踪的指数涨跌计算」。
   * 优先于 benchmark 使用。
   */
  trackedIndex?: TrackedIndex;
  /**
   * 是否「纯被动指数型基金」（排除指数增强）。为真且 trackedIndex 有效时，头条估值直接采用
   * 跟踪指数当日涨跌（指数实时估值优先），成分股穿透降为参考口径；否则（主动基金 / 指数增强 /
   * 拿不到跟踪指数行情）头条走通用本地穿透自算。
   * 注意：trackedIndex 对所有指数型基金（含指数增强）都会传入，使穿透口径的未披露部分按真实
   * 跟踪指数近似，不受 pureIndex 影响。
   */
  pureIndex?: boolean;
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
  /** 估值口径：index=指数实时估值优先（指数型基金）/ penetration=本地持仓穿透自算 / null=无 */
  valuationMethod?: 'index' | 'penetration' | null;
}

const EPS = 1e-9;

export function valueFund(input: ValuationInput): FundValuationResult {
  const { fundCode, officialNav, holdings, quotes } = input;

  // 纯被动指数基金可用「跟踪指数代理」给出估值（无需持仓数据）；
  // 仅当既无披露持仓、又无法用指数代理时，才提前返回「无法估算」。
  const hasIndexProxy =
    !!input.pureIndex && !!input.trackedIndex && input.trackedIndex.prevClose > 0;
  if (holdings.length === 0 && !hasIndexProxy) {
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
      valuationMethod: null,
    };
  }

  if (officialNav <= 0) {
    return {
      fundCode,
      officialNav,
      estNav: officialNav,
      estChangePct: 0,
      disclosureType: holdings[0]?.disclosureType ?? 'index',
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
      valuationMethod: null,
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

  // 未披露部分 (1 - disclosedWeightSum) 用指数当日涨跌近似。
  // 指数/ETF 基金优先用其「实际跟踪指数」(trackedIndex)，数学上远比通用沪深300贴近；
  // 主动基金或未提供 trackedIndex 时退回通用基准 benchmark（多为沪深300）。
  const benchmarkWeight = Math.max(0, 1 - disclosedWeightSum);
  let benchmarkReturn = 0;
  let benchmarkCode: string | null = null;
  let benchmarkName: string | null = null;
  const bench = input.trackedIndex
    ? {
        code: input.trackedIndex.indexCode,
        name: input.trackedIndex.indexName,
        price: input.trackedIndex.price,
        prevClose: input.trackedIndex.prevClose,
      }
    : input.benchmark && input.benchmark.prevClose > 0
      ? {
          code: input.benchmark.stockCode === '000300' ? '000300' : input.benchmark.stockCode,
          name: '沪深300',
          price: input.benchmark.price,
          prevClose: input.benchmark.prevClose,
        }
      : null;
  if (bench && bench.prevClose > 0) {
    benchmarkReturn = bench.price / bench.prevClose - 1;
    benchmarkCode = bench.code;
    benchmarkName = bench.name;
  }

  // 本地持仓穿透自算涨跌幅（参考口径）：披露部分穿透 + 未披露部分按基准指数近似。
  // 所有基金都计算，供指数基金作为「穿透参考」并列展示、供主动基金作为头条。
  const penetrationChangePct = portfolioReturn + benchmarkWeight * benchmarkReturn;

  // 纯被动指数型基金优先采用「跟踪指数实时估值」：头条直接用跟踪指数当日涨跌（跟踪误差极小，
  // 远比成分股穿透贴近）；成分股穿透降为参考口径填入 penetrationEstChangePct。
  // 指数增强 / 主动基金 / 拿不到跟踪指数行情：头条走通用本地穿透自算（未披露部分仍按跟踪指数近似）。
  let estChangePct: number;
  let valuationMethod: 'index' | 'penetration';
  if (input.pureIndex && input.trackedIndex && input.trackedIndex.prevClose > 0) {
    estChangePct = input.trackedIndex.price / input.trackedIndex.prevClose - 1;
    valuationMethod = 'index';
  } else {
    estChangePct = penetrationChangePct;
    valuationMethod = 'penetration';
  }

  const estNav = officialNav * (1 + estChangePct);

  void EPS;

  return {
    fundCode,
    officialNav,
    estNav,
    estChangePct,
    disclosureType: holdings[0]?.disclosureType ?? 'index',
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
    penetrationEstChangePct: penetrationChangePct,
    consensusEstChangePct: null,
    valuationMethod,
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

export interface PortfolioRisk {
  cumulativeReturnPct: number;
  annualizedReturnPct: number;
  annualizedVolPct: number;
  maxDrawdownPct: number;
  days: number;
}

export interface PositionSummary {
  fundCode: string;
  marketValue: number;
  pnl: number;
  pnlPct: number;
  dayPnl: number; // 头条当日收益（盘中=估算，盘后/休市=实际）
  dayPnlPct: number;
  dayPnlEst: number;
  dayPnlAct: number;
  dayPnlPctEst: number;
  dayPnlPctAct: number;
  weight: number; // 持仓占比
  estimated: boolean;
}

export interface PortfolioSummary {
  totalMarketValue: number;
  totalCost: number;
  totalPnl: number;
  totalPnlPct: number;
  estDayPnl: number; // 当日估算收益（投影）
  actDayPnl: number; // 当日实际收益
  dayPnlPctEst: number;
  dayPnlPctAct: number;
  positions: PositionSummary[];
  risk: PortfolioRisk | null;
}

export function summarizePortfolio(positions: PositionForSummary[], _phase: string = 'intraday'): PortfolioSummary {
  let totalMarketValue = 0;
  let totalCost = 0;
  let estDayPnl = 0;
  let actDayPnl = 0;
  const out: PositionSummary[] = [];

  for (const p of positions) {
    // 估算净值作为「上一交易日收盘净值」近似（mock 无 prev_nav 概念），与前端预览一致。
    const baselineNav = p.officialNav;
    const marketValue = p.shares * (p.estimated ? p.estNav : p.officialNav);
    const prevCloseMv = p.shares * baselineNav;
    const cost = p.shares * p.avgCost;
    const pnl = marketValue - cost;
    const dayEst = p.shares * (p.estNav - baselineNav);
    totalMarketValue += marketValue;
    totalCost += cost;
    if (p.estimated) {
      estDayPnl += dayEst;
      actDayPnl += dayEst;
    }
    out.push({
      fundCode: p.fundCode,
      marketValue,
      pnl,
      pnlPct: cost > 0 ? pnl / cost : 0,
      dayPnl: p.estimated ? dayEst : 0,
      dayPnlPct: prevCloseMv > 0 ? (p.estimated ? dayEst / prevCloseMv : 0) : 0,
      dayPnlEst: p.estimated ? dayEst : 0,
      dayPnlAct: p.estimated ? dayEst : 0,
      dayPnlPctEst: prevCloseMv > 0 ? (p.estimated ? dayEst / prevCloseMv : 0) : 0,
      dayPnlPctAct: prevCloseMv > 0 ? (p.estimated ? dayEst / prevCloseMv : 0) : 0,
      weight: 0, // 下方回填
      estimated: p.estimated,
    });
  }

  const weightDenom = totalMarketValue > 0 ? totalMarketValue : 1;
  for (const o of out) o.weight = o.marketValue / weightDenom;

  const totalPnl = totalMarketValue - totalCost;
  return {
    totalMarketValue,
    totalCost,
    totalPnl,
    totalPnlPct: totalCost > 0 ? totalPnl / totalCost : 0,
    estDayPnl,
    actDayPnl,
    dayPnlPctEst: totalMarketValue > 0 ? estDayPnl / totalMarketValue : 0,
    dayPnlPctAct: totalMarketValue > 0 ? actDayPnl / totalMarketValue : 0,
    positions: out,
    risk: null,
  };
}
