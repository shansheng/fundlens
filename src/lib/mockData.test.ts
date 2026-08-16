import { describe, it, expect } from 'vitest';
import { PLATFORMS, MOCK_FUNDS } from './mockData';

describe('mockData', () => {
  it('PLATFORMS 含三个已知平台且 code 自洽', () => {
    expect(Object.keys(PLATFORMS).sort()).toEqual(['alipay', 'jd_finance', 'tencent_licai']);
    for (const p of Object.values(PLATFORMS)) {
      expect(p.name.length).toBeGreaterThan(0);
      expect(p.accent).toMatch(/^#/);
    }
  });

  it('MOCK_FUNDS 非空且每只基金披露权重之和不超过 1', () => {
    expect(MOCK_FUNDS.length).toBeGreaterThan(0);
    for (const f of MOCK_FUNDS) {
      const sum = f.holdings.reduce((a, h) => a + h.weight, 0);
      expect(sum).toBeLessThanOrEqual(1.0000001);
    }
  });
});
