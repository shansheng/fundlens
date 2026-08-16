// 估值引擎纯函数单测（与 Rust 端 valuation.rs 保持算法一致）
import { describe, it, expect } from 'vitest';
import { valueFund, summarizePortfolio, type DisclosedHolding, type StockQuote } from './engine';

const hold = (stockCode: string, weight: number): DisclosedHolding => ({
  stockCode,
  stockName: stockCode,
  weight,
  reportPeriod: '2026Q1',
  disclosureType: 'top10',
});

const quote = (stockCode: string, price: number, prevClose: number): StockQuote => ({
  stockCode,
  price,
  prevClose,
});

describe('valueFund', () => {
  it('无披露持仓时不估算（estimated=false），净值与涨跌幅保持基准', () => {
    const r = valueFund({ fundCode: 'X', officialNav: 2, holdings: [], quotes: new Map() });
    expect(r.estimated).toBe(false);
    expect(r.estNav).toBe(2);
    expect(r.estChangePct).toBe(0);
    expect(r.reason).toBeTruthy();
    expect(r.disclosedWeightSum).toBe(0);
  });

  it('官方净值非正时不估算', () => {
    const r = valueFund({
      fundCode: 'X',
      officialNav: 0,
      holdings: [hold('A', 0.5)],
      quotes: new Map(),
    });
    expect(r.estimated).toBe(false);
  });

  it('仅披露部分：按个股收益加权，未披露部分用基准近似', () => {
    const quotes = new Map<string, StockQuote>([
      ['A', quote('A', 10, 10)], // 0% 收益
      ['B', quote('B', 11, 10)], // +10% 收益
    ]);
    const r = valueFund({
      fundCode: 'X',
      officialNav: 2,
      holdings: [hold('A', 0.5), hold('B', 0.3)],
      quotes,
    });
    expect(r.estimated).toBe(true);
    expect(r.disclosedWeightSum).toBeCloseTo(0.8);
    expect(r.benchmarkWeight).toBeCloseTo(0.2);
    // portfolioReturn = 0.5*0 + 0.3*0.1 = 0.03 → estNav = 2*1.03 = 2.06
    expect(r.estNav).toBeCloseTo(2.06);
    expect(r.estChangePct).toBeCloseTo(0.03);
  });

  it('提供基准行情时，未披露部分按基准涨跌幅近似', () => {
    const quotes = new Map<string, StockQuote>([
      ['A', quote('A', 10, 10)],
      ['B', quote('B', 11, 10)],
    ]);
    const benchmark = quote('000300', 103, 100); // +3%
    const r = valueFund({
      fundCode: 'X',
      officialNav: 2,
      holdings: [hold('A', 0.5), hold('B', 0.3)],
      quotes,
      benchmark,
    });
    expect(r.benchmarkReturn).toBeCloseTo(0.03);
    expect(r.benchmarkName).toBe('沪深300');
    // portfolioReturn = 0.03 + 0.2*0.03 = 0.036 → estNav = 2.072
    expect(r.estNav).toBeCloseTo(2.072);
  });

  it('缺行情的持仓贡献按 0，但仍计入披露占比', () => {
    const quotes = new Map<string, StockQuote>([['B', quote('B', 11, 10)]]); // 仅 B 有行情
    const r = valueFund({
      fundCode: 'X',
      officialNav: 2,
      holdings: [hold('A', 0.5), hold('B', 0.3)],
      quotes,
    });
    expect(r.disclosedWeightSum).toBeCloseTo(0.8);
    // A 缺行情贡献 0，仅 B 贡献 0.3*0.1 = 0.03
    expect(r.estNav).toBeCloseTo(2.06);
  });
});

describe('summarizePortfolio', () => {
  it('聚合市值/成本/盈亏与当日估算收益', () => {
    const s = summarizePortfolio([
      { fundCode: '1', shares: 100, avgCost: 1.0, estNav: 1.1, estimated: true, officialNav: 1.0 },
      { fundCode: '2', shares: 200, avgCost: 2.0, estNav: 2.0, estimated: false, officialNav: 1.9 },
    ]);
    // 基金1 估算市值 110，成本 100，盈亏 +10，当日 +10
    // 基金2 非估算→用官方净值 1.9，市值 380，成本 400，盈亏 -20，当日 0
    expect(s.totalMarketValue).toBeCloseTo(490);
    expect(s.totalCost).toBeCloseTo(500);
    expect(s.totalPnl).toBeCloseTo(-10);
    expect(s.totalPnlPct).toBeCloseTo(-0.02);
    expect(s.estDayPnl).toBeCloseTo(10);
    expect(s.positions).toHaveLength(2);

    const p1 = s.positions.find((p) => p.fundCode === '1')!;
    expect(p1.marketValue).toBeCloseTo(110);
    expect(p1.pnl).toBeCloseTo(10);
    expect(p1.pnlPct).toBeCloseTo(0.1);
    expect(p1.dayPnl).toBeCloseTo(10);

    const p2 = s.positions.find((p) => p.fundCode === '2')!;
    expect(p2.marketValue).toBeCloseTo(380);
    expect(p2.dayPnl).toBeCloseTo(0);
  });
});
