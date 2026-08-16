import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { StatTile, TrendChip, ConfidenceBadge, PlatformBadge } from './ui';

describe('UI 原子组件', () => {
  it('StatTile 展示标签与数值', () => {
    render(<StatTile label="总市值" value="¥10,000" />);
    expect(screen.getByText('总市值')).toBeInTheDocument();
    expect(screen.getByText('¥10,000')).toBeInTheDocument();
  });

  it('TrendChip 涨显示 --color-gain 与 + 前缀', () => {
    render(<TrendChip value={0.05} />);
    const el = screen.getByText('+5.00%');
    expect(el.getAttribute('style')).toContain('--color-gain');
  });

  it('TrendChip 跌显示 --color-loss', () => {
    render(<TrendChip value={-0.05} />);
    const el = screen.getByText('-5.00%');
    expect(el.getAttribute('style')).toContain('--color-loss');
  });

  it('ConfidenceBadge 高置信度展示「高」并带 success 样式', () => {
    render(<ConfidenceBadge level="high" />);
    const el = screen.getByText('置信度高');
    expect(el).toBeInTheDocument();
    expect(el.className).toContain('text-success');
  });

  it('PlatformBadge 按 code 展示平台名', () => {
    render(<PlatformBadge code="alipay" />);
    expect(screen.getByText('支付宝')).toBeInTheDocument();
  });

  it('PlatformBadge 未知 code 回退到 code 文本', () => {
    render(<PlatformBadge code="unknown_x" />);
    expect(screen.getByText('unknown_x')).toBeInTheDocument();
  });
});
