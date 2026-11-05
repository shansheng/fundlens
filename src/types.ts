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
  /** 当日实际收益（**仅累计官方净值已确认到今日的持仓**，见 actConfirmedCount） */
  actDayPnl: number;
  /** 当日估算收益率（比率口径，聚合层 = estDayPnl / totalMarketValue） */
  dayPnlPctEst: number;
  /** 当日实际收益率（比率口径，聚合层 = actDayPnl / 已确认部分昨收总市值） */
  dayPnlPctAct: number;
  /** 官方净值已确认到今日、实际计入 actDayPnl 的持仓数（净值发布陆续进行，用于提示「确认 N/M」） */
  actConfirmedCount: number;
  /** 参与「实际收益」统计的持仓总数（不含货基/理财、无代码兜底持仓） */
  actTotalCount: number;
  /** 进阶风险指标（年化收益/波动/最大回撤等）；无数据时为 null */
  risk: {
    cumulativeReturnPct: number;
    annualizedReturnPct: number;
    annualizedVolPct: number;
    maxDrawdownPct: number;
    days: number;
  } | null;
}
