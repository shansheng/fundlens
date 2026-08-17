import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import PositionTable from './PositionTable';
import { type PositionRow, type FundMeta } from '../api';

function makeFund(code: string, marketValue: number): FundMeta {
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

function makePos(code: string, marketValue: number, opts: Partial<PositionRow> = {}): PositionRow {
  return {
    fund: makeFund(code, marketValue),
    estNav: 1,
    estChangePct: 0,
    marketValue,
    dayPnl: 0,
    dayPnlPct: 0,
    dayPnlEst: 0,
    dayPnlPctEst: 0,
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

function renderTable(positions: PositionRow[]) {
  return render(
    <MemoryRouter>
      <PositionTable
        positions={positions}
        totalMarketValue={600}
        marketSession="post_close"
        daySession={{ label: '实际', cls: '' }}
        onDelete={() => {}}
      />
    </MemoryRouter>,
  );
}

function rowCodes(): string[] {
  const rows = screen.getAllByRole('row').slice(1); // 跳过表头
  return rows.map((r) => within(r).getByText(/^\d{6}$/).textContent!);
}

beforeEach(() => localStorage.clear());

describe('PositionTable 表头排序', () => {
  const a = makePos('000001', 100);
  const b = makePos('000002', 300);
  const c = makePos('000003', 200);

  it('默认（无排序）保持传入顺序', () => {
    renderTable([a, b, c]);
    expect(rowCodes()).toEqual(['000001', '000002', '000003']);
  });

  it('点击数值列表头：升序 → 降序 → 恢复默认 三态循环', () => {
    renderTable([a, b, c]);
    const th = screen.getByText('市值').closest('th')!;

    fireEvent.click(th); // 第一次：升序（市值小→大）
    expect(rowCodes()).toEqual(['000001', '000003', '000002']);

    fireEvent.click(th); // 第二次：降序（市值大→小）
    expect(rowCodes()).toEqual(['000002', '000003', '000001']);

    fireEvent.click(th); // 第三次：恢复默认
    expect(rowCodes()).toEqual(['000001', '000002', '000003']);
  });

  it('切换不同列会重置为按新列升序', () => {
    renderTable([a, b, c]);
    fireEvent.click(screen.getByText('市值').closest('th')!); // 按市值升序
    expect(rowCodes()).toEqual(['000001', '000003', '000002']);
    fireEvent.click(screen.getByText('累计盈亏').closest('th')!); // 切到累计盈亏（均为0）→ 保持稳定顺序即传入顺序
    expect(rowCodes()).toEqual(['000001', '000002', '000003']);
  });

  it('排序状态持久化到 localStorage', () => {
    renderTable([a, b, c]);
    fireEvent.click(screen.getByText('市值').closest('th')!);
    const saved = JSON.parse(localStorage.getItem('fundlens.overview.sort')!);
    expect(saved).toEqual({ key: 'marketValue', dir: 'asc' });

    fireEvent.click(screen.getByText('市值').closest('th')!); // 降序
    expect(JSON.parse(localStorage.getItem('fundlens.overview.sort')!).dir).toBe('desc');
  });

  it('持仓占比按市值占比正确降序（派生字段排序）', () => {
    renderTable([a, b, c]); // 占比 100/600 < 200/600 < 300/600
    fireEvent.click(screen.getByText('持仓占比').closest('th')!); // 升序
    expect(rowCodes()).toEqual(['000001', '000003', '000002']);
    fireEvent.click(screen.getByText('持仓占比').closest('th')!); // 降序
    expect(rowCodes()).toEqual(['000002', '000003', '000001']);
  });

  it('带符号列排序正确分离正/负（降序盈在前、升序亏在前）', () => {
    // 混合正负累计盈亏：+500 / -200 / +100 / -50
    const p1 = makePos('000011', 500, { totalPnl: 500 });
    const p2 = makePos('000022', 200, { totalPnl: -200 });
    const p3 = makePos('000033', 100, { totalPnl: 100 });
    const p4 = makePos('000044', 50, { totalPnl: -50 });
    renderTable([p1, p2, p3, p4]);
    const th = screen.getByText('累计盈亏').closest('th')!;

    fireEvent.click(th); // 升序：值最小在前（亏/负在最前）→ -200, -50, +100, +500
    expect(rowCodes()).toEqual(['000022', '000044', '000033', '000011']);

    fireEvent.click(th); // 降序：盈（正）在前，由大到小 → +500, +100, -50, -200
    expect(rowCodes()).toEqual(['000011', '000033', '000044', '000022']);
  });
});
