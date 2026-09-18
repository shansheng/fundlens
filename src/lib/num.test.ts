import { describe, expect, test } from 'vitest';
import { FLAT_EPSILON, normalizeFlat } from './num';

describe('lib/num 负零归一', () => {
  // 回归护栏：-1e-10 这类脏值若不归一，会渲染成「绿色 −0.00%」——
  // 颜色说「跌」、文本说「0」，自相矛盾。曾同时存在于 GainLossBadge 与 TrendChip。
  test('把负零与极小脏值收敛到正零', () => {
    expect(normalizeFlat(-1e-10)).toBe(0);
    expect(normalizeFlat(1e-10)).toBe(0);
    expect(normalizeFlat(-0)).toBe(0);
    expect(Object.is(normalizeFlat(-1e-10), -0)).toBe(false);
  });

  test('归一后不再产生负零文本', () => {
    expect(`${(normalizeFlat(-1e-10) * 100).toFixed(2)}%`).toBe('0.00%');
    expect(`${normalizeFlat(-1e-10).toFixed(4)}`).toBe('0.0000');
  });

  test('真正的涨跌不受影响（含刚好在阈值外的值）', () => {
    expect(normalizeFlat(0.05)).toBe(0.05);
    expect(normalizeFlat(-0.05)).toBe(-0.05);
    expect(normalizeFlat(FLAT_EPSILON)).toBe(FLAT_EPSILON);
    expect(normalizeFlat(-FLAT_EPSILON)).toBe(-FLAT_EPSILON);
  });
});
