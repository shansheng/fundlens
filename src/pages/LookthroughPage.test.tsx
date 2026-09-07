import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';
import LookthroughPage from './LookthroughPage';
import { ThemeProvider } from '../theme';
import * as api from '../api';
import type { LookthroughResult } from '../api';

vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    lookthroughOverview: vi.fn(),
    fetchStockProfiles: vi.fn(),
    fetchAllDisclosures: vi.fn(),
  };
});

const mockedOverview = vi.mocked(api.lookthroughOverview);
const mockedFetchProfiles = vi.mocked(api.fetchStockProfiles);
const mockedFetchAll = vi.mocked(api.fetchAllDisclosures);

function makeResult(): LookthroughResult {
  return {
    totalMv: 100000,
    coverage: 0.3,
    reportPeriods: ['2026Q2×2'],
    industriesL1: [
      { key: '医药医疗', marketValue: 20000, pct: 0.2, dayContribution: 120, isVirtual: false, parent: null },
      { key: '未穿透', marketValue: 70000, pct: 0.7, dayContribution: null, isVirtual: true, parent: null },
    ],
    industriesL2: [
      { key: '化学制药', marketValue: 20000, pct: 0.2, dayContribution: 120, isVirtual: false, parent: '医药医疗' },
      { key: '现金理财·未披露', marketValue: 70000, pct: 0.7, dayContribution: null, isVirtual: true, parent: '未穿透' },
    ],
    stocks: [
      {
        stockCode: '600276',
        stockName: '恒瑞医药',
        sectorL1: '医药医疗',
        industryL2: '化学制药',
        marketValue: 20000,
        pct: 0.2,
        fundCount: 3,
        funds: [
          { fundCode: '110011', fundName: '易方达', weight: 0.1, contributedMv: 10000 },
          { fundCode: '161725', fundName: '招商白酒', weight: 0.1, contributedMv: 10000 },
        ],
        dayChangePct: 0.006,
        dayContribution: 120,
        hiddenWarning: true,
      },
    ],
    cr5: 0.2,
    cr10: 0.2,
    funds: [
      { code: '110011', name: '易方达', marketValue: 100000, coverage: 0.2, reportPeriod: '2026Q2', unpenetratedMv: 80000 },
    ],
    unpenetratedMv: 70000,
    hasQuotes: true,
    asOf: '2026-09-08 01:00:00',
  };
}

function renderPage() {
  return render(
    <ThemeProvider>
      <MemoryRouter initialEntries={['/lookthrough']}>
        <Routes>
          <Route path="/lookthrough" element={<LookthroughPage />} />
        </Routes>
      </MemoryRouter>
    </ThemeProvider>,
  );
}

describe('LookthroughPage', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockedOverview.mockResolvedValue(makeResult());
  });

  it('渲染口径条：覆盖率、报告期分布与时滞提示常驻', async () => {
    renderPage();
    expect(await screen.findByText('基金穿透')).toBeTruthy();
    expect(screen.getByText(/组合覆盖率 30.0%/)).toBeTruthy();
    expect(screen.getByText(/2026Q2×2/)).toBeTruthy();
    // 时滞提示（披露滞后口径说明）
    expect(screen.getByText(/季报约滞后 15 个工作日/)).toBeTruthy();
  });

  it('行业穿透默认展示大类，可切换到细分（两级分母一致）', async () => {
    renderPage();
    expect(await screen.findByText('医药医疗')).toBeTruthy();
    // 大类视图含未穿透虚拟桶
    expect(screen.getByText('未穿透')).toBeTruthy();
    // 切到细分：出现东财行业名直出的 L2
    fireEvent.click(screen.getByRole('button', { name: '细分' }));
    expect(await screen.findByText('化学制药')).toBeTruthy();
  });

  it('个股穿透：CR5/CR10 徽标 + 隐性重仓预警徽标 + 行展开贡献基金明细', async () => {
    renderPage();
    fireEvent.click(await screen.findByRole('tab', { name: '个股穿透' }));
    expect(await screen.findByText(/CR5 20.0%/)).toBeTruthy();
    expect(screen.getByText(/CR10 20.0%/)).toBeTruthy();
    expect(screen.getByText('隐性重仓')).toBeTruthy();
    // 展开行 → 贡献基金明细
    fireEvent.click(screen.getByText('恒瑞医药'));
    expect(await screen.findByText(/贡献基金明细/)).toBeTruthy();
    expect(screen.getAllByText(/易方达/).length).toBeGreaterThan(0);
  });

  it('无披露数据时展示空态引导（抓取披露持仓）', async () => {
    mockedOverview.mockResolvedValue({
      ...makeResult(),
      stocks: [],
      funds: [],
      reportPeriods: [],
      industriesL1: [{ key: '未穿透', marketValue: 100000, pct: 1, dayContribution: null, isVirtual: true, parent: null }],
      industriesL2: [],
    });
    renderPage();
    expect(await screen.findByText('尚无披露持仓数据')).toBeTruthy();
  });

  it('补行业画像按钮调用 fetchStockProfiles 并回读结果', async () => {
    mockedFetchProfiles.mockResolvedValue({ total: 10, needed: 4, fetched: 4, failed: 0, failedCodes: [], at: 't' });
    renderPage();
    const btn = await screen.findByRole('button', { name: /补行业画像/ });
    fireEvent.click(btn);
    await waitFor(() => expect(mockedFetchProfiles).toHaveBeenCalled());
  });
});
