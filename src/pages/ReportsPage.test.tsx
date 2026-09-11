import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import ReportsPage from './ReportsPage';
import { ThemeProvider } from '../theme';
import * as api from '../api';
import type { PortfolioSummary } from '../types';

vi.mock('@tauri-apps/api/dialog', () => ({
  save: vi.fn(),
}));

vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    isMobile: false,
    getDailyReport: vi.fn(),
    getWeeklyReport: vi.fn(),
    getMonthlyReport: vi.fn(),
    getYearlyReport: vi.fn(),
    getPnlCalendar: vi.fn(),
    getOverview: vi.fn(),
    getOperationPnl: vi.fn(),
  };
});

const mockedDaily = vi.mocked(api.getDailyReport);
const mockedWeekly = vi.mocked(api.getWeeklyReport);
const mockedMonthly = vi.mocked(api.getMonthlyReport);
const mockedYearly = vi.mocked(api.getYearlyReport);
const mockedCalendar = vi.mocked(api.getPnlCalendar);
const mockedOverview = vi.mocked(api.getOverview);
const mockedOperation = vi.mocked(api.getOperationPnl);

function emptyReport(): api.PeriodReport {
  return {
    period: 'day',
    scope: '全部账户',
    startDate: null,
    endDate: null,
    startMv: 0,
    endMv: 0,
    deltaMv: 0,
    deltaPnl: 0,
    pnlRate: 0,
    estDeltaPnl: 0,
    estActDiff: 0,
    estPnlRate: 0,
    diffRate: 0,
    positiveDays: 0,
    negativeDays: 0,
    estPositiveDays: 0,
    estNegativeDays: 0,
    series: [],
    best: null,
    worst: null,
    hasHistory: false,
  };
}

function operationFixture(): api.OperationPnl {
  return {
    startDate: '2026-09-01',
    endDate: '2026-09-10',
    endNavDate: '2026-09-10',
    totalBuyPnl: 320.5,
    totalSellPnl: -150.2,
    totalPnl: 170.3,
    rows: [
      { fundCode: '003095', fundName: '中欧医疗健康混合', side: 'buy', buyPnl: 220.4, sellPnl: 0, endNav: 2.31, hasNav: true },
      { fundCode: '161725', fundName: '招商中证白酒', side: 'sell', buyPnl: 0, sellPnl: -150.2, endNav: 1.08, hasNav: true },
    ],
  };
}

describe('ReportsPage 区间操作收益', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedDaily.mockResolvedValue(emptyReport());
    mockedWeekly.mockResolvedValue(emptyReport());
    mockedMonthly.mockResolvedValue(emptyReport());
    mockedYearly.mockResolvedValue(emptyReport());
    mockedCalendar.mockResolvedValue([]);
    mockedOverview.mockResolvedValue({
      summary: {
        totalMarketValue: 50000,
        totalCost: 48000,
        totalPnl: 2000,
        totalPnlPct: 0.04,
        estDayPnl: 120,
        actDayPnl: 100,
        dayPnlPctEst: 0.0024,
        dayPnlPctAct: 0.002,
        risk: null,
      } as PortfolioSummary,
      positions: [],
      trading: false,
      marketSession: '',
      asOf: '',
    } as unknown as api.OverviewResult);
  });

  it('切换到「区间操作收益」tab，输入日期并计算后展示汇总与明细', async () => {
    mockedOperation.mockResolvedValue(operationFixture());

    render(
      <ThemeProvider>
        <ReportsPage />
      </ThemeProvider>,
    );

    // 切到操作收益 tab
    fireEvent.click(await screen.findByText('区间操作收益'));

    // 输入日期
    const startInput = screen.getByLabelText('起始日期') as HTMLInputElement;
    const endInput = screen.getByLabelText('结束日期') as HTMLInputElement;
    fireEvent.change(startInput, { target: { value: '2026-09-01' } });
    fireEvent.change(endInput, { target: { value: '2026-09-10' } });

    fireEvent.click(screen.getByRole('button', { name: /计算/ }));

    await waitFor(() => expect(mockedOperation).toHaveBeenCalledWith('2026-09-01', '2026-09-10'));

    // 汇总数字（含 +/- 符号）
    expect(await screen.findByText(/中欧医疗健康混合/)).toBeTruthy();
    expect(screen.getByText(/招商中证白酒/)).toBeTruthy();
    expect(screen.getByText(/明细（2 只基金）/)).toBeTruthy();
  });

  it('未选日期点计算时给出提示，不调用后端', async () => {
    render(
      <ThemeProvider>
        <ReportsPage />
      </ThemeProvider>,
    );
    fireEvent.click(await screen.findByText('区间操作收益'));
    fireEvent.click(screen.getByRole('button', { name: /计算/ }));

    await waitFor(() => expect(screen.getByText(/请选择起始与结束日期/)).toBeTruthy());
    expect(mockedOperation).not.toHaveBeenCalled();
  });

  it('结束日期早于起始日期时给出提示', async () => {
    render(
      <ThemeProvider>
        <ReportsPage />
      </ThemeProvider>,
    );
    fireEvent.click(await screen.findByText('区间操作收益'));
    const startInput = screen.getByLabelText('起始日期') as HTMLInputElement;
    const endInput = screen.getByLabelText('结束日期') as HTMLInputElement;
    fireEvent.change(startInput, { target: { value: '2026-09-10' } });
    fireEvent.change(endInput, { target: { value: '2026-09-01' } });
    fireEvent.click(screen.getByRole('button', { name: /计算/ }));

    await waitFor(() => expect(screen.getByText(/结束日期不能早于起始日期/)).toBeTruthy());
    expect(mockedOperation).not.toHaveBeenCalled();
  });
});
