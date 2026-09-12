// FundLens 前端 ↔ 后端桥接层
// 在 Tauri 运行时调用真实命令；在浏览器（评审/开发预览）回退到本地 mock，
// 保证 UI 不依赖 Rust 后端即可可视化。mock 与真实命令保持相同的返回结构（见 SPEC.md 第 5/6 节）。

import { MOCK_FUNDS, isTradingNow, PLATFORMS, liveMockPrice } from './lib/mockData';
import {
  valueFund,
  summarizePortfolio,
  type FundValuationResult,
  type PortfolioSummary,
  type DisclosedHolding,
  type StockQuote,
} from './valuation/engine';
// 单一数据源：浏览器预览模式回退到 package.json 的 version（与 tauri.conf.json 保持一致）。
import pkg from '../package.json';

// 是否运行在 Tauri 环境中
export const isTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in (window as unknown as Record<string, unknown>);

// 是否 Tauri 移动端（Android/iOS WebView）：dialog 插件的 open/save 在移动端只返回
// content:// URI，std::fs 无法读写，文件链路必须走「<input type=file> 读字节 → base64 内容传参」。
// wry RustWebChromeClient 已实现 onShowFileChooser，故 HTML file input 在移动端可靠可用。
export const isMobile =
  isTauri &&
  typeof navigator !== 'undefined' &&
  /Android|iPhone|iPad|iPod/i.test(navigator.userAgent ?? '');

// 延迟加载 invoke，避免浏览器端打包/执行报错
async function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
  return tauriInvoke(cmd, args);
}

/**
 * 带超时的 invoke：网络/行情接口偶发长阻塞（超时/反爬）时前端及时恢复 UI，
 * 避免按钮无限转圈、用户误以为应用卡死只能杀进程。
 * 注意：超时仅中断前端等待；后端命令线程仍会继续执行完毕（各命令均为幂等设计，重复调用无害）。
 */
