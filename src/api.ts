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
  summary: PortfolioSummary;
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
  const summary = summarizePortfolio(summaryInput, isTradingNow() ? 'intraday' : 'closed');
  positions.sort((a, b) => b.marketValue - a.marketValue);
  const marketSession: OverviewResult['marketSession'] = isTradingNow() ? 'intraday' : 'closed';
  return { summary, positions, trading: isTradingNow(), marketSession, asOf: new Date().toLocaleString('zh-CN') };
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
  const phase: FundDetailResult['marketSession'] = isTradingNow() ? 'intraday' : 'closed';
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

export interface FetchAllDisclosuresResult {
  total: number;
  ok: number;
  failed: number;
  failedCodes: string[];
  at: string;
}

/** 一键抓取所有基金的披露持仓（遍历本地全部基金，逐只拉取并写入）。 */
export async function fetchAllDisclosures(): Promise<FetchAllDisclosuresResult> {
  if (!isTauri) return { total: 0, ok: 0, failed: 0, failedCodes: [], at: new Date().toLocaleString('zh-CN') };
  return (await invokeWithTimeout('fetch_all_disclosures', undefined, 300000, '抓取披露持仓')) as FetchAllDisclosuresResult;
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
 * 批量刷新「今日官方净值尚未取到」的基金官方净值。
 * 后端仅对 nav_date 为空或早于昨日的基金发起请求，已持有最新净值的自动跳过。
 */
export async function refreshOfficialNav(): Promise<RefreshNavResult> {
  if (!isTauri) {
    return { total: 0, skipped: 0, fetched: 0, gotToday: 0, failed: 0, failedCodes: [], at: new Date().toLocaleString('zh-CN') };
  }
  return (await invokeWithTimeout('refresh_official_nav', undefined, 150000, '刷新官方净值')) as RefreshNavResult;
}

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
