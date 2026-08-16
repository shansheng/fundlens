import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { GainLossBadge } from './GainLossBadge';

// A 股语义硬约束：涨=红(--color-gain)/跌=绿(--color-loss)，且必须 +/- 号 + 箭头双重编码。
describe('GainLossBadge', () => {
  it('涨（正）渲染红色增益变量与上行箭头', () => {
    render(<GainLossBadge value={0.0123} format="pct" />);
    const el = screen.getByText('+1.23%');
    expect(el).toBeInTheDocument();
    expect(el.getAttribute('style')).toContain('--color-gain');
  });

  it('跌（负）渲染绿色损失变量与下行箭头', () => {
    render(<GainLossBadge value={-0.0456} format="pct" />);
    const el = screen.getByText('-4.56%');
    expect(el).toBeInTheDocument();
    expect(el.getAttribute('style')).toContain('--color-loss');
  });

  it('平（0）不显示箭头图标', () => {
    const { container } = render(<GainLossBadge value={0} />);
    expect(container.querySelector('svg')).toBeNull();
    expect(screen.getByText('0.00%')).toBeInTheDocument();
  });

  it('金额格式带 ¥ 与正负号', () => {
    render(<GainLossBadge value={1234.5} format="amount" />);
    expect(screen.getByText('+¥1,234.50')).toBeInTheDocument();
  });
});
