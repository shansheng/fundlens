import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import LedgerPage from './LedgerPage';
import * as api from '../api';

// 覆盖需要控制的 API；其余沿用真实实现（PLATFORMS 等常量）。
vi.mock('../api', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api')>();
  return {
    ...actual,
    isTauri: true,
    listTransactions: vi.fn(),
    addTransaction: vi.fn(),
    getFundDetail: vi.fn(),
    importTransactions: vi.fn(),
    importTxnScreenshots: vi.fn(),
    deleteTransaction: vi.fn(),
    readImageDataUrl: vi.fn(),
  };
});

const mockedList = vi.mocked(api.listTransactions);
const mockedAdd = vi.mocked(api.addTransaction);
const mockedGetDetail = vi.mocked(api.getFundDetail);

function renderPage() {
  return render(
    <MemoryRouter>
      <LedgerPage />
    </MemoryRouter>,
  );
}

describe('LedgerPage 记一笔 · 平台透传（记账 bug 回归）', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.spyOn(window, 'alert').mockImplementation(() => {});
    mockedList.mockResolvedValue([]);
    mockedAdd.mockResolvedValue(1);
    mockedGetDetail.mockResolvedValue({
      fund: { code: '110011', name: 'X', platform: '', platformName: '' } as never,
      valuation: {} as never,
      quotes: [],
      marketSession: 'closed',
      valuationSource: 'local',
      delayNote: null,
      transactions: [],
      position: { platform: '' } as never,
    } as never);
  });

  it('默认平台为支付宝，保存流水时把平台透传给 addTransaction（不再落到空平台）', async () => {
    renderPage();
    // 记一笔表单
    fireEvent.change(screen.getByPlaceholderText('如 110011'), { target: { value: '110011' } });
    fireEvent.change(screen.getByPlaceholderText('份'), { target: { value: '100' } });
    fireEvent.change(screen.getByPlaceholderText('留空自动算'), { target: { value: '500' } });

    fireEvent.click(screen.getByRole('button', { name: '保存流水' }));

    await waitFor(() => expect(mockedAdd).toHaveBeenCalledTimes(1));
    const args = mockedAdd.mock.calls[0];
    // 第 9 个参数为 platform
    expect(args[8]).toBe('alipay');
    // 其它关键参数：类型/代码/份额/金额
    expect(args[0]).toBe('buy');
    expect(args[1]).toBe('110011');
    expect(args[2]).toBe(100);
    expect(args[3]).toBe(500);
  });

  it('切换平台选择后，保存流水透传所选平台', async () => {
    renderPage();
    fireEvent.change(screen.getByDisplayValue('支付宝'), { target: { value: 'jd_finance' } });

    fireEvent.change(screen.getByPlaceholderText('如 110011'), { target: { value: '110011' } });
    fireEvent.change(screen.getByPlaceholderText('份'), { target: { value: '100' } });
    fireEvent.change(screen.getByPlaceholderText('留空自动算'), { target: { value: '500' } });

    fireEvent.click(screen.getByRole('button', { name: '保存流水' }));

    await waitFor(() => expect(mockedAdd).toHaveBeenCalledTimes(1));
    expect(mockedAdd.mock.calls[0][8]).toBe('jd_finance');
  });

  it('输入基金代码失焦后，自动回退到该基金已有持仓平台', async () => {
    mockedGetDetail.mockResolvedValue({
      fund: { code: '000001', name: 'Y', platform: 'jd_finance', platformName: '京东金融' } as never,
      valuation: {} as never,
      quotes: [],
      marketSession: 'closed',
      valuationSource: 'local',
      delayNote: null,
      transactions: [],
      position: { platform: 'jd_finance' } as never,
    } as never);

    renderPage();
    const codeInput = screen.getByPlaceholderText('如 110011');
    fireEvent.change(codeInput, { target: { value: '000001' } });
    fireEvent.blur(codeInput);

    // 平台下拉值应自动切到京东金融
    await waitFor(() => {
      const sel = screen.getByDisplayValue('京东金融') as HTMLSelectElement;
      expect(sel.value).toBe('jd_finance');
    });
  });
});
