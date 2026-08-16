// FundLens 前端 ↔ 后端桥接层
// 在 Tauri 运行时调用真实命令；在浏览器（评审/开发预览）回退到本地 mock，
// 保证 UI 不依赖 Rust 后端即可可视化。mock 与真实命令保持相同的返回结构（见 SPEC.md 第 5/6 节）。

import { MOCK_FUNDS, isTradingNow, PLATFORMS } from './lib/mockData';
import {
  valueFund,
  summarizePortfolio,
  type FundValuationResult,
  type PortfolioSummary,
  type DisclosedHolding,
  type StockQuote,
} from './valuation/engine';

// 是否运行在 Tauri 环境中
export const isTauri =
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in (window as unknown as Record<string, unknown>);

// 延迟加载 invoke，避免浏览器端打包/执行报错
async function invoke(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  const { invoke: tauriInvoke } = await import('@tauri-apps/api/core');
  return tauriInvoke(cmd, args);
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
  valuationApplicable?: boolean;
}

export interface PositionRow {
  fund: FundMeta;
  estNav: number;
  estChangePct: number;
  marketValue: number;
  dayPnl: number;
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
  /** QDII 延迟结算提示：T+1·海外交易中 / T+1·海外净值；非 QDII 为 null */
  delayNote?: string | null;
}

export interface OverviewResult {
  summary: PortfolioSummary;
  positions: PositionRow[];
  trading: boolean;
  /** 市场时段：intraday=交易中(当日预估) / post_close=盘后(当日实际) / prev_day=休市(上一交易日实际) */
  marketSession: 'intraday' | 'post_close' | 'prev_day';
  asOf: string;
}

export interface FundDetailResult {
  fund: FundMeta;
  valuation: FundValuationResult;
  quotes: { stockCode: string; stockName: string; price: number; prevClose: number; priceReturn: number }[];
  trading: boolean;
  /** 盘中实时估值（交易时段优先来源），无则为 null */
  realtimeEstimate?: { estNav: number; estChangePct: number; gztime: string } | null;
  /** QDII 延迟结算提示：T+1·海外交易中 / T+1·海外净值；非 QDII 为 null */
  delayNote?: string | null;
  /** 该基金的交易流水（买卖/分红/手动），按日期倒序 */
  transactions: TransactionOut[];
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

export type TxnType = 'buy' | 'sell' | 'dividend' | 'deposit' | 'withdraw';

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
}

export interface MoverOut {
  code: string;
  name: string;
  totalPnl: number;
  totalPnlPct: number;
}

export interface PeriodReport {
  scope: string;
  startDate: string | null;
  endDate: string | null;
  startMv: number;
  endMv: number;
  deltaMv: number;
  deltaPnl: number;
  pnlRate: number;
  positiveDays: number;
  negativeDays: number;
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
  txnType: TxnType; // buy / sell / dividend
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
    fundType: '007',
    fundTypeLabel: '混合型',
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
    quotes.set(q.stockCode, { stockCode: q.stockCode, price: q.price, prevClose: q.prevClose });
  }
  const valuation = valueFund({ fundCode: f.code, officialNav: f.officialNav, holdings, quotes });
  return { holdings, quotes, valuation };
}

async function mockOverview(): Promise<OverviewResult> {
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
    positions.push({
      fund: meta,
      estNav: valuation.estNav,
      estChangePct: valuation.estChangePct,
      marketValue,
      dayPnl: valuation.estimated ? f.shares * (valuation.estNav - f.officialNav) : 0,
      totalPnl: marketValue - cost,
      totalPnlPct: cost > 0 ? (marketValue - cost) / cost : 0,
      estimated: valuation.estimated,
      disclosureType: f.disclosureType,
      disclosedWeightSum: valuation.disclosedWeightSum,
      confidence: valuation.confidence,
      penetrationEstChangePct: valuation.penetrationEstChangePct ?? null,
      consensusEstChangePct: valuation.consensusEstChangePct ?? null,
      delayNote: null,
    });
  }
  const summary = summarizePortfolio(summaryInput);
  positions.sort((a, b) => b.marketValue - a.marketValue);
  return { summary, positions, trading: isTradingNow(), marketSession: isTradingNow() ? 'intraday' : 'prev_day', asOf: new Date().toLocaleString('zh-CN') };
}