async function invokeWithTimeout(
  cmd: string,
  args: Record<string, unknown> | undefined,
  ms: number,
  label: string,
): Promise<unknown> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeoutPromise = new Promise<never>((_, reject) => {
    timer = setTimeout(
      () => reject(new Error(`${label}执行超时（${Math.round(ms / 1000)} 秒），后端仍在处理中，请稍候再试`)),
      ms,
    );
  });
  try {
    return await Promise.race([invoke(cmd, args), timeoutPromise]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export interface FundMeta {
  code: string;
  name: string;
  platform: string;
  platformName: string;
  shares: number;
  costAmount: number;
  avgCost: number;
  officialNav: number;
  /** 披露期：取自最新披露持仓记录；无披露时为 null */
  reportPeriod: string | null;
  disclosureType: 'top10' | 'full' | '';
  fundType?: string;
  fundTypeLabel?: string;
  /** 跟踪指数（指数/ETF 类），用于估值近似与展示；非指数基金为 null */
  trackedIndex?: { indexCode: string; indexName: string } | null;
  valuationApplicable?: boolean;
}

export interface PositionRow {
  fund: FundMeta;
  estNav: number;
  estChangePct: number;
  marketValue: number;
  dayPnl: number;
  dayPnlPct: number;
  dayPnlEst: number;
  dayPnlPctEst: number;
  /** 当日实际收益（金额）：份额 ×(官方净值 − 昨收基准)，永远为真实官方口径（今日净值已确认=今日实际；否则=上日实际）。
   *  仅当无法取到真实官方口径（货基/理财、QDII 延迟、官方净值/昨收基准缺失）时为 0，此时「当日」列回退为估算。 */
  dayPnlAct: number;
  /** 当日实际收益率（同口径比率） */
  dayPnlPctAct: number;
  /** 是否有真实官方口径的「当日/上日实际」可用（非货基/理财、有真实代码、官方净值与昨收基准均有效）。
   *  false 时当日实际收益无真实数据支撑，前端「当日」列回退为估算（标「估算」）——如 QDII T+1 / 官方净值接口被反爬 / 未刷新过。
   *  注意：不再要求 nav_date==今日，故开盘前/周末/休盘（展示最近交易日确认净值）也视为 true，仅控制标签「实际/上次」。 */
  hasDayActual: boolean;
  /** 当日官方净值是否真的取到（发布日期==今日）：true→当日列标「实际」，false→标「上次」（开盘前/周末/休盘展示上一次净值） */
  dayIsToday: boolean;
  /** 官方净值日期（YYYY-MM-DD；空串=未取到），供「上次」标签显示具体净值日（透明化） */
  navDate: string;
  /** 上一交易日估算收益（position_daily 最后一条的 day_pnl_est）；非交易日用于「当日估算」列回填。null=历史尚未回填。 */
  lastDayPnlEst?: number | null;
  /** 上一交易日实际收益（position_daily 最后一条的 day_pnl_act） */
  lastDayPnlAct?: number | null;
  /** 上一交易日官方净值日期（position_daily 最后一条的 nav_date）；用于「上一交易日 MM-DD」标签 */
  lastNavDate?: string | null;
  totalPnl: number;
  totalPnlPct: number;
  estimated: boolean;
  disclosureType: 'top10' | 'full';
  disclosedWeightSum: number;
  /** 估值来源：realtime=盘中实时估值(平台) / local=本地自算 / none=无 */
  valuationSource?: 'realtime' | 'local' | 'none';
  /** 交叉验证置信度：high/medium/low/none */
  confidence?: 'high' | 'medium' | 'low' | 'none';
  /** 本地持仓穿透自算涨跌幅（双源之一，始终带来源数值） */
  penetrationEstChangePct?: number | null;
  /** 多源共识估值涨跌幅；无则 null */
  consensusEstChangePct?: number | null;
  /** 估值口径：index=指数实时估值优先（指数型基金）/ penetration=本地持仓穿透自算 / null=无 */
  valuationMethod?: 'index' | 'penetration' | null;
  /** QDII 延迟结算提示：T+1·海外交易中 / T+1·海外净值；非 QDII 为 null */
  delayNote?: string | null;
}

export interface OverviewResult {
  summary: PortfolioSummary & { lastNavDate?: string | null };
  positions: PositionRow[];
  trading: boolean;
  /** 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / closed=休市(上一交易日实际) */
  marketSession: 'intraday' | 'post_close' | 'closed';
  asOf: string;
}

export interface FundPosition {
  /** 当前份额 */
  shares: number;
  /** 单位成本 */
  avgCost: number;
  /** 持仓成本（累计投入成本基数） */
  costAmount: number;
  /** 市值（交易中=估算口径市值 / 其余=官方净值口径市值） */
  marketValue: number;
  /** 累计盈亏 = 市值 − 持仓成本 */
  totalPnl: number;
  /** 累计收益率 = 累计盈亏 / 持仓成本 */
  totalPnlPct: number;
  /** 当日收益（头条口径：交易中=估算，否则=实际） */
  dayPnl: number;
  /** 当日收益率（头条口径） */
  dayPnlPct: number;
  /** 当日估算收益（盘中浮动估算，随行情跳动） */
  dayPnlEst: number;
  /** 当日估算收益率 */
  dayPnlPctEst: number;
  /** 当日官方净值是否真的取到（发布日期==今日）：true→「当日收益」标「实际」，false→标「上日实际」 */
  dayIsToday: boolean;
  /** 上一交易日估算收益（position_daily 最后一条的 day_pnl_est）；非交易日用于「当日估算收益」回填。null=历史尚未回填。 */
  lastDayPnlEst?: number | null;
  /** 上一交易日实际收益（position_daily 最后一条的 day_pnl_act） */
  lastDayPnlAct?: number | null;
  /** 上一交易日官方净值日期（position_daily 最后一条的 nav_date）；用于「上一交易日 MM-DD」标签 */
  lastNavDate?: string | null;
  /** 是否纳入浮动净值估算（货基/理财=false，仅展示累计持有收益） */
  estimated: boolean;
}

export interface FundDetailResult {
  fund: FundMeta;
  valuation: FundValuationResult;
  quotes: { stockCode: string; stockName: string; price: number; prevClose: number; priceReturn: number }[];
  /** 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / closed=休市(上一交易日实际) */
  marketSession: 'intraday' | 'post_close' | 'closed';
  /** 估值来源：local=本地穿透自算 / none=无估值（平台实时估值接口已停用） */
  valuationSource?: 'realtime' | 'local' | 'none';
  /** QDII 延迟结算提示：T+1·海外交易中 / T+1·海外净值；非 QDII 为 null */
  delayNote?: string | null;
  /** 该基金的交易流水（买卖/分红/手动），按日期倒序 */
  transactions: TransactionOut[];
  /** 该基金「我的持仓」业界标准指标（市值/成本/累计盈亏/当日收益等） */
  position: FundPosition;
}

export interface AssetSlice {
  category: string; // equity / fixed / money / qdii / other
  label: string; // 权益类 / 固收类 / 货币类 / QDII / 其他
  marketValue: number;
  pct: number; // 0~1
}

export interface StatsResult {
  summary: PortfolioSummary;
  best: PositionRow | null;
  worst: PositionRow | null;
  byPlatform: { platform: string; platformName: string; marketValue: number; totalPnl: number }[];
  estimatedCoverage: number; // 可估算持仓占比 0~1
  assetAllocation: AssetSlice[]; // 资产配置全景（按 fund_type 归并大类）
}

export interface ImportPreview {
  platform: string;
  platformName: string;
  detectedCount: number;
  funds: {
    code: string;
    name: string;
    shares: number;
    nav: number;
    holdingAmount: number;
    holdingProfit: number;
    yesterdayProfit: number;
    profitRate: number;
  }[];
  ocrReady: boolean;
  note: string;
  rawLines: string[];
}

// ===================== 交易流水 / 报表（单机单账户，平台维度在前端筛选） =====================

export type TxnType = 'buy' | 'sell' | 'dividend' | 'reinvest_dividend' | 'deposit' | 'withdraw' | 'adjust';

export interface TransactionOut {
  id: number;
  accountId: number; // 内部固定为 1（单机单账户），前端不直接暴露账户概念
  txnType: TxnType;
  fundCode: string | null;
  fundName: string | null;
  shares: number | null;
  amount: number;
  price: number | null;
  txnDate: string;
  txnTime?: string;
  note: string | null;
  source: string;
  sourceRef: string | null;
}

/// 导入交易记录项（买/卖/分红）
export interface ImportTxn {
  fundCode: string;
  fundName?: string | null;
  txnType: TxnType;
  shares?: number | null;
  amount: number;
  price?: number | null;
  txnDate: string;
  txnTime?: string;
  note?: string | null;
}

/// 单条交易记录 OCR 预览项（可编辑）
export interface ImportTxnOut {
  txnType: string;       // 归一化类型：buy/sell/dividend
  txnTypeRaw: string;    // 原始类型标签（如「买入」「赎回」）
  date: string;          // ISO 日期 YYYY-MM-DD
  hasYear: boolean;      // 日期是否含年份（false 需提醒核对）
  time?: string;         // 交易时间 HH:MM（截图含时间时返回）
  code: string;          // 基金代码（6 位）
  name: string;          // 基金名称
  shares: number;        // 份额
  amount: number;        // 成交金额
  price: number;         // 单位净值/价格
  confidence: number;    // 置信度 0~1
}

/// 交易记录截图 OCR 预览（可编辑后落地）
export interface ImportTxnPreview {
  platform: string;
  platformName: string;
  detectedCount: number;
  txns: ImportTxnOut[];
  ocrReady: boolean;
  note: string;
  rawLines: string[];
}

export interface SnapshotPoint {
  date: string;
  totalMarketValue: number;
  totalCost: number;
  totalPnl: number;
  dayPnl: number;
  /** 当日估算收益（快照日盘中估算投影；历史快照缺省为 0） */
  dayPnlEst: number;
  /** 当日估算市值（按估算净值口径；历史快照缺省为 0） */
  estMarketValue: number;
}

export interface MoverOut {
  code: string;
  name: string;
  totalPnl: number;
  totalPnlPct: number;
}

export interface PeriodReport {
  /** 报告周期：daily / weekly / monthly / yearly（四种报告共用同一结构，可直接对比） */
  period: string;
  scope: string;
  startDate: string | null;
  endDate: string | null;
  startMv: number;
  endMv: number;
  deltaMv: number;
  deltaPnl: number;
  pnlRate: number;
  /** 区间估算收益累计（Σ 快照日当日估算收益；估算统计自启用起累积，旧数据为 0） */
  estDeltaPnl: number;
  /** 估算 − 实际偏差（estDeltaPnl − deltaPnl；>0 表示估算整体高估） */
  estActDiff: number;
  /** 区间估算收益率（estDeltaPnl / 期初成本） */
  estPnlRate: number;
  /** 偏差率（estActDiff / 期初成本） */
  diffRate: number;
  positiveDays: number;
  negativeDays: number;
  /** 估算口径盈利天数（series 中 dayPnlEst > 0 的天数） */
  estPositiveDays: number;
  /** 估算口径亏损天数（series 中 dayPnlEst < 0 的天数） */
  estNegativeDays: number;
  series: SnapshotPoint[];
  best: MoverOut | null;
  worst: MoverOut | null;
  hasHistory: boolean;
}

// ===================== 区间操作收益 =====================

export interface OperationPnlRow {
  fundCode: string;
  fundName: string;
  /** 归并方向：buy=买入、sell=卖出 */
  side: string;
  /** 买入收益（区间内买入份额的涨跌，涨=正） */
  buyPnl: number;
  /** 卖出收益（区间内卖出份额的涨跌，涨=负、跌=正） */
  sellPnl: number;
  /** 区间末基准净值 */
  endNav: number;
  /** 是否有净值数据支撑 */
  hasNav: boolean;
}

export interface OperationPnl {
  startDate: string;
  endDate: string;
  /** 实际采用的区间末基准净值日（≤ end 的最近有净值交易日） */
  endNavDate: string | null;
  totalBuyPnl: number;
  totalSellPnl: number;
  totalPnl: number;
  rows: OperationPnlRow[];
}

// ===================== 净值走势 / 成本走势 =====================

export interface NavPoint {
  date: string; // YYYY-MM-DD
  nav: number; // 单位净值
  accNav: number; // 累计净值
}

export interface CostPoint {
  date: string;
  cumulativeCost: number; // 累计成本
  unitCost: number; // 单位成本
  shares: number;
}

export interface TxnMarker {
  date: string;
  txnType: TxnType; // buy / sell / dividend / reinvest_dividend
  shares: number;
  amount: number;
}

export interface FundSeries {
  navPoints: NavPoint[];
  costPoints: CostPoint[];
  txnMarkers: TxnMarker[];
  range: string; // '1m' | '3m' | '6m' | 'all'
}

function mockFundToMeta(f: (typeof MOCK_FUNDS)[number]): FundMeta {
  return {
    code: f.code,
    name: f.name,
    platform: f.platform,
    platformName: PLATFORMS[f.platform].name,
    shares: f.shares,
    costAmount: f.costAmount,
    avgCost: f.costAmount / f.shares,
    officialNav: f.officialNav,
    reportPeriod: f.reportPeriod,
    disclosureType: f.disclosureType,
    fundType: f.fundType,
    fundTypeLabel: f.fundTypeLabel,
    trackedIndex: f.trackedIndex
      ? { indexCode: f.trackedIndex.indexCode, indexName: f.trackedIndex.indexName }
      : null,
    valuationApplicable: true,
  };
}

function runMockValuation(f: (typeof MOCK_FUNDS)[number]) {
  const holdings: DisclosedHolding[] = f.holdings.map((h) => ({
    stockCode: h.stockCode,
    stockName: h.stockName,
    weight: h.weight,
    reportPeriod: f.reportPeriod,
    disclosureType: f.disclosureType,
  }));
  const quotes = new Map<string, StockQuote>();
  for (const q of f.quotes) {
    const price = liveMockPrice(q.price, q.prevClose, q.stockCode);
    quotes.set(q.stockCode, { stockCode: q.stockCode, price, prevClose: q.prevClose });
  }
  // 跟踪指数现价同样随时间摆动（指数基金未披露部分按此近似）
  const trackedIndex = f.trackedIndex
    ? {
        indexCode: f.trackedIndex.indexCode,
        indexName: f.trackedIndex.indexName,
        price: liveMockPrice(f.trackedIndex.price, f.trackedIndex.prevClose, f.trackedIndex.indexCode),
        prevClose: f.trackedIndex.prevClose,
      }
    : undefined;
  // 被动指数型基金判定（与后端 data::is_pure_index_fund 对齐）：类型码 006/008/009，或名称含 指数/ETF/联接，
  // **不含** 指数增强（指数增强走穿透口径，以贴合其跟踪误差）。
  // 纯被动指数头条估值优先采用跟踪指数当日涨跌（指数实时估值优先）；trackedIndex 始终传入，
  // 使穿透口径的未披露部分按真实跟踪指数近似。
  const pureIndex =
    (!!f.trackedIndex ||
      ['006', '008', '009'].includes(f.fundType) ||
      /指数|ETF|联接/i.test(f.name)) &&
    !/指数增强/i.test(f.name);
  const valuation = valueFund({
    fundCode: f.code,
    officialNav: f.officialNav,
    holdings,
    quotes,
    trackedIndex,
    pureIndex,
  });
  return { holdings, quotes, valuation };
}

// 浏览器预览用的三态时段判定，与后端 market_phase() 对齐：
// intraday=连续竞价时段(9:30-11:30, 13:00-15:00) / post_close=交易日非竞价时段(盘前/午休/盘后)
// / closed=周末或节假日。原来只用 isTradingNow() 把午休也压成 closed，导致浏览器预览时中午
// 「当日收益」整块消失；这里改为三态，与真机一致。
function mockMarketSession(): 'intraday' | 'post_close' | 'closed' {
  const now = new Date();
  const day = now.getDay();
  if (day === 0 || day === 6) return 'closed';
  const hm = now.getHours() * 60 + now.getMinutes();
  const open1 = 9 * 60 + 30;
  const close1 = 11 * 60 + 30;
  const open2 = 13 * 60;
  const close2 = 15 * 60;
  if ((hm >= open1 && hm < close1) || (hm >= open2 && hm < close2)) return 'intraday';
  return 'post_close';
}

// 浏览器预览：构造一个「上一交易日」日期（昨天），用于演示休市时回填上一交易日估算/实际。
function yesterdayStr(): string {
  const d = new Date();
  d.setDate(d.getDate() - 1);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${day}`;
}

async function mockOverview(): Promise<OverviewResult> {
  // ⚠️ 浏览器 Mock：下方指标是按「baseline = officialNav（昨收基准）」简化的演示口径，
  // 近似 Rust compute_position_metrics/summarize_portfolio（valuation.rs），但不含 prev_nav 三级
  // 优先级、nav_date==today 市值口径、day_pnl_act 真实值等；仅供无 Tauri 后端的预览，勿作口径回归。
  const positions: PositionRow[] = [];
  const summaryInput: Parameters<typeof summarizePortfolio>[0] = [];
  for (const f of MOCK_FUNDS) {
    const { valuation } = runMockValuation(f);
    const meta = mockFundToMeta(f);
    summaryInput.push({
      fundCode: f.code,
      shares: f.shares,
      avgCost: meta.avgCost,
      estNav: valuation.estNav,
      estimated: valuation.estimated,
      officialNav: f.officialNav,
    });
    const marketValue = f.shares * (valuation.estimated ? valuation.estNav : f.officialNav);
    const cost = f.shares * meta.avgCost;
    const prevCloseMv = f.shares * f.officialNav;
    const dayPnlEst = valuation.estimated ? f.shares * (valuation.estNav - f.officialNav) : 0;
    const dayPnlPctEst = prevCloseMv > 0 ? dayPnlEst / prevCloseMv : 0;
    positions.push({
      fund: meta,
      estNav: valuation.estNav,
      estChangePct: valuation.estChangePct,
      marketValue,
      dayPnl: dayPnlEst,
      dayPnlPct: dayPnlPctEst,
      dayPnlEst,
      dayPnlPctEst,
      dayPnlAct: 0,
      dayPnlPctAct: 0,
      hasDayActual: false,
      dayIsToday: false,
      navDate: '',
      // 浏览器预览：mock 无 prev_nav，无法算真实官方「当日/上日实际」，但为演示休市回填，
      // 用「当日估算」近似充当上一交易日估算（仅可估算基金有值；货基/理财留空→显示 —）。
      lastDayPnlEst: valuation.estimated ? dayPnlEst : undefined,
      lastDayPnlAct: undefined,
      lastNavDate: yesterdayStr(),
      totalPnl: marketValue - cost,
      totalPnlPct: cost > 0 ? (marketValue - cost) / cost : 0,
      estimated: valuation.estimated,
      disclosureType: f.disclosureType,
      disclosedWeightSum: valuation.disclosedWeightSum,
      confidence: valuation.confidence,
      penetrationEstChangePct: valuation.penetrationEstChangePct ?? null,
      consensusEstChangePct: valuation.consensusEstChangePct ?? null,
      valuationMethod: valuation.valuationMethod ?? null,
      delayNote: null,
      // 浏览器预览：mock 走本地自算估值（非平台实时），来源标记为 local。
      valuationSource: valuation.estimated ? 'local' : 'none',
    });
  }
  const summary = summarizePortfolio(summaryInput, mockMarketSession());
  positions.sort((a, b) => b.marketValue - a.marketValue);
  const marketSession: OverviewResult['marketSession'] = mockMarketSession();
  // 组合级 lastNavDate：mock 用「昨日」演示休市时头条角标；真机由后端下发 position_daily 末条日期。
  const summaryWithNav: OverviewResult['summary'] = { ...summary, lastNavDate: yesterdayStr() };
  return { summary: summaryWithNav, positions, trading: isTradingNow(), marketSession, asOf: new Date().toLocaleString('zh-CN') };
}

async function mockFundDetail(code: string): Promise<FundDetailResult> {
  const f = MOCK_FUNDS.find((x) => x.code === code);
  if (!f) throw new Error(`未找到基金 ${code}`);
  const { quotes, valuation } = runMockValuation(f);
  const meta = mockFundToMeta(f);
  const quoteView = f.quotes.map((q) => {
    const live = quotes.get(q.stockCode)!;
    return {
      stockCode: q.stockCode,
      stockName: q.stockName,
      price: live.price,
      prevClose: q.prevClose,
      priceReturn: q.prevClose > 0 ? live.price / q.prevClose - 1 : 0,
    };
  });
  // 与后端 get_fund_detail 同一套口径：三态时段 + compute_position_metrics 等价实现。
  const phase: FundDetailResult['marketSession'] = mockMarketSession();
  const estimable = !['002', '005'].includes(f.fundType);
  const shares = f.shares;
  const cost = shares * meta.avgCost;
  const refNav = phase === 'intraday' && valuation.estimated ? valuation.estNav : f.officialNav;
  const marketValue = shares * refNav;
  const prevCloseMv = shares * f.officialNav;
  const dayPnlEst = valuation.estimated ? shares * (valuation.estNav - f.officialNav) : 0;
  const dayPnlPctEst = prevCloseMv > 0 ? dayPnlEst / prevCloseMv : 0;
  // mock 预览无 prev_nav（昨收基准），无法计算真实官方「当日/上日实际」，故「当日收益」置 0；
  // 盘中实时浮动估算只在「当日估算收益」(dayPnlEst) 展示，与真实后端口径一致。
  const dayPnl = 0;
  const dayPnlPct = 0;
  const position: FundPosition = {
    shares,
    avgCost: meta.avgCost,
    costAmount: cost,
    marketValue,
    totalPnl: marketValue - cost,
    totalPnlPct: cost > 0 ? (marketValue - cost) / cost : 0,
    dayPnl,
    dayPnlPct,
    dayPnlEst,
    dayPnlPctEst,
    dayIsToday: false,
    // 浏览器预览：mock 无 prev_nav，用「当日估算」近似充当上一交易日估算（仅可估算基金有值）。
    lastDayPnlEst: valuation.estimated ? dayPnlEst : undefined,
    lastDayPnlAct: undefined,
    lastNavDate: yesterdayStr(),
    estimated: valuation.estimated && estimable,
  };
  return {
    fund: meta,
    valuation,
    quotes: quoteView,
    marketSession: phase,
    delayNote: null,
    transactions: [],
    valuationSource: valuation.estimated ? 'local' : 'none',
    position,
  };
}

async function mockStats(): Promise<StatsResult> {
  const overview = await mockOverview();
  const byPlatformMap = new Map<string, { platform: string; platformName: string; marketValue: number; totalPnl: number }>();
  const assetMap = new Map<string, { label: string; marketValue: number }>();
  let totalMv = 0;
  let estCount = 0;
  for (const p of overview.positions) {
    if (p.estimated) estCount += 1;
    const cur = byPlatformMap.get(p.fund.platform) ?? {
      platform: p.fund.platform,
      platformName: p.fund.platformName,
      marketValue: 0,
      totalPnl: 0,
    };
    cur.marketValue += p.marketValue;
    cur.totalPnl += p.totalPnl;
    byPlatformMap.set(p.fund.platform, cur);
    const cat = assetCategory(p.fund.fundType ?? '');
    const entry = assetMap.get(cat) ?? { label: assetCategoryLabel(cat), marketValue: 0 };
    entry.marketValue += p.marketValue;
    assetMap.set(cat, entry);
    totalMv += p.marketValue;
  }
  const assetAllocation: AssetSlice[] = [...assetMap.entries()]
    .map(([category, v]) => ({
      category,
      label: v.label,
      marketValue: v.marketValue,
      pct: totalMv > 0 ? v.marketValue / totalMv : 0,
    }))
    .sort((a, b) => b.marketValue - a.marketValue);
  const sorted = [...overview.positions].sort((a, b) => b.totalPnlPct - a.totalPnlPct);
  return {
    summary: overview.summary,
    best: sorted[0] ?? null,
    worst: sorted[sorted.length - 1] ?? null,
    byPlatform: [...byPlatformMap.values()],
    estimatedCoverage: overview.positions.length > 0 ? estCount / overview.positions.length : 0,
    assetAllocation,
  };
}

/** 资产大类映射（与后端 data::asset_category 保持一致，供浏览器预览 mock 使用） */
function assetCategory(fundType: string): string {
  if (['001', '007', '008', '009', '006'].includes(fundType)) return 'equity';
  if (['004', '005'].includes(fundType)) return 'fixed';
  if (fundType === '002') return 'money';
  if (fundType === '003') return 'qdii';
  return 'other';
}
function assetCategoryLabel(cat: string): string {
  return { equity: '权益类', fixed: '固收类', money: '货币类', qdii: 'QDII', other: '其他' }[cat] ?? '其他';
}

async function mockImport(platform: string): Promise<ImportPreview> {
  const pm = PLATFORMS[platform];
  const funds = MOCK_FUNDS.filter((f) => f.platform === platform).map((f) => ({
    code: f.code,
    name: f.name,
    shares: f.shares,
    nav: f.officialNav,
    holdingAmount: f.costAmount,
    holdingProfit: f.costAmount - f.shares * f.officialNav,
    yesterdayProfit: 0,
    profitRate: 0,
  }));
  return {
    platform,
    platformName: pm ? pm.name : platform,
    detectedCount: funds.length,
    funds,
    ocrReady: true,
    note: '（演示）已识别截图中的持仓条目；真实环境下由本地 OCR + 平台规则模板解析。',
    rawLines: [],
  };
}

// ============ 报表 浏览器预览 mock ============

function mockTransactions(): TransactionOut[] {
  return [
    { id: 1, accountId: 1, txnType: 'buy', fundCode: '003095', fundName: '中欧医疗健康混合', shares: 1000, amount: 4196, price: 4.196, txnDate: '2026-01-05', note: '建仓', source: 'manual_set', sourceRef: null },
    { id: 2, accountId: 1, txnType: 'deposit', fundCode: null, fundName: null, shares: null, amount: 10000, price: null, txnDate: '2026-02-01', note: '入金', source: 'manual_txn', sourceRef: null },
    { id: 3, accountId: 1, txnType: 'sell', fundCode: '003095', fundName: '中欧医疗健康混合', shares: 200, amount: 1000, price: 5.0, txnDate: '2026-03-10', note: '减仓', source: 'manual_txn', sourceRef: null },
    { id: 4, accountId: 1, txnType: 'dividend', fundCode: '003095', fundName: '中欧医疗健康混合', shares: null, amount: 120, price: null, txnDate: '2026-04-12', note: '现金分红', source: 'import_txn', sourceRef: 'demo-batch' },
  ];
}

function mockReport(_kind: '日' | '周' | '月' | '年'): PeriodReport {
  const today = new Date();
  const series: SnapshotPoint[] = [];
  let mv = 50000;
  for (let i = 30; i >= 0; i -= 1) {
    const d = new Date(today.getTime() - i * 86400000);
    const dayPnl = Math.round(Math.sin(i / 3) * 400);
    const dayPnlEst = Math.round(Math.sin(i / 3) * 400 * 0.96);
    mv += dayPnl;
    series.push({
      date: d.toISOString().slice(0, 10),
      totalMarketValue: mv,
      totalCost: 48000,
      totalPnl: mv - 48000,
      dayPnl,
      dayPnlEst,
      estMarketValue: mv - dayPnl + dayPnlEst,
    });
  }
  const end = series[series.length - 1];
  const start = series[0];
  const deltaPnl = end.totalPnl - start.totalPnl;
  const estDeltaPnl = series.reduce((acc, s) => acc + s.dayPnlEst, 0);
  return {
    period: 'weekly',
    scope: '全部账户',
    startDate: start.date,
    endDate: end.date,
    startMv: start.totalMarketValue,
    endMv: end.totalMarketValue,
    deltaMv: end.totalMarketValue - start.totalMarketValue,
    deltaPnl,
    pnlRate: deltaPnl / start.totalCost,
    estDeltaPnl,
    estActDiff: estDeltaPnl - deltaPnl,
    estPnlRate: estDeltaPnl / start.totalCost,
    diffRate: (estDeltaPnl - deltaPnl) / start.totalCost,
    positiveDays: series.filter((s) => s.dayPnl > 0).length,
    negativeDays: series.filter((s) => s.dayPnl < 0).length,
    estPositiveDays: series.filter((s) => s.dayPnlEst > 0).length,
    estNegativeDays: series.filter((s) => s.dayPnlEst < 0).length,
    series,
    best: { code: '003095', name: '中欧医疗健康混合', totalPnl: 3200, totalPnlPct: 0.18 },
    worst: { code: '161725', name: '招商中证白酒', totalPnl: -800, totalPnlPct: -0.05 },
    hasHistory: true,
  };
}

function mockCalendar(): SnapshotPoint[] {
  const today = new Date();
  const out: SnapshotPoint[] = [];
  let mv = 50000;
  for (let i = 90; i >= 0; i -= 1) {
    const d = new Date(today.getTime() - i * 86400000);
    const dayPnl = Math.round(Math.sin(i / 4) * 350);
    mv += dayPnl;
    out.push({
      date: d.toISOString().slice(0, 10),
      totalMarketValue: mv,
      totalCost: 48000,
      totalPnl: mv - 48000,
      dayPnl,
      dayPnlEst: Math.round(dayPnl * 0.96),
      estMarketValue: mv - dayPnl + Math.round(dayPnl * 0.96),
    });
  }
  return out;
}

function mockOperationPnl(startDate: string, endDate: string): OperationPnl {
  return {
    startDate,
    endDate,
    endNavDate: endDate,
    totalBuyPnl: 320.5,
    totalSellPnl: -150.2,
    totalPnl: 170.3,
    rows: [
      { fundCode: '003095', fundName: '中欧医疗健康混合', side: 'buy', buyPnl: 220.4, sellPnl: 0, endNav: 2.31, hasNav: true },
      { fundCode: '161725', fundName: '招商中证白酒', side: 'sell', buyPnl: 0, sellPnl: -150.2, endNav: 1.08, hasNav: true },
      { fundCode: '001551', fundName: '某指数基金', side: 'buy', buyPnl: 100.1, sellPnl: 0, endNav: 1.42, hasNav: true },
    ],
  };
}

// ---- 净值走势 / 成本走势 浏览器预览 mock ----

function rangeCutoff(range: string): string | null {
  const months: Record<string, number> = { '1m': 1, '3m': 3, '6m': 6 };
  if (!(range in months)) return null;
  const d = new Date();
  d.setDate(d.getDate() - months[range] * 30);
  return d.toISOString().slice(0, 10);
}

/// 生成约 180 个交易日的合成历史净值（带轻微随机游走，结尾贴近官方净值），供浏览器预览。
function mockNavHistory(_code: string): NavPoint[] {
  const today = new Date();
  const out: NavPoint[] = [];
  let nav = 4.0;
  for (let i = 180; i >= 0; i -= 1) {
    const d = new Date(today.getTime() - i * 86400000);
    const wd = d.getDay();
    if (wd === 0 || wd === 6) continue; // 跳过周末
    const r = (Math.sin(i / 5) + Math.cos(i / 13)) * 0.02;
    nav = Math.max(0.5, nav * (1 + r * 0.05));
    out.push({ date: d.toISOString().slice(0, 10), nav: +nav.toFixed(4), accNav: +(nav * 1.05).toFixed(4) });
  }
  return out;
}

/// 从 mock 流水回放平均成本法，产出成本序列与交易标记（与后端 get_cost_series 口径一致）。
function mockTxnSeries(code: string): { cost: CostPoint[]; markers: TxnMarker[] } {
  const txns = mockTransactions().filter(
    (t) => t.fundCode === code && t.txnDate !== '1970-01-01' && ['buy', 'sell', 'dividend'].includes(t.txnType),
  );
  let shares = 0;
  let basis = 0;
  const cost: CostPoint[] = [];
  const markers: TxnMarker[] = [];
  for (const t of txns) {
    if (t.txnType === 'buy') {
      if (t.shares && t.shares > 0) {
        shares += t.shares;
        basis += t.amount;
      } else {
        basis = t.amount;
      }
    } else if (t.txnType === 'sell') {
      if (t.shares && t.shares > 0 && shares > 0) {
        const sellBasis = t.shares * (shares > 0 ? basis / shares : 0);
        basis -= sellBasis;
        shares -= t.shares;
        if (shares <= 1e-9) {
          shares = 0;
          basis = 0;
        }
      }
    } else if (t.txnType === 'dividend') {
      if (shares > 0) basis -= t.amount;
    }
    cost.push({ date: t.txnDate, cumulativeCost: basis, unitCost: shares > 0 ? basis / shares : 0, shares });
    markers.push({ date: t.txnDate, txnType: t.txnType, shares: t.shares ?? 0, amount: t.amount });
  }
  return { cost, markers };
}

function mockFundSeries(code: string, range: string): FundSeries {
  const nav = mockNavHistory(code);
  const cutoff = rangeCutoff(range);
  const navPoints = cutoff ? nav.filter((p) => p.date >= cutoff) : nav;
  const { cost, markers } = mockTxnSeries(code);
  return { navPoints, costPoints: cost, txnMarkers: markers, range };
}

/**
 * 读取应用版本号：Tauri 运行时用 getVersion()（来自 tauri.conf.json 的 package.version），
 * 浏览器预览模式回退到 package.json 的 version，二者同源、不写死。
 */
export async function getAppVersion(): Promise<string> {
  if (isTauri) {
    try {
      const { getVersion } = await import('@tauri-apps/api/app');
      return await getVersion();
    } catch {
      // 极少数情况 getVersion 失败（如插件异常），回退到静态值
    }
  }
  return (pkg as { version: string }).version;
}


// ============ 基金穿透（Look-through）：只读分析层 ============

/** L1/L2 行业切片（分母一致=组合总市值；isVirtual=未穿透/境外/未分类虚拟桶） */
export interface IndustrySlice {
  key: string;
  marketValue: number;
  pct: number;
  dayContribution: number | null;
  isVirtual: boolean;
  /** L2 → 所属 L1 大类；L1 行为 null */
  parent: string | null;
}

export interface StockFundWeight {
  fundCode: string;
  fundName: string;
  weight: number;
  contributedMv: number;
}

export interface StockRow {
  stockCode: string;
  stockName: string;
  sectorL1: string;
  industryL2: string;
  marketValue: number;
  pct: number;
  fundCount: number;
  funds: StockFundWeight[];
  dayChangePct: number | null;
  dayContribution: number | null;
  /** 隐性重仓预警：同一股票经 ≥3 只基金持有且合计穿透占比 >5% */
  hiddenWarning: boolean;
}

export interface FundInfoRow {
  code: string;
  name: string;
  marketValue: number;
  coverage: number;
  reportPeriod: string | null;
  unpenetratedMv: number;
  /** 穿透口径：index_constituent=纯被动指数基金按跟踪指数成分穿透；disclosure_top10=披露前十大口径（含货基/无披露） */
  penetrationSource: 'disclosure_top10' | 'index_constituent';
}

export interface LookthroughResult {
  totalMv: number;
  coverage: number;
  reportPeriods: string[];
  industriesL1: IndustrySlice[];
  industriesL2: IndustrySlice[];
  stocks: StockRow[];
  cr5: number;
  cr10: number;
  funds: FundInfoRow[];
  unpenetratedMv: number;
  hasQuotes: boolean;
  asOf: string;
}

/** 组合穿透主查询：交易时段带当日行业贡献（复用既有估算行情链路，不新增出站压力） */
export async function lookthroughOverview(platform: string | null = null): Promise<LookthroughResult> {
  if (!isTauri) return mockLookthrough();
  return (await invokeWithTimeout('lookthrough_overview', { platform: platform ?? null }, 45000, '基金穿透')) as LookthroughResult;
}

/** 浏览器预览模式回退：静态示例数据（口径与真实命令一致，纯展示用） */
function mockLookthrough(): LookthroughResult {
  const mk = (key: string, mv: number, pct: number, contrib: number | null, isVirtual = false, parent: string | null = null): IndustrySlice =>
    ({ key, marketValue: mv, pct, dayContribution: contrib, isVirtual, parent });
  return {
    totalMv: 337000,
    coverage: 0.42,
    reportPeriods: ['2026Q2×4', '2026Q1×2'],
    industriesL1: [
      mk('医药医疗', 71000, 0.21, 320),
      mk('科技TMT', 54000, 0.16, -180),
      mk('主要消费', 38000, 0.11, 95),
      mk('境外资产', 22000, 0.065, null, true),
      mk('未穿透', 152000, 0.45, null, true),
    ],
    industriesL2: [
      mk('化学制药', 44000, 0.13, 210, false, '医药医疗'),
      mk('中药', 27000, 0.08, 110, false, '医药医疗'),
      mk('半导体', 31000, 0.09, -120, false, '科技TMT'),
      mk('软件开发', 23000, 0.068, -60, false, '科技TMT'),
      mk('酿酒行业', 38000, 0.11, 95, false, '主要消费'),
      mk('港股', 13000, 0.039, null, true, '境外资产'),
      mk('美股', 9000, 0.027, null, true, '境外资产'),
      mk('现金理财·未披露', 152000, 0.45, null, true, '未穿透'),
    ],
    stocks: [
      {
        stockCode: '600519', stockName: '贵州茅台', sectorL1: '主要消费', industryL2: '酿酒行业',
        marketValue: 21000, pct: 0.062, fundCount: 4, funds: [], dayChangePct: 0.012, dayContribution: 252, hiddenWarning: true,
      },
      {
        stockCode: '600276', stockName: '恒瑞医药', sectorL1: '医药医疗', industryL2: '化学制药',
        marketValue: 18500, pct: 0.055, fundCount: 3, funds: [], dayChangePct: -0.008, dayContribution: -148, hiddenWarning: true,
      },
      {
        stockCode: '002049', stockName: '紫光国微', sectorL1: '科技TMT', industryL2: '半导体',
        marketValue: 12000, pct: 0.036, fundCount: 2, funds: [], dayChangePct: 0.021, dayContribution: 252, hiddenWarning: false,
      },
    ],
    cr5: 0.24,
    cr10: 0.38,
    funds: [],
    unpenetratedMv: 152000,
    hasQuotes: true,
    asOf: new Date().toISOString().slice(0, 19).replace('T', ' '),
  };
}


// ============ P1：基金重合矩阵 / 单基金穿透 ============

export interface OverlapFundBrief {
  code: string;
  name: string;
  marketValue: number;
  coverage: number;
}

export interface OverlapCell {
  i: number;
  j: number;
  /** 权重重合度 = Σ min(wᵢₛ, wⱼₛ)（1.0 = 完全复制） */
  weightOverlap: number;
  /** top10 Jaccard = |∩| / |∪|（集合口径） */
  jaccard: number;
  commonCount: number;
}

export interface OverlapResult {
  funds: OverlapFundBrief[];
  /** 上三角单元（i<j） */
  cells: OverlapCell[];
  maxWeightOverlap: number;
  asOf: string;
}

export interface FundLookthroughResult {
  fundCode: string;
  fundName: string;
  marketValue: number;
  coverage: number;
  reportPeriod: string | null;
  /** 穿透口径：index_constituent=按跟踪指数成分穿透；disclosure_top10=披露前十大口径 */
  penetrationSource: 'disclosure_top10' | 'index_constituent';
  industriesL1: IndustrySlice[];
  industriesL2: IndustrySlice[];
  topStocks: StockRow[];
  unpenetratedMv: number;
  asOf: string;
}

/** P1：基金两两重合矩阵（纯 DB 聚合，无网络请求） */
export async function lookthroughOverlap(platform: string | null = null): Promise<OverlapResult> {
  if (!isTauri) return mockOverlap();
  return (await invokeWithTimeout('lookthrough_overlap', { platform: platform ?? null }, 30000, '基金重合')) as OverlapResult;
}

/** P1：单基金穿透（分母=该基金市值，口径与组合穿透一致） */
export async function lookthroughFund(code: string): Promise<FundLookthroughResult> {
  if (!isTauri) return mockFundLookthrough(code);
  return (await invokeWithTimeout('lookthrough_fund', { code }, 30000, '单基金穿透')) as FundLookthroughResult;
}

/** 浏览器预览回退：静态示例 */
function mockOverlap(): OverlapResult {
  return {
    funds: [
      { code: '110011', name: '易方达优质精选', marketValue: 75000, coverage: 0.82 },
      { code: '161725', name: '招商中证白酒', marketValue: 50000, coverage: 0.68 },
      { code: '005827', name: '易方达蓝筹精选', marketValue: 60000, coverage: 0.79 },
    ],
    cells: [
      { i: 0, j: 1, weightOverlap: 0.21, jaccard: 0.18, commonCount: 3 },
      { i: 0, j: 2, weightOverlap: 0.56, jaccard: 0.44, commonCount: 6 },
      { i: 1, j: 2, weightOverlap: 0.19, jaccard: 0.15, commonCount: 2 },
    ],
    maxWeightOverlap: 0.56,
    asOf: new Date().toISOString().slice(0, 19).replace('T', ' '),
  };
}

function mockFundLookthrough(code: string): FundLookthroughResult {
  const mk = (key: string, mv: number, isVirtual = false, parent: string | null = null): IndustrySlice =>
    ({ key, marketValue: mv, pct: 0, dayContribution: null, isVirtual, parent });
  const l1 = [
    mk('医药医疗', 21000), mk('科技TMT', 15000), mk('境外资产', 8000, true), mk('未穿透', 31000, true),
  ];
  const total = l1.reduce((a, b) => a + b.marketValue, 0);
  for (const x of l1) x.pct = x.marketValue / total;
  return {
    fundCode: code,
    fundName: '示例基金',
    marketValue: total,
    coverage: 0.69,
    reportPeriod: '2026Q2',
    penetrationSource: 'disclosure_top10',
    industriesL1: l1,
    industriesL2: [mk('化学制药', 13000, false, '医药医疗'), mk('中药', 8000, false, '医药医疗'), mk('港股', 8000, true, '境外资产'), mk('现金理财·未披露', 31000, true, '未穿透')],
    topStocks: [],
    unpenetratedMv: 31000,
    asOf: new Date().toISOString().slice(0, 19).replace('T', ' '),
  };
}

// ============ P2：重合矩阵钻取 / 风格箱 ============

/** 钻取一对基金的共同持仓明细中的单只股票 */
export interface OverlapCommonHolding {
  stockCode: string;
  stockName: string;
  /** 在 A 基金中的穿透权重（0~1） */
  weightA: number;
  /** 在 B 基金中的穿透权重（0~1） */
  weightB: number;
}

export interface OverlapDetailResult {
  codeA: string;
  nameA: string;
  codeB: string;
  nameB: string;
  /** 权重重合度 = Σ min(wᵢₛ, wⱼₛ)（两基金共同持仓逐股取小后求和） */
  weightOverlap: number;
  /** 共同持股 Jaccard = |∩| / |∪| */
  jaccard: number;
  /** 共同持股数 */
  commonCount: number;
  /** 共同持仓明细（按 min 权重降序） */
  common: OverlapCommonHolding[];
  asOf: string;
}

export interface StyleStockBrief {
  stockCode: string;
  stockName: string;
  marketValue: number;
}

/** 风格箱九宫格单个单元格（大/中/小 × 价值/核心/成长） */
export interface StyleCell {
  /** 规模档：大 / 中 / 小 */
  size: string;
  /** 风格档：价值 / 核心 / 成长 */
  style: string;
  marketValue: number;
  pct: number;
  stockCount: number;
  topStocks: StyleStockBrief[];
}

export interface StyleBoxResult {
  /** 组合总市值（分母） */
  totalMv: number;
  /** 已纳入风格箱的穿透市值（含九宫格 + 境外 + 估值缺失） */
  coveredMv: number;
  /** 覆盖率 = coveredMv / totalMv */
  coveredPct: number;
  cells: StyleCell[];
  /** 境外资产穿透市值（单列，不计入九宫格） */
  overseasMv: number;
  /** 无市值 / 亏损股估值缺失市值（单列，不计入九宫格） */
  noValuationMv: number;
  /** 风格快照日期（YYYY-MM-DD HH:mm），无则 null */
  snapshotAt: string | null;
  asOf: string;
}

/** P2：重合矩阵钻取（逐对聚合共同持仓，无网络请求） */
export async function lookthroughOverlapDetail(codeA: string, codeB: string): Promise<OverlapDetailResult> {
  if (!isTauri) return mockOverlapDetail(codeA, codeB);
  return (await invokeWithTimeout('lookthrough_overlap_detail', { codeA, codeB }, 30000, '重合明细')) as OverlapDetailResult;
}

/** P2：风格箱九宫格（快照估算，东财公开接口，非晨星官方风格箱） */
export async function lookthroughStyle(platform: string | null = null): Promise<StyleBoxResult> {
  if (!isTauri) return mockStyleBox();
  return (await invokeWithTimeout('lookthrough_style', { platform: platform ?? null }, 60000, '风格箱')) as StyleBoxResult;
}

/** 浏览器预览回退：空共同持仓明细 */
function mockOverlapDetail(codeA: string, codeB: string): OverlapDetailResult {
  return {
    codeA,
    nameA: codeA,
    codeB,
    nameB: codeB,
    weightOverlap: 0,
    jaccard: 0,
    commonCount: 0,
    common: [],
    asOf: new Date().toISOString().slice(0, 19).replace('T', ' '),
  };
}

/** 浏览器预览回退：9 个空 cell 的合法空结构（大中小 × 价值核心成长 固定序） */
function mockStyleBox(): StyleBoxResult {
  const sizes = ['大', '中', '小'];
  const styles = ['价值', '核心', '成长'];
  const cells: StyleCell[] = [];
  for (const size of sizes) {
    for (const style of styles) {
      cells.push({ size, style, marketValue: 0, pct: 0, stockCount: 0, topStocks: [] });
    }
  }
  return {
    totalMv: 0,
    coveredMv: 0,
    coveredPct: 0,
    cells,
    overseasMv: 0,
    noValuationMv: 0,
    snapshotAt: null,
    asOf: new Date().toISOString().slice(0, 19).replace('T', ' '),
  };
}

// ============ 对外 API ============

export async function getOverview(platform: string | null = null): Promise<OverviewResult> {
  if (!isTauri) return mockOverview();
  return (await invokeWithTimeout('get_overview', { platform: platform ?? null }, 45000, '刷新总览')) as OverviewResult;
}

export async function getFundDetail(code: string): Promise<FundDetailResult> {
  if (!isTauri) return mockFundDetail(code);
  return (await invoke('get_fund_detail', { code })) as FundDetailResult;
}

/**
 * 手动改仓：更新某基金的持仓份额（与持仓成本）。
 * costAmount 由调用方按"保持单位成本不变"口径传入（avgCost × 新份额），
 * 后端 set_baseline 落库后，市值/累计盈亏等由 get_fund_detail 用"份额 × 最新净值"重算。
 * mock 模式（非 Tauri）下仅空操作，不改变内存态。
 */
export async function updatePosition(code: string, shares: number, costAmount: number, platform?: string): Promise<void> {
  if (!isTauri) return;
  await invoke('update_position', { code, shares, costAmount, platform: platform ?? null });
}

/**
 * 修改持仓成本价（单位成本）：后端按「当前份额 × 成本价」重算持仓成本，
 * 只就地更新既有基线流水，**不产生任何操作记录**（不新增交易/盘点流水）。
 */
export async function updatePositionCost(code: string, costPrice: number, platform?: string): Promise<void> {
  if (!isTauri) return;
  await invoke('update_position_cost', { code, costPrice, platform: platform ?? null });
}

export async function getStats(platform: string | null = null): Promise<StatsResult> {
  if (!isTauri) return mockStats();
  return (await invoke('get_stats', { platform: platform ?? null })) as StatsResult;
}

export async function importScreenshots(platform: string, _filePaths: string[]): Promise<ImportPreview> {
  if (!isTauri) return mockImport(platform);
  return (await invoke('import_screenshots', { platform, filePaths: _filePaths })) as ImportPreview;
}

// 持仓截图 OCR 导入——内存字节版（M2-P0：移动端 content:// URI 无文件路径，前端传 base64）
export async function importScreenshotsB64(platform: string, imagesB64: string[]): Promise<ImportPreview> {
  if (!isTauri) return mockImport(platform);
  return (await invoke('import_screenshots_b64', { platform, imagesB64 })) as ImportPreview;
}

/// 交易记录截图 OCR：识别买/卖/分红流水，返回可编辑预览（不落库，由前端核对后调用 importTransactions）。
export async function importTxnScreenshots(platform: string, filePaths: string[]): Promise<ImportTxnPreview> {
  if (!isTauri) {
    return {
      platform,
      platformName: platform,
      detectedCount: 0,
      txns: [],
      ocrReady: false,
      note: '非 Tauri 环境：请用桌面端运行以启用截图 OCR',
      rawLines: [],
    };
  }
  return (await invoke('import_txn_screenshots', { platform, filePaths })) as ImportTxnPreview;
}

// 交易记录截图 OCR 预览——内存字节版（M2-P0：移动端内容传参，见 importScreenshotsB64 说明）
export async function importTxnScreenshotsB64(platform: string, imagesB64: string[]): Promise<ImportTxnPreview> {
  if (!isTauri) {
    return {
      platform,
      platformName: platform,
      detectedCount: 0,
      txns: [],
      ocrReady: false,
      note: '非 Tauri 环境：请用桌面端运行以启用截图 OCR',
      rawLines: [],
    };
  }
  return (await invoke('import_txn_screenshots_b64', { platform, imagesB64 })) as ImportTxnPreview;
}

// ---- 交易流水（单机单账户，账户维度不暴露给前端） ----
export async function listTransactions(fundCode?: string): Promise<TransactionOut[]> {
  if (!isTauri) return mockTransactions();
  return (await invoke('list_transactions', { fundCode: fundCode ?? null })) as TransactionOut[];
}
export async function addTransaction(
  txnType: TxnType,
  fundCode: string | null,
  shares: number | null,
  amount: number,
  price: number | null,
  txnDate: string,
  txnTime?: string,
  note?: string,
  platform = 'alipay',
): Promise<number> {
  if (!isTauri) return 1;
  // platform 必须透传：后端按 (基金, 平台) 累计流水，空平台会让手动记账落到
  // 「无平台」幻影持仓，或与空平台基线键碰撞而覆盖已有持仓（见记账 bug 修复）。
  return (await invoke('add_transaction', {
    txnType,
    fundCode,
    shares,
    amount,
    price,
    txnDate,
    txnTime: txnTime ?? null,
    note: note ?? null,
    platform,
  })) as number;
}
export async function deleteTransaction(id: number): Promise<void> {
  if (!isTauri) return;
  await invoke('delete_transaction', { id });
}

/// 增量导入交易记录（买/卖/分红）。sourceRef 标识导入批次：
/// 提供则与已有同批次幂等替换（避免叠加），不提供则纯追加。
export async function importTransactions(
  items: ImportTxn[],
  sourceRef?: string | null,
  platform?: string | null,
): Promise<number> {
  if (!isTauri) return items.length;
  return (await invoke('import_transactions', {
    items,
    sourceRef: sourceRef ?? null,
    platform: platform ?? null,
  })) as number;
}

// ---- 报表（单机单账户，始终全账户聚合；平台拆分属后续增强） ----
export async function getDailyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('日');
  return (await invoke('get_daily_report')) as PeriodReport;
}
export async function getWeeklyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('周');
  return (await invoke('get_weekly_report')) as PeriodReport;
}
export async function getMonthlyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('月');
  return (await invoke('get_monthly_report')) as PeriodReport;
}
export async function getYearlyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('年');
  return (await invoke('get_yearly_report')) as PeriodReport;
}
export async function getPnlCalendar(months = 3): Promise<SnapshotPoint[]> {
  if (!isTauri) return mockCalendar();
  return (await invoke('get_pnl_calendar', { months })) as SnapshotPoint[];
}

// 区间操作收益：指定 [start, end] 计算区间内买入/卖出的涨跌收益（交易口径）。
export async function getOperationPnl(startDate: string, endDate: string): Promise<OperationPnl> {
  if (!isTauri) return mockOperationPnl(startDate, endDate);
  return (await invoke('get_operation_pnl', { startDate, endDate })) as OperationPnl;
}

// 将文本写入用户选定的本地文件（周报/月报「保存为 .md」）。浏览器预览模式无文件系统，no-op。
export async function writeTextFile(targetPath: string, content: string): Promise<void> {
  if (!isTauri) return;
  await invoke('write_text_file', { targetPath, content });
}

// 读取本地图片为 base64 data URL（后端读取，规避 asset 协议作用域限制）
export async function readImageDataUrl(path: string): Promise<string> {
  if (!isTauri) return '';
  return (await invoke('read_image_data_url', { path })) as string;
}

export async function refreshQuotes(): Promise<{ ok: boolean; at: string }> {
  if (!isTauri) return { ok: true, at: new Date().toLocaleString('zh-CN') };
  return (await invokeWithTimeout('refresh_quotes', undefined, 45000, '刷新行情')) as { ok: boolean; at: string };
}

export async function fetchDisclosure(code: string): Promise<{ ok: boolean }> {
  if (!isTauri) return { ok: true };
  await invoke('fetch_disclosure', { code });
  return { ok: true };
}

/** 批量后台任务进度（披露抓取 / 净值刷新共用，后台线程执行 + 前端轮询）。 */
export interface FetchTaskProgress {
  running: boolean;
  total: number;
  done: number;
  ok: number;
  failed: number;
  /** 跳过只数（净值刷新：已最新；披露任务恒 0） */
  skipped: number;
  /** 取到「今日」净值的只数（仅净值刷新非 0） */
  gotToday: number;
  /** 当前正在处理的基金代码（null=空闲或已结束） */
  current: string | null;
  failedCodes: string[];
  startedAt: string | null;
  finishedAt: string | null;
  /** 本次任务是否被用户取消（仅结束后为 true） */
  cancelled: boolean;
}

const idleTaskProgress: FetchTaskProgress = {
  running: false, total: 0, done: 0, ok: 0, failed: 0, skipped: 0, gotToday: 0,
  current: null, failedCodes: [], startedAt: null, finishedAt: null, cancelled: false,
};

/** 启动批量披露抓取后台任务（幂等：已在跑则直接返回当前进度）。 */
export async function disclosureFetchStart(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('disclosure_fetch_start')) as FetchTaskProgress;
}

/** 轮询批量披露抓取进度。 */
export async function disclosureFetchProgress(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('disclosure_fetch_progress')) as FetchTaskProgress;
}

/** 请求取消进行中的批量抓取（协作式：当前这只跑完即停）。返回是否确有任务在跑。 */
export async function disclosureFetchCancel(): Promise<boolean> {
  if (!isTauri) return false;
  return (await invoke('disclosure_fetch_cancel')) as boolean;
}

/** 启动今日净值刷新后台任务（幂等：已在跑则直接返回当前进度）。 */
export async function navRefreshStart(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('nav_refresh_start')) as FetchTaskProgress;
}

/** 轮询今日净值刷新进度。 */
export async function navRefreshProgress(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('nav_refresh_progress')) as FetchTaskProgress;
}

/** 请求取消进行中的净值刷新（协作式：当前这只跑完即停）。返回是否确有任务在跑。 */
export async function navRefreshCancel(): Promise<boolean> {
  if (!isTauri) return false;
  return (await invoke('nav_refresh_cancel')) as boolean;
}

/** 启动股票行业画像补拉后台任务（幂等：已在跑则直接返回当前进度）。 */
export async function stockProfilesStart(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('stock_profiles_start')) as FetchTaskProgress;
}

/** 轮询股票行业画像补拉进度。 */
export async function stockProfilesProgress(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('stock_profiles_progress')) as FetchTaskProgress;
}

/** 请求取消进行中的行业画像补拉（协作式：当前这只跑完即停）。 */
export async function stockProfilesCancel(): Promise<boolean> {
  if (!isTauri) return false;
  return (await invoke('stock_profiles_cancel')) as boolean;
}

/** 启动股票风格估值补拉后台任务（幂等：已在跑则直接返回当前进度）。 */
export async function stockStyleStart(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('stock_style_start')) as FetchTaskProgress;
}

/** 轮询股票风格估值补拉进度。 */
export async function stockStyleProgress(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('stock_style_progress')) as FetchTaskProgress;
}

/** 请求取消进行中的风格估值补拉（协作式：当前这只跑完即停）。 */
export async function stockStyleCancel(): Promise<boolean> {
  if (!isTauri) return false;
  return (await invoke('stock_style_cancel')) as boolean;
}

/** 启动指数成分表补拉后台任务（幂等：已在跑则直接返回当前进度）。 */
export async function indexConstituentsStart(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('index_constituents_start')) as FetchTaskProgress;
}

/** 轮询指数成分表补拉进度。 */
export async function indexConstituentsProgress(): Promise<FetchTaskProgress> {
  if (!isTauri) return idleTaskProgress;
  return (await invoke('index_constituents_progress')) as FetchTaskProgress;
}

/** 请求取消进行中的指数成分补拉（协作式：当前这只跑完即停）。 */
export async function indexConstituentsCancel(): Promise<boolean> {
  if (!isTauri) return false;
  return (await invoke('index_constituents_cancel')) as boolean;
}

// ---- 披露持仓：历史期次与「较上期」变化 ----

export interface HoldingChange {
  stockCode: string;
  stockName: string;
  /** 本期占净值 0~1；本期已无此股为 null */
  currWeight: number | null;
  /** 上期占净值 0~1；上期无此股为 null */
  prevWeight: number | null;
  /** 变化量 = 本期 − 上期 */
  delta: number;
  /** new=新增 / exit=退出 / increase=加仓 / decrease=减仓 / flat=持平 */
  changeType: 'new' | 'exit' | 'increase' | 'decrease' | 'flat';
}

export interface HoldingChangesResult {
  code: string;
  /** 本期期次（最新）；无披露时为空串 */
  currPeriod: string;
  /** 上期期次；无历史可比时为空串 */
  prevPeriod: string;
  /** 是否已存在上期（false 时界面应提示「暂无可对比的上期」） */
  hasPrev: boolean;
  changes: HoldingChange[];
}

export interface FetchDisclosureHistoryResult {
  code: string;
  attempted: number;
  storedPeriods: string[];
  storedRows: number[];
  /** 该基金当前已入库的全部期次（从旧到新） */
  allPeriods: string[];
  at: string;
}

/// 「本期 vs 上期」的持仓变化（新增/退出/加仓/减仓/持平）。仅用于展示，不参与估值——
/// 估值始终只用最新一期，否则多期叠加会让覆盖度爆表。
export async function getHoldingChanges(code: string): Promise<HoldingChangesResult> {
  if (!isTauri) {
    return { code, currPeriod: '2026Q2', prevPeriod: '', hasPrev: false, changes: [] };
  }
  return (await invoke('get_holding_changes', { code })) as HoldingChangesResult;
}

/**
 * 补录某基金的历史披露持仓：按候选期次从新到旧逐期抓取入库。
 *
 * 存在必要性：东财接口每次只返回「最新一期」，光放开存储历史也不会凭空出现——
 * 必须主动按 (年, 季) 逐期抓取，才能让「较上期」对比立刻有数据可看。
 * 单期 8 秒超时、最多 12 期，整体给 180 秒兜底。
 */
export async function fetchDisclosureHistory(
  code: string,
  quarters = 8,
): Promise<FetchDisclosureHistoryResult> {
  if (!isTauri) {
    return {
      code,
      attempted: 0,
      storedPeriods: [],
      storedRows: [],
      allPeriods: [],
      at: new Date().toLocaleString('zh-CN'),
    };
  }
  return (await invokeWithTimeout(
    'fetch_disclosure_history',
    { code, quarters },
    180000,
    '补录历史持仓',
  )) as FetchDisclosureHistoryResult;
}

// ---- 批量刷新今日官方净值 ----

export interface RefreshNavResult {
  /** 全部持仓基金数 */
  total: number;
  /** 已持有今日/最新净值、无需刷新的只数 */
  skipped: number;
  /** 本次实际抓取并写入的只数 */
  fetched: number;
  /** 其中成功取到「今日」官方净值的只数（盘面将显示「实际」） */
  gotToday: number;
  /** 抓取失败只数 */
  failed: number;
  /** 抓取失败的基金代码 */
  failedCodes: string[];
  /** 操作完成时间 */
  at: string;
}

/**
 * 【已废弃 → navRefreshStart】批量刷新「今日官方净值尚未取到」的基金官方净值。
 * 2026-09-11 起改走后台任务三命令（nav_refresh_start/_progress/_cancel），
 * 旧同步命令 refresh_official_nav 后端保留仅供兼容，前端不再调用。
 */

export async function deleteFund(code: string): Promise<void> {
  if (!isTauri) return;
  await invoke('delete_fund', { code });
}

// ---- 净值走势 / 成本走势 ----
export async function getFundSeries(code: string, range = 'all'): Promise<FundSeries> {
  if (!isTauri) return mockFundSeries(code, range);
  return (await invoke('get_fund_series', { code, range })) as FundSeries;
}

export async function refreshNavHistory(code: string): Promise<number> {
  if (!isTauri) return 0;
  return (await invoke('refresh_nav_history', { code })) as number;
}

// ---- 数据库备份 / 恢复（SPEC §F5：SQLite 可导出备份） ----

export interface BackupInfo {
  path: string;
  size: number;
  at: string;
}

// 备份文件的内存字节载体（移动端导出：后端回传 base64，前端走系统分享落地）
export interface BackupB64Info {
  data: string;
  size: number;
  at: string;
  fileName: string;
}

/** 导出当前数据库为独立备份文件（在线一致快照）。targetPath 由系统保存对话框选定。 */
export async function exportDb(targetPath: string): Promise<BackupInfo> {
  if (!isTauri) return { path: targetPath, size: 0, at: new Date().toLocaleString('zh-CN') };
  return (await invoke('export_db', { targetPath })) as BackupInfo;
}

/** 从备份文件恢复数据库（整个覆盖当前数据，调用前前端须二次确认）。 */
export async function importDb(sourcePath: string): Promise<BackupInfo> {
  if (!isTauri) return { path: sourcePath, size: 0, at: new Date().toLocaleString('zh-CN') };
  return (await invoke('import_db', { sourcePath })) as BackupInfo;
}

/** 导出数据库备份——内存字节版（移动端：无系统保存对话框可写路径，回传 base64 由前端分享落地）。 */
export async function exportDbB64(): Promise<BackupB64Info> {
  if (!isTauri) {
    return { data: '', size: 0, at: new Date().toLocaleString('zh-CN'), fileName: 'fundlens-backup.db' };
  }
  return (await invoke('export_db_b64')) as BackupB64Info;
}

/** 从内存字节恢复数据库——内容传参版（移动端：<input type=file> 读 .db → base64 → 后端整库覆盖）。 */
export async function importDbB64(data: string): Promise<BackupInfo> {
  if (!isTauri) {
    return { path: '(base64)', size: 0, at: new Date().toLocaleString('zh-CN') };
  }
  return (await invoke('import_db_b64', { data })) as BackupInfo;
}


// ============================================================
// 策略信号层（valuation_grid 引擎移植，P0）
// ============================================================

export interface GridConfigOut {
  fundCode: string;
  fundName?: string | null;
  fundType: string;
  enabled: boolean;
  maxPosition?: number | null;
  volSensitivity?: number | null;
  sellFeeRate?: number | null;
  cooldownSellDate?: string | null;
  peakNav?: number | null;
  shares: number;
  costAmount: number;
  platforms: string[];
}

export interface GridFifoStep {
  batchId: string;
  buyDate: string;
  sellShares: number;
  batchTotalShares: number;
  isFullSell: boolean;
  isPassthrough: boolean;
  holdDays: number;
  feeRate: number;
  profitPct: number;
  estimatedFee: number;
  estimatedNetProfit: number;
  reason: string;
  note: string;
}

export interface GridFifoPlan {
  totalShares: number;
  batchCount: number;
  steps: GridFifoStep[];
  totalEstimatedFee: number;
  totalEstimatedProfit: number;
  hasPassthrough: boolean;
  passthroughWarning?: string | null;
  passthroughLossTotal: number;
  instruction: string;
}

export interface GridSignalOut {
  fundCode: string;
  fundName?: string | null;
  signalDate: string;
  source: string;
  signalName: string;
  action: 'buy' | 'sell' | 'hold' | string;
  priority: number;
  reason: string;
  amount?: number | null;
  sellShares?: number | null;
  sellPct?: number | null;
  alert: boolean;
  confidence: number;
  estChangePct: number;
  currentNav: number;
  totalProfitPct?: number | null;
  regime: string;
  targetBatchId?: string | null;
  fifoPlan?: GridFifoPlan | null;
  platforms: string[];
  isRebuy: boolean;
  pendingRebuyId?: number | null;
  rebuyPlan?: { triggerNav: number; amount: number; ratio: number; trend: string; discount: number } | null;
}

export interface GridComputeResult {
  signals: GridSignalOut[];
  session: 'pre' | 'intraday' | 'post';
  regime: string;
  autoRegime: boolean;
  computedAt: string;
  budgetCap?: number | null;
  budgetUsed?: number | null;
}

export interface GridSettingsOut {
  regime: string;
  auto: boolean;
  manual: boolean;
  manualRegime?: string | null;
  cashAvailable?: string | null;
}

export interface GridTodayBadge {
  fundCode: string;
  signalName?: string | null;
  action: string;
  alert: boolean;
}

export interface GridHistoryRow {
  fund_code: string;
  signal_date: string;
  signal_name?: string | null;
  action?: string | null;
  reason?: string | null;
  amount?: number | null;
  sell_pct?: number | null;
  today_change?: number | null;
  current_nav?: number | null;
  total_profit_pct?: number | null;
  outcome_t3?: number | null;
  outcome_t5?: number | null;
  outcome_t10?: number | null;
  executed: number;
  fund_name?: string | null;
}

export async function gridListConfig(): Promise<GridConfigOut[]> {
  return (await invoke('grid_list_config')) as GridConfigOut[];
}

export async function gridEnableFund(fundCode: string, enabled: boolean, maxPosition?: number | null): Promise<void> {
  await invoke('grid_enable_fund', { fundCode, enabled, maxPosition: maxPosition ?? null });
}

export async function gridComputeSignals(): Promise<GridComputeResult> {
  return (await invokeWithTimeout('grid_compute_signals', undefined, 180000, '计算策略信号')) as GridComputeResult;
}

export async function gridSignalHistory(fundCode?: string | null, limit?: number): Promise<GridHistoryRow[]> {
  return (await invoke('grid_signal_history', { fundCode: fundCode ?? null, limit: limit ?? 30 })) as GridHistoryRow[];
}

export async function gridSetRegime(regime: string, auto?: boolean, manual?: boolean, cashAvailable?: number | null): Promise<GridSettingsOut> {
  return (await invoke('grid_set_regime', { regime, auto: auto ?? true, manual: manual ?? false, cashAvailable: cashAvailable ?? null })) as GridSettingsOut;
}

export async function gridGetSettings(): Promise<GridSettingsOut> {
  return (await invoke('grid_get_settings')) as GridSettingsOut;
}

export async function gridTodaySignals(signalDate?: string): Promise<GridTodayBadge[]> {
  return (await invoke('grid_today_signals', { signalDate: signalDate ?? null })) as GridTodayBadge[];
}

export interface GridOutcomeStatRow {
  action: string;
  count: number;
  winCount: number;
  winRate: number;
  avgT3?: number | null;
  avgT5?: number | null;
  avgT10?: number | null;
}

export interface GridOutcomeResult {
  updated: number;
  stats: GridOutcomeStatRow[];
}

export interface GridPendingRow {
  id: number;
  fundCode: string;
  fundName?: string | null;
  createdDate?: string | null;
  expireDate?: string | null;
  triggerNav?: number | null;
  amount?: number | null;
  ratio?: number | null;
  sourceSignal?: string | null;
  signalLabel?: string | null;
  sellNav?: number | null;
  status: string;
  triggeredDate?: string | null;
}

export async function gridSaveFund(
  fundCode: string,
  maxPosition?: number | null,
  volSensitivity?: number | null,
  sellFeeRate?: number | null,
  cooldownSellDate?: string | null,
): Promise<void> {
  await invoke('grid_save_fund', {
    fundCode,
    maxPosition: maxPosition ?? null,
    volSensitivity: volSensitivity ?? null,
    sellFeeRate: sellFeeRate ?? null,
    cooldownSellDate: cooldownSellDate ?? null,
  });
}

export async function gridBackfillOutcomes(): Promise<GridOutcomeResult> {
  return (await invoke('grid_backfill_outcomes')) as GridOutcomeResult;
}

export async function gridListPending(fundCode?: string | null, limit?: number): Promise<GridPendingRow[]> {
  return (await invoke('grid_list_pending', { fundCode: fundCode ?? null, limit: limit ?? 50 })) as GridPendingRow[];
}

export async function gridPendingCancel(fundCode: string, id: number): Promise<void> {
  await invoke('grid_pending_cancel', { fundCode, id });
}

// ============================================================
// 多设备同步 M3：设备快照导出/导入 + 冲突/状态（传输无关）
//
// 快照 = 全量存活行（ts 为该行 updated_at）+ 删除墓碑，JSONL 载荷。
// 文件是本阶段的通道；M2 云通道接入后复用同一批封装与同一套 UI。
// ============================================================

export interface SyncSnapshotInfo {
  path: string;
  count: number;
  size: number;
  at: string;
}

export interface SyncSnapshotB64Info {
  fileName: string;
  data: string;
  count: number;
  size: number;
  at: string;
}

export interface SyncSnapshotImportInfo {
  applied: number;
  conflicts: number;
  total: number;
  device: string;
  at: string;
  /** 本次导入前的自动备份文件名（M4；备份失败为 null） */
  backupFile: string | null;
}

export interface SyncConflictRow {
  id: number;
  tbl: string;
  /** 表的中文标签（后端统一映射） */
  tableLabel: string;
  rowKey: string;
  device: string;
  resolved: number;
  createdAt: string;
}

export interface SyncStatus {
  deviceId: string;
  tablesSynced: number;
  pendingChanges: number;
  totalChanges: number;
  lastExportAt: string | null;
  lastImportAt: string | null;
  conflictCount: number;
  /** M4 自动备份：保留份数 / 现有份数 / 最近一份时间 / 备份目录 */
  backupKeep: number;
  backupCount: number;
  lastBackupAt: string | null;
  backupDir: string;
  /** M2 云通道：模式 / 是否可用 / 地址（不含令牌）/ 最近推送、拉取时间 / 已认识的远端设备数 */
  cloudMode: string;
  cloudReady: boolean;
  cloudEndpoint: string;
  cloudDir: string;
  cloudTokenSet: boolean;
  cloudLastPush: string | null;
  cloudLastPull: string | null;
  cloudPeers: number;
}

/** 云通道配置（不含令牌明文，只告知是否已设置）。 */
export interface CloudConfigInfo {
  /** off | dir | cloud | pg */
  mode: string;
  /** HTTP 模式：relay / 云函数地址 */
  endpoint: string;
  /** 本地目录模式：快照根目录 */
  dir: string;
  tokenSet: boolean;
  /** 配置是否完整、可发起同步 */
  ready: boolean;
}

/** 云通道连通性检查结果。 */
export interface CloudCheckInfo {
  mode: string;
  /** 远端条目总数 */
  items: number;
  /** 其中属于其它设备的快照数（潜在可拉取量） */
  others: number;
  deviceId: string;
}

/** 云端推送结果。 */
export interface CloudPushInfo {
  key: string;
  count: number;
  size: number;
  at: string;
}

/** 单个远端设备的拉取明细。 */
export interface CloudPullDetail {
  device: string;
  key: string;
  applied: number;
  conflicts: number;
  at: string;
}

/** 云端拉取结果。 */
export interface CloudPullInfo {
  planned: number;
  pulled: number;
  skippedOwn: number;
  applied: number;
  conflicts: number;
  details: CloudPullDetail[];
  at: string;
}

/** 一份整库备份产物（M4）。 */
export interface BackupEntry {
  file: string;
  size: number;
  at: string;
  tag: string;
}

/** 导出设备快照到用户选定路径（桌面端）。 */
export async function syncExportSnapshot(targetPath: string): Promise<SyncSnapshotInfo> {
  if (!isTauri) return { path: targetPath, count: 0, size: 0, at: new Date().toLocaleString('zh-CN') };
  return (await invoke('sync_export_snapshot', { targetPath })) as SyncSnapshotInfo;
}

/** 导出设备快照为内存字节（移动端：前端走系统分享落地）。 */
export async function syncExportSnapshotB64(): Promise<SyncSnapshotB64Info> {
  if (!isTauri) {
    return { fileName: 'fundlens-sync.jsonl', data: '', count: 0, size: 0, at: new Date().toLocaleString('zh-CN') };
  }
  return (await invoke('sync_export_snapshot_b64')) as SyncSnapshotB64Info;
}

/** 从快照文件导入（桌面端路径版；合并语义，LWW，不整库覆盖）。 */
export async function syncImportSnapshot(sourcePath: string): Promise<SyncSnapshotImportInfo> {
  if (!isTauri) {
    return { applied: 0, conflicts: 0, total: 0, device: 'mock', at: new Date().toLocaleString('zh-CN'), backupFile: null };
  }
  return (await invoke('sync_import_snapshot', { sourcePath })) as SyncSnapshotImportInfo;
}

/** 从内存字节导入快照（移动端内容传参版）。 */
export async function syncImportSnapshotB64(data: string): Promise<SyncSnapshotImportInfo> {
  if (!isTauri) {
    return { applied: 0, conflicts: 0, total: 0, device: 'mock', at: new Date().toLocaleString('zh-CN'), backupFile: null };
  }
  return (await invoke('sync_import_snapshot_b64', { data })) as SyncSnapshotImportInfo;
}

/** 冲突中单个字段的本地值 vs 远端值（值可能为 null）。 */
export interface SyncConflictField {
  col: string;
  local: unknown;
  remote: unknown;
}

/** 一条冲突的字段级详情（M3 裁决用）。 */
export interface SyncConflictDetail {
  id: number;
  tbl: string;
  /** 表的中文标签（后端下发，前后端单一事实源） */
  tableLabel: string;
  rowKey: string;
  device: string;
  createdAt: string;
  resolved: boolean;
  /** upsert 改行 / delete 删行 / corrupt 载荷无法解析 */
  op: 'upsert' | 'delete' | 'corrupt';
  /** 本地当前是否还有这一行 */
  localExists: boolean;
  /** 逐字段差异（主键与 updated_at 不列入） */
  fields: SyncConflictField[];
  /** 无实质差异：采用远端与保留本地结果相同 */
  identical: boolean;
  /** op === 'corrupt' 时的解析失败说明 */
  payloadError: string | null;
}

/** 冲突裁决结果。 */
export interface SyncConflictResolveOut {
  /** 实际改为已解的条数 */
  resolved: number;
  /** 写回本地的行数（保留本地恒为 0） */
  applied: number;
  /** 未能处理的条数（载荷损坏等），这些条目保持未解 */
  failed: number;
}

/** 冲突裁决口径：保留本地（丢弃远端）| 采用远端（覆盖本地）。 */
export type SyncConflictChoice = 'local' | 'remote';

/** 列出 LWW 冲突（未解优先、最新在前，最多 200 条）。 */
export async function syncListConflicts(): Promise<SyncConflictRow[]> {
  if (!isTauri) return [];
  return (await invoke('sync_list_conflicts')) as SyncConflictRow[];
}

/** 读取一条冲突的字段级详情（本地当前行 vs 远端被拒变更）。 */
export async function syncConflictDetail(id: number): Promise<SyncConflictDetail> {
  if (!isTauri) throw new Error('冲突详情仅在桌面端可用');
  return (await invoke('sync_conflict_detail', { id })) as SyncConflictDetail;
}

/** 解算一条冲突（保留本地 / 采用远端）。 */
export async function syncConflictResolve(
  id: number,
  choice: SyncConflictChoice,
): Promise<SyncConflictResolveOut> {
  if (!isTauri) throw new Error('冲突裁决仅在桌面端可用');
  return (await invoke('sync_conflict_resolve', { id, choice })) as SyncConflictResolveOut;
}

/** 批量解算全部未解冲突。 */
export async function syncConflictsResolveAll(
  choice: SyncConflictChoice,
): Promise<SyncConflictResolveOut> {
  if (!isTauri) throw new Error('冲突裁决仅在桌面端可用');
  return (await invoke('sync_conflicts_resolve_all', { choice })) as SyncConflictResolveOut;
}

/** 同步状态：设备标识、参与表数、待同步变更数、最近导出/导入时间、未解冲突数。 */
export async function syncStatus(): Promise<SyncStatus> {
  if (!isTauri) {
    return {
      deviceId: 'browser-preview',
      tablesSynced: 13,
      pendingChanges: 0,
      totalChanges: 0,
      lastExportAt: null,
      lastImportAt: null,
      conflictCount: 0,
      backupKeep: 7,
      backupCount: 0,
      lastBackupAt: null,
      backupDir: '(浏览器预览)',
      cloudMode: 'off',
      cloudReady: false,
      cloudEndpoint: '',
      cloudDir: '',
      cloudTokenSet: false,
      cloudLastPush: null,
      cloudLastPull: null,
      cloudPeers: 0,
    };
  }
  return (await invoke('sync_status')) as SyncStatus;
}

/** 立即生成一份整库备份（M4）。 */
export async function syncCreateBackup(): Promise<BackupEntry> {
  if (!isTauri) return { file: 'fundlens-mock.db', size: 0, at: new Date().toLocaleString('zh-CN'), tag: 'manual' };
  return (await invoke('sync_create_backup')) as BackupEntry;
}

/** 列出全部整库备份（最新在前）。 */
export async function syncListBackups(): Promise<BackupEntry[]> {
  if (!isTauri) return [];
  return (await invoke('sync_list_backups')) as BackupEntry[];
}

/** 恢复某份整库备份的产物（M4）。 */
export interface RestoreBackupOut {
  /** 实际恢复成功的备份文件名 */
  file: string;
  /**
   * 恢复前自动生成的安全网备份文件名（before-restore 标签）。
   * 为 null 表示安全网生成失败——恢复已完成但当前数据已被覆盖且不可还原，前端须警告。
   */
  safetyBackup: string | null;
}

/**
 * 从整库备份文件恢复（M4）：先用 before-restore 标签自动备份当前整库，
 * 再用所选备份整库覆盖回去。破坏性、不可撤销，前端必须二次确认。
 */
export async function syncRestoreBackup(file: string): Promise<RestoreBackupOut> {
  if (!isTauri) return { file, safetyBackup: 'fundlens-before-restore.db' };
  return (await invoke('sync_restore_backup', { file })) as RestoreBackupOut;
}

/** 删除一份整库备份文件（M4）。不可撤销，前端应二次确认。 */
export async function syncDeleteBackup(file: string): Promise<void> {
  if (!isTauri) return;
  await invoke('sync_delete_backup', { file });
}

/** 设置自动备份保留份数（夹在 1..=60），立即剪枝；返回生效值。 */
export async function syncSetBackupKeep(keep: number): Promise<number> {
  if (!isTauri) return Math.min(60, Math.max(1, keep));
  return (await invoke('sync_set_backup_keep', { keep })) as number;
}

/** 读取云通道配置（不含令牌明文）。 */
export async function syncCloudConfigGet(): Promise<CloudConfigInfo> {
  if (!isTauri) return { mode: 'off', endpoint: '', dir: '', tokenSet: false, ready: false };
  return (await invoke('sync_cloud_config_get')) as CloudConfigInfo;
}

/**
 * 保存云通道配置。
 * `token` 传 null 表示「保持不变」（界面上留空即不修改）；传空串表示清空令牌。
 */
export async function syncCloudConfigSet(
  mode: string,
  endpoint: string,
  dir: string,
  token: string | null,
): Promise<CloudConfigInfo> {
  if (!isTauri) {
    return { mode, endpoint, dir, tokenSet: token === null ? false : token.length > 0, ready: false };
  }
  return (await invoke('sync_cloud_config_set', { mode, endpoint, dir, token })) as CloudConfigInfo;
}

/** 云通道连通性检查（只读：列出远端条目）。 */
export async function syncCloudCheck(): Promise<CloudCheckInfo> {
  if (!isTauri) return { mode: 'off', items: 0, others: 0, deviceId: 'browser-preview' };
  return (await invoke('sync_cloud_check')) as CloudCheckInfo;
}

/** 立即把本设备快照推送到云通道。 */
export async function syncCloudPush(): Promise<CloudPushInfo> {
  if (!isTauri) return { key: '', count: 0, size: 0, at: new Date().toLocaleString('zh-CN') };
  return (await invoke('sync_cloud_push')) as CloudPushInfo;
}

/** 立即从云通道拉取他设备快照并按行 LWW 合并。 */
export async function syncCloudPull(): Promise<CloudPullInfo> {
  if (!isTauri) {
    return { planned: 0, pulled: 0, skippedOwn: 0, applied: 0, conflicts: 0, details: [], at: new Date().toLocaleString('zh-CN') };
  }
  return (await invoke('sync_cloud_pull')) as CloudPullInfo;
}
