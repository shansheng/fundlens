import { describe, it, expect } from 'vitest';
import { estDisplay } from './estDisplay';

describe('estDisplay 估值数据日期口径', () => {
  it('盘中不显示日期标签（由「当日」列承担「当日估算」）', () => {
    const r = estDisplay(
      {
        dayPnlEst: 120,
        dayPnlPctEst: 0.012,
        marketValue: 10000,
        dayPnl: 80,
        navDate: '2026-10-29',
        estDate: '2026-10-30',
        lastNavDate: '2026-10-29',
      },
      'intraday',
    );
    expect(r.val).toBe(120);
    expect(r.fromLive).toBe(true);
    expect(r.date).toBeNull();
  });

  it('盘后用估算数据日期（estDate），不是官方净值日（navDate）', () => {
    // 回归（2026-10-30 用户报障）：10-30 周五 15:39 收盘后，官方净值仍是 10-29
    // （当日净值当晚才发布），行情已是 10-30 收盘 ⇒ 估值描述 10-30。
    // 旧实现取 navDate ⇒ 标签误打「10-29」。
    const r = estDisplay(
      {
        dayPnlEst: 120,
        dayPnlPctEst: 0.012,
        marketValue: 10000,
        dayPnl: 80,
        navDate: '2026-10-29',
        estDate: '2026-10-30',
        lastNavDate: '2026-10-29',
      },
      'post_close',
    );
    expect(r.val).toBe(120);
    expect(r.fromLive).toBe(true);
    expect(r.date).toBe('2026-10-30');
  });

  it('estDate 缺失时回退 navDate（老数据兼容，不出空标签）', () => {
    const r = estDisplay(
      {
        dayPnlEst: 120,
        dayPnlPctEst: 0.012,
        marketValue: 10000,
        dayPnl: 80,
        navDate: '2026-10-29',
        lastNavDate: '2026-10-28',
      },
      'post_close',
    );
    expect(r.date).toBe('2026-10-29');
  });

  it('实时值为 0 时回填种子，日期取种子日 lastNavDate', () => {
    const r = estDisplay(
      {
        dayPnlEst: 0,
        dayPnlPctEst: 0,
        lastDayPnlEst: 55,
        marketValue: 10000,
        dayPnl: 80,
        navDate: '2026-10-29',
        estDate: '',
        lastNavDate: '2026-10-28',
      },
      'closed',
    );
    expect(r.val).toBe(55);
    expect(r.fromLive).toBe(false);
    expect(r.date).toBe('2026-10-28');
    // 收益率 = 种子估算 ÷ (市值 − 当日实际)
    expect(r.pct).toBeCloseTo(55 / (10000 - 80), 10);
  });

  it('实时与回填都无数据时返回 null（显示 —），不谎报 0', () => {
    const r = estDisplay(
      {
        dayPnlEst: 0,
        dayPnlPctEst: 0,
        marketValue: 10000,
        dayPnl: 80,
        navDate: '2026-10-29',
        estDate: '',
      },
      'closed',
    );
    expect(r.val).toBeNull();
    expect(r.pct).toBeNull();
  });
});
