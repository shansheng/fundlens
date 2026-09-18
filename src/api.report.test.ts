import { describe, expect, it } from 'vitest';
import { getWeeklyReport } from './api';

// 报表「估算 vs 实际」偏差的口径护栏。
//
// jsdom 下没有 __TAURI_INTERNALS__ → isTauri=false → getWeeklyReport 走 mock 通道，
// 正是「浏览器预览」那条路。
//
// 回归（2026-09-19）：mock 曾按 `estDeltaPnl - deltaPnl` 计算偏差，而后端
// commands.rs 实现与其单测按 `est_delta_pnl - act_delta_pnl`（两端同取"窗口内
// 逐日盈亏之和"）。deltaPnl 是期初/期末**存量差**，比逐日和少算首日一项
// ⇒ 浏览器预览与桌面端显示的"估算偏差"是两个不同的数。
describe('报表 mock 通道：偏差口径与后端一致', () => {
  it('estActDiff 取逐日实际和口径，而非 deltaPnl 存量差', async () => {
    const r = await getWeeklyReport();

    // 旧口径（错误）会得到这个值：与 deltaPnl 一起算出的偏差
    const legacyWrong = r.estDeltaPnl - r.deltaPnl;
    expect(
      Math.abs(r.estActDiff - legacyWrong),
      '偏差若与"estDeltaPnl − deltaPnl"相等，说明又退回存量差口径',
    ).toBeGreaterThan(1);

    // 自洽：偏差率与估算收益率都应相对**同一期初成本**（由 pnlRate 反推）
    const cost = r.deltaPnl / r.pnlRate;
    expect(r.diffRate).toBeCloseTo(r.estActDiff / cost, 6);
    expect(r.estPnlRate).toBeCloseTo(r.estDeltaPnl / cost, 6);
  });
});
