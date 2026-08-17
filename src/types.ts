// 全局类型定义（同步 SPEC.md 第 5/6 节 Tauri Commands 与 DB 表）

export type DisclosureType = 'top10' | 'full';

export interface DisclosedHolding {
  stockCode: string;
  stockName: string;
  weight: number; // 占净值比例 0~1
  reportPeriod: string;
  disclosureType: DisclosureType;
}

export interface HoldingValuation {
  stockCode: string;
  stockName: string;
  weight: number;
  priceReturn: number; // 个股当日涨跌幅
  contribution: number;
}

export interface FundValuation {
  fundCode: string;
  fundName: string;
  officialNav: number;
  estNav: number;
  estChangePct: number;
  disclosureType: DisclosureType;
  disclosedWeightSum: number;
  estimated: boolean;
  reason?: string;
  holdings: HoldingValuation[];
}

export interface HoldingRow {
  fundCode: string;
  fundName: string;
  platform: string; // 平台 code
  platformName: string;
  shares: number;
  costAmount: number; // 累计成本
  avgCost: number; // 加权成本净值
  estNav: number;
  estChangePct: number;
  marketValue: number;
  dayPnl: number;
  totalPnl: number;
  totalPnlPct: number;
  estimated: boolean;
  basis?: DisclosureType;
}

export interface PortfolioSummary {
  totalMarketValue: number;
  totalCost: number;
  totalPnl: number;
  totalPnlPct: number;
  /** 当日估算收益（投影：实时/本地自算估值，交易时段随行情跳动） */
  estDayPnl: number;
  /** 当日实际收益（已确认：盘后=当日实际，休市=上一交易日实际；交易中未实现则为 0） */
  actDayPnl: number;
  /** 当日估算收益率（比率口径，聚合层 = estDayPnl / totalMarketValue） */
  dayPnlPctEst: number;
  /** 当日实际收益率（比率口径，聚合层 = actDayPnl / totalMarketValue） */
  dayPnlPctAct: number;
  /** 进阶风险指标（年化收益/波动/最大回撤等）；无数据时为 null */
  risk: {
    cumulativeReturnPct: number;
    annualizedReturnPct: number;
    annualizedVolPct: number;
    maxDrawdownPct: number;
    days: number;
  } | null;
}
