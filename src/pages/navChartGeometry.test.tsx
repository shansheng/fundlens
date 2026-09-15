/**
 * 净值走势图「几何」回归：断言买入/卖出/分红圆点与净值线**同一个数据点**的坐标完全重合。
 *
 * 为什么需要这个测试：组件使用的 recharts Scatter 取 x 的规则是
 *   xAxisDataKey = isNil(xAxis.dataKey) ? item.props.dataKey : xAxis.dataKey
 * 若 XAxis 用分类轴（dataKey="date"），线与点的取坐标路径可能不一致 → 图上圆点偏离曲线。
 * 本测试直接渲染真实组件并比对 SVG 坐标，任何回归（换回分类轴、误改 dataKey）都会失败。
 */
import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import { NavChart, buildNavChartRows, evenlySpacedTicks, type NavChartColors } from './FundDetailPage';
import type { NavPoint } from '../api';

const COLORS: NavChartColors = {
  surface: 'rgb(255,255,255)',
  border: 'rgb(220,220,220)',
  foreground: 'rgb(20,20,20)',
  muted: 'rgb(140,140,140)',
  gain: 'rgb(220,40,40)',
  loss: 'rgb(30,160,90)',
  warning: 'rgb(200,140,20)',
  primary: 'rgb(40,90,220)',
};

const nav = (date: string, v: number): NavPoint => ({ date, nav: v, accNav: 0 });

// 8 个净值日（≤12 → 净值线自身也画 dot，可用来做「点是否在线上」的对照）
const SEQ: NavPoint[] = [
  nav('2026-09-01', 1.0),
  nav('2026-09-02', 1.05),
  nav('2026-09-03', 1.02),
  nav('2026-09-04', 1.1),
  nav('2026-09-07', 1.08),
  nav('2026-09-08', 1.14),
  nav('2026-09-09', 1.11),
  nav('2026-09-10', 1.2),
];

// 2026-09-05/06 是周末 → 该笔买入应归到 09-04
const MARKERS = [
  { date: '2026-09-02', txnType: 'buy', shares: 100 },
  { date: '2026-09-06', txnType: 'buy', shares: 100 },
  { date: '2026-09-08', txnType: 'sell', shares: 50 },
  { date: '2026-09-09', txnType: 'dividend', shares: 0 },
];

function renderChart(markers = MARKERS) {
  const rows = buildNavChartRows(SEQ, markers);
  const tMin = rows[0].t;
  const tMax = rows[rows.length - 1].t;
  const { container } = render(
    <NavChart
      rows={rows}
      xTicks={evenlySpacedTicks(tMin, tMax, 5)}
      yDomain={[0.9, 1.3]}
      yTicks={[0.9, 1.0, 1.1, 1.2, 1.3]}
      yDecimals={2}
      costLevel={1.05}
      hasAccNav={false}
      isTouch={false}
      narrow={false}
      colors={COLORS}
      tickFormatter={(v: number) => new Date(v).toISOString().slice(5, 10)}
      width={640}
      height={280}
    />,
  );
  const circles = Array.from(container.querySelectorAll('circle')).map((c) => ({
    cx: Number(c.getAttribute('cx')),
    cy: Number(c.getAttribute('cy')),
    r: Number(c.getAttribute('r')),
    fill: c.getAttribute('fill'),
    stroke: c.getAttribute('stroke'),
  }));
  return { rows, circles, container };
}

describe('NavChart 几何：圆点精确落在净值线上', () => {
  it('每个交易/分红圆点都与净值线同数据点的坐标完全重合', () => {
    const { rows, circles } = renderChart();
    // 净值线自身的点圆（r=2.5）
    const lineDots = circles.filter((c) => c.r === 2.5);
    expect(lineDots.length).toBe(rows.length);

    // 交易圆点：买入 r=4 实心、卖出 r=4 空心、分红 r=3
    const buyDots = circles.filter((c) => c.r === 4 && c.fill === COLORS.gain);
    const sellDots = circles.filter((c) => c.r === 4 && c.fill === COLORS.surface);
    const divDots = circles.filter((c) => c.r === 3);
    expect(buyDots).toHaveLength(2); // 09-02 与（归位后的）09-04
    expect(sellDots).toHaveLength(1);
    expect(divDots).toHaveLength(1);

    for (const dot of [...buyDots, ...sellDots, ...divDots]) {
      const hit = lineDots.find((d) => Math.abs(d.cx - dot.cx) < 0.01 && Math.abs(d.cy - dot.cy) < 0.01);
      expect(hit, `圆点 (${dot.cx},${dot.cy}) 未落在净值线的数据点上`).toBeTruthy();
    }
  });

  it('圆圈半径/填充区分买、卖、分红：实心 vs 空心不依赖颜色', () => {
    const { circles } = renderChart();
    const filled = circles.find((c) => c.r === 4 && c.fill === COLORS.gain)!;
    const hollow = circles.find((c) => c.r === 4 && c.fill === COLORS.surface)!;
    expect(filled).toBeTruthy();
    expect(hollow).toBeTruthy();
    expect(hollow.stroke).toBe(COLORS.loss); // 空心圆圈用卖出色描边
    expect(filled.cy).not.toBe(hollow.cy);
  });

  it('同日既买又卖：买入实心点 + 外圈「只描边」卖出环，同心且互不遮挡', () => {
    const { circles } = renderChart([{ date: '2026-09-03', txnType: 'buy', shares: 100 }, { date: '2026-09-03', txnType: 'sell', shares: 50 }]);
    const dot = circles.find((c) => c.r === 4)!;
    const ring = circles.find((c) => c.r === 6.8)!;
    expect(dot.fill).toBe(COLORS.gain);
    expect(ring.fill).toBe('none'); // 不填充 → 不会盖住内层买入点
    expect(ring.stroke).toBe(COLORS.loss);
    expect(ring.cx).toBeCloseTo(dot.cx, 6);
    expect(ring.cy).toBeCloseTo(dot.cy, 6);
  });

  it('Y 轴刻度等距（网格线间距一致）', () => {
    const { container } = renderChart();
    const ys = Array.from(container.querySelectorAll('.recharts-yAxis .recharts-cartesian-axis-tick-value'))
      .map((t) => Number(t.getAttribute('y')))
      .sort((a, b) => a - b);
    expect(ys.length).toBe(5);
    const gaps = ys.slice(1).map((y, i) => +(y - ys[i]).toFixed(2));
    expect(new Set(gaps).size).toBe(1);
  });

  it('X 轴刻度按日期等距（同一网格步长），不随数据点疏密偏移', () => {
    const { rows } = renderChart();
    const ticks = evenlySpacedTicks(rows[0].t, rows[rows.length - 1].t, 5);
    const gaps = ticks.slice(1).map((t, i) => t - ticks[i]);
    expect(new Set(gaps).size).toBe(1); // 间隔完全一致
  });

  it('成本线渲染为横向参考线（y 恒定 = 持仓成本）', () => {
    const { container } = renderChart();
    const texts = Array.from(container.querySelectorAll('text')).map((t) => t.textContent ?? '');
    expect(texts.some((t) => t.includes('成本 1.0500'))).toBe(true);
    // Y 轴单位说明
    expect(texts.some((t) => t.includes('净值（元）'))).toBe(true);
  });
});
