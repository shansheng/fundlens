import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';
import FundDetailPage from './FundDetailPage';
import * as api from '../api';
import type { FundDetailResult } from '../api';

// 只覆盖需要控制的几个 API；其余（类型/常量）沿用真实实现。
vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    getFundDetail: vi.fn(),
    updatePosition: vi.fn(),
    getFundSeries: vi.fn(),
    refreshNavHistory: vi.fn(),
  };
});

const mockedGetFundDetail = vi.mocked(api.getFundDetail);
const mockedUpdatePosition = vi.mocked(api.updatePosition);
const mockedGetFundSeries = vi.mocked(api.getFundSeries);
const mockedRefreshNavHistory = vi.mocked(api.refreshNavHistory);

const AVG_COST = 4;
const SHARES = 1000;

function makeDetail(): FundDetailResult {
  return {
    fund: {
      code: '000001',
      name: '测试基金',
      platform: 'alipay',
      platformName: '支付宝',
      shares: SHARES,
      costAmount: AVG_COST * SHARES,
      avgCost: AVG_COST,
      officialNav: 4,
      reportPeriod: null,
      disclosureType: '',
      fundType: 'hybrid',
      fundTypeLabel: '混合型',
      valuationApplicable: true,
    },
    valuation: {
      fundCode: '000001',
      officialNav: 4,
      estNav: 4.1,
      estChangePct: 0.025,
      disclosureType: 'none',
      disclosedWeightSum: 0,
      holdings: [],
      estimated: true,
      reason: undefined,
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
    },
    quotes: [],
    marketSession: 'closed',
    valuationSource: 'local',
    delayNote: null,
    transactions: [],
    position: {
      shares: SHARES,
      avgCost: AVG_COST,
      costAmount: AVG_COST * SHARES,
      marketValue: SHARES * 4,
      totalPnl: 0,
      totalPnlPct: 0,
      dayPnl: 0,
      dayPnlPct: 0,
      dayPnlEst: 0,
      dayPnlPctEst: 0,
      estimated: true,
    },
  };
}

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/fund/000001']}>
      <Routes>
        <Route path="/fund/:code" element={<FundDetailPage />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe('FundDetailPage 持仓份额编辑', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(window, 'alert').mockImplementation(() => {});
    mockedGetFundDetail.mockResolvedValue(makeDetail());
    mockedGetFundSeries.mockResolvedValue({ navPoints: [], costPoints: [], txnMarkers: [] });
    mockedRefreshNavHistory.mockResolvedValue(undefined);
    mockedUpdatePosition.mockResolvedValue(undefined);
  });

  it('点击铅笔进入编辑态，输入新份额并保存后调用 updatePosition（保持单位成本不变）', async () => {
    renderPage();
    // 等待详情加载完成
    await screen.findByText('测试基金');

    // 初始展示份额 1,000
    expect(screen.getByText('1,000')).toBeInTheDocument();

    // 点击编辑（铅笔按钮，aria-label=编辑持仓份额）
    fireEvent.click(screen.getByRole('button', { name: '编辑持仓份额' }));

    // 编辑态出现数字输入框（aria-label=编辑持仓份额）
    const input = screen.getByLabelText('编辑持仓份额') as HTMLInputElement;
    expect(input).toBeTruthy();

    // 输入新份额 1200
    fireEvent.change(input, { target: { value: '1200' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    // 断言调用参数：code, 新份额, 持仓成本(= avgCost × 新份额 = 4 × 1200 = 4800), platform
    await waitFor(() => expect(mockedUpdatePosition).toHaveBeenCalledTimes(1));
    expect(mockedUpdatePosition).toHaveBeenCalledWith('000001', 1200, AVG_COST * 1200, 'alipay');

    // 保存后触发 reload（get_fund_detail 被再次调用）
    expect(mockedGetFundDetail).toHaveBeenCalledTimes(2);
  });

  it('非法输入（负数）不调用 updatePosition', async () => {
    renderPage();
    await screen.findByText('测试基金');

    fireEvent.click(screen.getByRole('button', { name: '编辑持仓份额' }));
    const input = screen.getByLabelText('编辑持仓份额') as HTMLInputElement;
    fireEvent.change(input, { target: { value: '-5' } });
    fireEvent.click(screen.getByRole('button', { name: '保存' }));

    await waitFor(() => expect(mockedUpdatePosition).not.toHaveBeenCalled());
    expect(mockedUpdatePosition).not.toHaveBeenCalled();
  });

  it('取消按钮退出编辑态且不调用 updatePosition', async () => {
    renderPage();
    await screen.findByText('测试基金');

    fireEvent.click(screen.getByRole('button', { name: '编辑持仓份额' }));
    screen.getByLabelText('编辑持仓份额');
    fireEvent.click(screen.getByRole('button', { name: '取消' }));

    // 退出编辑态：铅笔按钮重新出现，输入框消失
    expect(screen.getByRole('button', { name: '编辑持仓份额' })).toBeTruthy();
    expect(mockedUpdatePosition).not.toHaveBeenCalled();
  });
});
