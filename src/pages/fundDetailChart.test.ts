import { describe, it, expect } from 'vitest';
import { buildNavChartRows, evenlySpacedTicks, toTimeKey, DAY_MS, type NavChartRow } from './FundDetailPage';
import type { NavPoint } from '../api';

// 「净值走势图」的纯逻辑回归：
// ① 时间轴按日期等距（与数据点/成交点疏密无关）；② 买卖/分红圆点与净值线共用同一 (t, nav) → 必然落在线上。

const nav = (date: string, v: number, acc = 0): NavPoint => ({ date, nav: v, accNav: acc });

// 2026-09-07(一) 09-08(二) 09-09(三) 09-10(四) 09-11(五) 09-14(一)
const SEQ: NavPoint[] = [
  nav('2026-09-07', 1.0),
  nav('2026-09-08', 1.02),
  nav('2026-09-09', 0.99, 1.12),
  nav('2026-09-10', 1.05),
  nav('2026-09-11', 1.08),
  nav('2026-09-14', 1.03),
];

describe('toTimeKey', () => {
  it('解析为 UTC 毫秒，不受本地时区影响（GMT+8 下不跨日偏移）', () => {
    const t = toTimeKey('2026-09-14');
    expect(t).toBe(Date.UTC(2026, 8, 14));
    expect(new Date(t).getUTCDate()).toBe(14);
    expect(new Date(t).getUTCMonth()).toBe(8); // 0-based → 9 月
  });

  it('相邻自然日相差恰好 1 天（含跨月/跨年）', () => {
    expect(toTimeKey('2026-09-30') - toTimeKey('2026-09-29')).toBe(DAY_MS);
    expect(toTimeKey('2027-01-01') - toTimeKey('2026-12-31')).toBe(DAY_MS);
  });
});

describe('buildNavChartRows：买卖/分红圆点精确落在净值线上', () => {
  it('交易日买入 → 写入该行且取值等于该行 nav（与净值线同点）', () => {
    const rows = buildNavChartRows(SEQ, [{ date: '2026-09-09', txnType: 'buy', shares: 100 }]);
    const hit = rows.find((r) => r.buy !== null)!;
    expect(hit.date).toBe('2026-09-09');
    expect(hit.buy).toBe(hit.nav); // ← 不变量：标记值 = 所在行净值
    expect(hit.buy).toBe(0.99);
  });

  it('非交易日（周末）买入 → 归到当日前最近净值日，且取值等于该行 nav', () => {
    // 2026-09-12 是周六、09-13 是周日 → 应归到 09-11(五)
    const rows = buildNavChartRows(SEQ, [{ date: '2026-09-13', txnType: 'buy', shares: 100 }]);
    const hit = rows.find((r) => r.buy !== null)!;
    expect(hit.date).toBe('2026-09-11');
    expect(hit.buy).toBe(hit.nav);
  });

  it('买入/卖出/分红分别落到各自行，同一行可同时有买与分红', () => {
    const rows = buildNavChartRows(SEQ, [
      { date: '2026-09-08', txnType: 'buy', shares: 100 },
      { date: '2026-09-10', txnType: 'sell', shares: 50 },
      { date: '2026-09-10', txnType: 'dividend', shares: 0 },
    ]);
    expect(rows.find((r) => r.date === '2026-09-08')!.buy).toBe(1.02);
    const r10 = rows.find((r) => r.date === '2026-09-10')!;
    expect(r10.sell).toBe(r10.nav);
    expect(r10.div).toBe(r10.nav); // 分红 shares 恒为 0，但必须打点
  });

  it('shares≤0 的占位流水（buy/sell）不打点，避免虚假成交标记', () => {
    const rows = buildNavChartRows(SEQ, [
      { date: '2026-09-08', txnType: 'buy', shares: 0 },
      { date: '2026-09-09', txnType: 'sell', shares: 0 },
    ]);
    expect(rows.every((r) => r.buy === null && r.sell === null && r.div === null)).toBe(true);
  });

  it('早于区间首日的流水 → 归到首行，不会画到域外', () => {
    const rows = buildNavChartRows(SEQ, [{ date: '2026-08-01', txnType: 'buy', shares: 100 }]);
    const hit = rows.find((r) => r.buy !== null)!;
    expect(hit.date).toBe('2026-09-07');
    expect(hit.buy).toBe(hit.nav);
  });

  it('每个标记值的取值都等于同一行的 nav（全量校验不变量）', () => {
    const rows = buildNavChartRows(SEQ, [
      { date: '2026-09-07', txnType: 'buy', shares: 10 },
      { date: '2026-09-13', txnType: 'buy', shares: 10 },
      { date: '2026-09-09', txnType: 'sell', shares: 10 },
      { date: '2026-09-14', txnType: 'dividend', shares: 0 },
    ]);
    const markersOf = (r: NavChartRow) => [r.buy, r.sell, r.div];
    for (const r of rows) {
      for (const v of markersOf(r)) {
        if (v !== null) expect(v).toBe(r.nav);
      }
    }
  });

  it('空净值序列 → 空行（不抛错、不产出悬空标记）', () => {
    expect(buildNavChartRows([], [{ date: '2026-09-08', txnType: 'buy', shares: 100 }])).toEqual([]);
  });
});

describe('evenlySpacedTicks：时间轴按日期等距', () => {
  it('首末对齐窗口两端，且相邻刻度间隔完全相等', () => {
    const tMin = toTimeKey('2026-01-01');
    const tMax = toTimeKey('2026-12-31');
    const ticks = evenlySpacedTicks(tMin, tMax, 5);
    expect(ticks).toHaveLength(5);
    expect(ticks[0]).toBe(tMin);
    expect(ticks[4]).toBe(tMax);
    const gaps = ticks.slice(1).map((t, i) => t - ticks[i]);
    for (const g of gaps) expect(g).toBeCloseTo(gaps[0], 6);
  });

  it('刻度位置只由日期窗口决定，与数据点疏密无关（成交密集区不改变刻度）', () => {
    const sparse = buildNavChartRows([nav('2026-01-01', 1), nav('2026-12-31', 1.2)], []);
    // 3~6 月每天都有净值点（模拟成交/净值密集）
    const densePts: NavPoint[] = [nav('2026-01-01', 1)];
    for (let d = 1; d <= 180; d += 1) {
      densePts.push(nav(new Date(Date.UTC(2026, 2, d)).toISOString().slice(0, 10), 1 + d / 1000));
    }
    densePts.push(nav('2026-12-31', 1.2));
    const dense = buildNavChartRows(densePts, [{ date: '2026-04-01', txnType: 'buy', shares: 1 }]);
    const a = evenlySpacedTicks(sparse[0].t, sparse[sparse.length - 1].t, 5);
    const b = evenlySpacedTicks(dense[0].t, dense[dense.length - 1].t, 5);
    expect(b).toEqual(a); // 窗口相同 → 刻度相同
  });

  it('单点/退化窗口返回单刻度，不产生 NaN', () => {
    const t = toTimeKey('2026-09-14');
    expect(evenlySpacedTicks(t, t, 5)).toEqual([t]);
    expect(evenlySpacedTicks(t, t, 1)).toEqual([t]);
  });
});
