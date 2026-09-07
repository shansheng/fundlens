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

function renderTable(positions: PositionRow[]) {
  return render(
    <MemoryRouter>
      <PositionTable
        positions={positions}
        marketSession="post_close"
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

  it('「当日」列排序按单元格实际展示口径（有实际→按实际涨跌幅，而非估算）', () => {
    // A：有实际口径，实际 +2%（列显示 +2%），估算 +0.5%
    // B：有实际口径，实际 -1%（列显示 -1%），估算 +3%
    // 升序按展示值（实际）：-1% 在前 → B, A；若错误按估算排序会得到 A(0.5%), B(3%)。
    const a = makePos('000021', 100, {
      hasDayActual: true,
      dayIsToday: true,
      dayPnlPctAct: 0.02,
      dayPnlPctEst: 0.005,
    });
    const b = makePos('000022', 100, {
      hasDayActual: true,
      dayIsToday: false,
      dayPnlPctAct: -0.01,
      dayPnlPctEst: 0.03,
    });
    renderTable([a, b]);
    fireEvent.click(screen.getByText('当日').closest('th')!);
    expect(rowCodes()).toEqual(['000022', '000021']);

    fireEvent.click(screen.getByText('当日').closest('th')!); // 降序 → +2% 在前
    expect(rowCodes()).toEqual(['000021', '000022']);
  });
});

describe('PositionTable 当日实际/上次/估算 标签与隐藏', () => {
  function rowOf(code: string): HTMLElement {
    return screen.getByText(code).closest('tr')!;
  }

  it('盘后 + 当日官方净值已确认 → 当日列显示「实际」', () => {
    renderTable([
      makePos('000001', 100, {
        delayNote: null,
        hasDayActual: true,
        dayIsToday: true,
        dayPnlAct: 12,
        dayPnlPctAct: 0.01,
        dayPnlEst: 5,
        dayPnlPctEst: 0.004,
      }),
    ]);
    const row = rowOf('000001');
    expect(within(row).getByText('实际')).toBeTruthy();
    expect(within(row).queryByText('估算')).toBeNull();
    expect(within(row).queryByText('上次')).toBeNull();
  });

  it('开盘前/周末/休盘（上一次净值）→ 当日列只显示净值月日（MMDD，无「上次」前缀）', () => {
    renderTable([
      makePos('000002', 100, {
        delayNote: null,
        hasDayActual: true,
        dayIsToday: false,
        navDate: '2026-08-25',
        dayPnlAct: 12,
        dayPnlPctAct: 0.01,
        dayPnlEst: 5,
        dayPnlPctEst: 0.004,
      }),
    ]);
    const row = rowOf('000002');
    expect(within(row).getByText('0825')).toBeTruthy();
    expect(within(row).queryByText('上次')).toBeNull();
    expect(within(row).queryByText('实际')).toBeNull();
    expect(within(row).queryByText('估算')).toBeNull();
  });

  it('上一次净值但无净值日期（navDate 为空）→ 回退显示「上次」', () => {
    renderTable([
      makePos('000002b', 100, {
        delayNote: null,
        hasDayActual: true,
        dayIsToday: false,
        navDate: '',
        dayPnlAct: 12,
        dayPnlPctAct: 0.01,
      }),
    ]);
    const row = rowOf('000002b');
    expect(within(row).getByText('上次')).toBeTruthy();
  });

  it('QDII 海外净值（hasDayActual=false）显示「估算」', () => {
    renderTable([
      makePos('000003', 100, {
        delayNote: 'T+1·海外净值',
        hasDayActual: false,
        dayIsToday: false,
        dayPnlEst: 5,
        dayPnlPctEst: 0.004,
      }),
    ]);
    const row = rowOf('000003');
    expect(within(row).getByText('估算')).toBeTruthy();
    expect(within(row).queryByText('实际')).toBeNull();
    expect(within(row).queryByText('上次')).toBeNull();
  });

  it('盘中（intraday）默认 hasDayActual=false → 显示「估算」', () => {
    render(
      <MemoryRouter>
        <PositionTable
          positions={[makePos('000004', 100, { hasDayActual: false, dayPnlEst: 5, dayPnlPctEst: 0.004 })]}
          marketSession="intraday"
          onDelete={() => {}}
        />
      </MemoryRouter>,
    );
    const row = rowOf('000004');
    expect(within(row).getByText('估算')).toBeTruthy();
    expect(within(row).queryByText('实际')).toBeNull();
  });

  it('closed（周末/休盘）：当日列不再隐藏，展示上一次净值（标「上次」），估算收益/估算收益率两列仍隐藏（—）', () => {
    render(
      <MemoryRouter>
        <PositionTable
          positions={[
            makePos('000005', 100, {
              hasDayActual: true,
              dayIsToday: false,
              dayPnlAct: 12,
              dayPnlPctAct: 0.01,
            }),
          ]}
          marketSession="closed"
          onDelete={() => {}}
        />
      </MemoryRouter>,
    );
    const row = rowOf('000005');
    // 当日列（涨跌幅）显示上一次净值实际，标「上次」
    expect(within(row).getByText('上次')).toBeTruthy();
    expect(within(row).queryByText('实际')).toBeNull();
    expect(within(row).queryByText('估算')).toBeNull();
    // 拆列后「估算收益」「估算收益率」两列因休市均隐藏 → 整行恰 2 个「—」
    expect(within(row).getAllByText('—').length).toBe(2);
  });

  it('「估算收益」「估算收益率」两列已拆分，且恒定估算口径（不受 hasDayActual 影响，无角标切换）', () => {
    renderTable([
      makePos('000006', 100, { hasDayActual: true, dayIsToday: true, dayPnlEst: 5, dayPnlPctEst: 0.004 }),
    ]);
    // 列名已从「当日估算收益」拆为「估算收益」+「估算收益率」
    expect(screen.getByText('估算收益')).toBeTruthy();
    expect(screen.getByText('估算收益率')).toBeTruthy();
    expect(screen.queryByText('当日估算收益')).toBeNull();
    const row = rowOf('000006');
    // hasDayActual=true 时「当日」列显示「实际」，但拆分后的估算列不含任何角标（无「实际」/「估算」切换），
    // 故整行仅出现 1 个「实际」角标（在当日列）。
    expect(within(row).getAllByText('实际').length).toBe(1);
    expect(within(row).queryByText('估算')).toBeNull();
  });
});
