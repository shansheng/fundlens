import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import LedgerPage, { classifyTxnRows } from './LedgerPage';
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

// 交易截图导入：单行缺陷不再打回整批（回归 2026-09-21「一堆需补基金代码就整批入不了库」）
describe('classifyTxnRows 部分导入规则', () => {
  const row = (over: Partial<Parameters<typeof classifyTxnRows>[0][number]> = {}) => ({
    txnType: 'buy' as const,
    date: '2026-09-17',
    time: '21:27',
    code: '007531',
    name: '华宝券商ETF联接C',
    shares: '100',
    amount: '100',
    price: '',
    confidence: 0.9,
    ...over,
  });

  it('全部合法时全部可导入，无需保留任何行', () => {
    const r = classifyTxnRows([row(), row({ code: '090018', name: '大成新锐产业混合A' })]);
    expect(r.items).toHaveLength(2);
    expect(r.skipped).toHaveLength(0);
    expect(r.keptRows).toHaveLength(0);
    expect(r.keepIdx).toHaveLength(0);
  });

  it('缺 6 位代码的行只留在预览里，其余照常导入（不再整批拒绝）', () => {
    const missing = row({ code: '', name: '广发汽车指数C' });
    const r = classifyTxnRows([row(), missing, row({ code: '501012', name: '汇添富中证中药ETF联接(LOF)C' })]);
    // 已解析的两条照常入库
    expect(r.items.map((i) => i.fundCode)).toEqual(['007531', '501012']);
    // 缺码的一条留在预览，且下标对准（前端据此渲染「需补代码」红标）
    expect(r.skipped).toHaveLength(1);
    expect(r.skipped[0]).toMatchObject({ row: 2, name: '广发汽车指数C', reason: '缺 6 位基金代码' });
    expect(r.keepIdx).toEqual([1]);
    expect(r.keptRows).toHaveLength(1);
    expect(r.keptRows[0].name).toBe('广发汽车指数C');
  });

  it('缺交易类型、金额无效同样只留该行，不影响其余行', () => {
    const r = classifyTxnRows([
      row({ txnType: '' }),
      row({ amount: '0' }),
      row({ code: '' }),
      row({ code: '000965' }),
    ]);
    expect(r.items.map((i) => i.fundCode)).toEqual(['000965']);
    expect(r.skipped.map((s) => s.reason)).toEqual(['缺少交易类型', '金额无效', '缺 6 位基金代码']);
    expect(r.keepIdx).toEqual([0, 1, 2]);
  });

  it('用户手工补全 6 位代码后该行即可导入', () => {
    const filled = row({ code: '004855', name: '广发汽车指数C' });
    const r = classifyTxnRows([filled]);
    expect(r.skipped).toHaveLength(0);
    expect(r.items[0].fundCode).toBe('004855');
  });

  it('买入/卖出无份额仍可导入，仅登记待回填（不阻断）', () => {
    const r = classifyTxnRows([row({ shares: '' })]);
    expect(r.items).toHaveLength(1);
    expect(r.missingShares).toHaveLength(1);
    expect(r.missingShares[0]).toEqual({ row: 1, code: '007531' });
  });
});