async function mockFundDetail(code: string): Promise<FundDetailResult> {
  const f = MOCK_FUNDS.find((x) => x.code === code);
  if (!f) throw new Error(`未找到基金 ${code}`);
  const { quotes, valuation } = runMockValuation(f);
  const quoteView = f.quotes.map((q) => ({
    stockCode: q.stockCode,
    stockName: q.stockName,
    price: q.price,
    prevClose: q.prevClose,
    priceReturn: quotes.get(q.stockCode)!.prevClose > 0 ? q.price / q.prevClose - 1 : 0,
  }));
  return { fund: mockFundToMeta(f), valuation, quotes: quoteView, trading: isTradingNow(), delayNote: null, transactions: [] };
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

function mockReport(_kind: '周' | '月'): PeriodReport {
  const today = new Date();
  const series: SnapshotPoint[] = [];
  let mv = 50000;
  for (let i = 30; i >= 0; i -= 1) {
    const d = new Date(today.getTime() - i * 86400000);
    const dayPnl = Math.round((Math.sin(i / 3) * 400));
    mv += dayPnl;
    series.push({
      date: d.toISOString().slice(0, 10),
      totalMarketValue: mv,
      totalCost: 48000,
      totalPnl: mv - 48000,
      dayPnl,
    });
  }
  const end = series[series.length - 1];
  const start = series[0];
  return {
    scope: '全部账户',
    startDate: start.date,
    endDate: end.date,
    startMv: start.totalMarketValue,
    endMv: end.totalMarketValue,
    deltaMv: end.totalMarketValue - start.totalMarketValue,
    deltaPnl: end.totalPnl - start.totalPnl,
    pnlRate: (end.totalPnl - start.totalPnl) / start.totalCost,
    positiveDays: series.filter((s) => s.dayPnl > 0).length,
    negativeDays: series.filter((s) => s.dayPnl < 0).length,
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
    out.push({ date: d.toISOString().slice(0, 10), totalMarketValue: mv, totalCost: 48000, totalPnl: mv - 48000, dayPnl });
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

// ============ 对外 API ============

export async function getOverview(platform: string | null = null): Promise<OverviewResult> {
  if (!isTauri) return mockOverview();
  return (await invoke('get_overview', { platform: platform ?? null })) as OverviewResult;
}

export async function getFundDetail(code: string): Promise<FundDetailResult> {
  if (!isTauri) return mockFundDetail(code);
  return (await invoke('get_fund_detail', { code })) as FundDetailResult;
}

export async function getStats(platform: string | null = null): Promise<StatsResult> {
  if (!isTauri) return mockStats();
  return (await invoke('get_stats', { platform: platform ?? null })) as StatsResult;
}

export async function importScreenshots(platform: string, _filePaths: string[]): Promise<ImportPreview> {
  if (!isTauri) return mockImport(platform);
  return (await invoke('import_screenshots', { platform, filePaths: _filePaths })) as ImportPreview;
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
): Promise<number> {
  if (!isTauri) return 1;
  return (await invoke('add_transaction', {
    txnType,
    fundCode,
    shares,
    amount,
    price,
    txnDate,
    txnTime: txnTime ?? null,
    note: note ?? null,
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
export async function getWeeklyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('周');
  return (await invoke('get_weekly_report')) as PeriodReport;
}
export async function getMonthlyReport(): Promise<PeriodReport> {
  if (!isTauri) return mockReport('月');
  return (await invoke('get_monthly_report')) as PeriodReport;
}
export async function getPnlCalendar(months = 3): Promise<SnapshotPoint[]> {
  if (!isTauri) return mockCalendar();
  return (await invoke('get_pnl_calendar', { months })) as SnapshotPoint[];
}

// 读取本地图片为 base64 data URL（后端读取，规避 asset 协议作用域限制）
export async function readImageDataUrl(path: string): Promise<string> {
  if (!isTauri) return '';
  return (await invoke('read_image_data_url', { path })) as string;
}

export async function refreshQuotes(): Promise<{ ok: boolean; at: string }> {
  if (!isTauri) return { ok: true, at: new Date().toLocaleString('zh-CN') };
  return (await invoke('refresh_quotes')) as { ok: boolean; at: string };
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
  return (await invoke('fetch_all_disclosures')) as FetchAllDisclosuresResult;
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
