import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import OverviewPage from './OverviewPage';
import * as api from '../api';
import type { OverviewResult, PositionRow, FundMeta } from '../api';

// 仅 mock 需要的 api 调用；其余沿用真实实现（类型/常量）。
vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    getOverview: vi.fn(),
    gridTodaySignals: vi.fn().mockResolvedValue([]),
    deleteFund: vi.fn().mockResolvedValue(undefined),
  };
});

const mockedGetOverview = vi.mocked(api.getOverview);

function makeFund(code: string): FundMeta {
  return {
    code,
    name: `基金${code}`,
    platform: 'alipay',
    platformName: '支付宝',
    shares: 100,
    costAmount: 100,
    avgCost: 1,
    officialNav: 1,
    reportPeriod: null,
    disclosureType: 'top10',
    valuationApplicable: true,
  };
}

function makePos(code: string, opts: Partial<PositionRow> = {}): PositionRow {
  return {
    fund: makeFund(code),
    estNav: 1,
    estChangePct: 0,
    marketValue: 100,
    dayPnl: 0,
    dayPnlPct: 0,
    dayPnlEst: 0,
    dayPnlPctEst: 0,
    dayPnlAct: 0,
    dayPnlPctAct: 0,
    hasDayActual: false,
    dayIsToday: false,
    navDate: '',
    totalPnl: 0,
    totalPnlPct: 0,
    estimated: true,
    disclosureType: 'top10',
    disclosedWeightSum: 1,
    valuationMethod: null,
    delayNote: null,
    ...opts,
  };
}

function baseSummary(over: Partial<OverviewResult['summary']> = {}): OverviewResult['summary'] {
  return {
    totalMarketValue: 10000,
    totalCost: 9000,
    totalPnl: 1000,
    totalPnlPct: 0.1,
    estDayPnl: 0,
    actDayPnl: 0,
    dayPnlPctEst: 0,
    dayPnlPctAct: 0,
    risk: null,
    ...over,
  } as OverviewResult['summary'];
}

function fixture(over: Partial<OverviewResult> = {}): OverviewResult {
  return {
    summary: baseSummary(),
    positions: [],
    trading: false,
    marketSession: 'closed',
    asOf: '2026-09-12 10:00',
    ...over,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockedGetOverview.mockResolvedValue(fixture());
});

describe('OverviewPage 头条：休市不隐藏当日收益', () => {
  it('closed（休市）：头条显示上一交易日实际收益（非 —），并带日期角标「09-11」', async () => {
    mockedGetOverview.mockResolvedValue(
      fixture({
        summary: baseSummary({ actDayPnl: 123.45, lastNavDate: '2026-09-11' }),
        marketSession: 'closed',
        positions: [
          makePos('000001', {
            hasDayActual: true,
            dayIsToday: false,
            dayPnlAct: 123.45,
            dayPnlPctAct: 0.01,
            lastDayPnlEst: 9.5,
            lastNavDate: '2026-09-11',
          }),
        ],
      }),
    );
    render(
      <MemoryRouter>
        <OverviewPage />
      </MemoryRouter>,
    );
    await screen.findByText('持仓总览');

    // 头条实际收益不再是 —，而是上一交易日实际 +¥123.45
    expect(screen.getByText('+¥123.45')).toBeTruthy();
    // 角标「09-11」存在（日期口径：标签只显示日期，不带「上一交易日」前缀）
    expect(screen.getAllByText('09-11').length).toBeGreaterThanOrEqual(1);
    // 旧误导文案不应出现
    expect(screen.queryByText(/当日收益暂不展示/)).toBeNull();
    // 头条「上一交易日实际收益」tile 内不含 —
    const actualTile = screen.getByText('实际收益 09-11').parentElement as HTMLElement;
    expect(within(actualTile).queryByText('—')).toBeNull();
  });

  it('closed 但持仓无 lastDayPnlEst：头条实际仍显示，估算收益 tile 显示 —（不编造）', async () => {
    mockedGetOverview.mockResolvedValue(
      fixture({
        summary: baseSummary({ actDayPnl: 80, lastNavDate: '2026-09-11' }),
        marketSession: 'closed',
        positions: [makePos('000001', { hasDayActual: true, dayIsToday: false, dayPnlAct: 80 })],
      }),
    );
    render(
      <MemoryRouter>
        <OverviewPage />
      </MemoryRouter>,
    );
    await screen.findByText('持仓总览');

    expect(screen.getByText('+¥80.00')).toBeTruthy();
    const estTile = screen.getByText('估算收益 09-11').parentElement as HTMLElement;
    expect(within(estTile).getByText('—')).toBeTruthy();
  });

  it('closed 且完全不带新字段（lastNavDate/lastDayPnlEst 均无）：基础渲染不崩，实际仍显示，休市横幅在', async () => {
    mockedGetOverview.mockResolvedValue(
      fixture({
        summary: baseSummary({ actDayPnl: 80 }),
        marketSession: 'closed',
        positions: [makePos('000001', { hasDayActual: true, dayIsToday: false, dayPnlAct: 80 })],
      }),
    );
    render(
      <MemoryRouter>
        <OverviewPage />
      </MemoryRouter>,
    );
    await screen.findByText('持仓总览');

    expect(screen.getByText('+¥80.00')).toBeTruthy();
    expect(screen.getByText(/休市中/)).toBeTruthy();
    // 无日期字段时安全回退：头条不出现带 MM-DD 的日期角标
    expect(screen.queryByText('09-11')).toBeNull();
  });

  it('intraday（盘中）：当日估算收益 tile 显示当日实时估算，当日实际收益 tile 显示 —，无上一交易日角标', async () => {
    mockedGetOverview.mockResolvedValue(
      fixture({
        summary: baseSummary({ estDayPnl: 50, actDayPnl: 0 }),
        marketSession: 'intraday',
        positions: [makePos('000001', { dayPnlEst: 50, dayPnlPctEst: 0.005 })],
      }),
    );
    render(
      <MemoryRouter>
        <OverviewPage />
      </MemoryRouter>,
    );
    await screen.findByText('持仓总览');

    // 盘中估算 +¥50.00 正常展示（头条与持仓表各一处，故 ≥1）
    expect(screen.getAllByText('+¥50.00').length).toBeGreaterThanOrEqual(1);
    // 当日实际收益 tile 仍显示 —
    const actualTile = screen.getByText('当日实际收益').parentElement as HTMLElement;
    expect(within(actualTile).getByText('—')).toBeTruthy();
    // 当日口径，不应带「上一交易日」日期角标（标签口径已改为纯日期，此处守卫旧前缀不再出现）
    expect(screen.queryByText(/上一交易日/)).toBeNull();
  });
});
