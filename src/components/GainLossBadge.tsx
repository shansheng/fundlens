import { ArrowDownRight, ArrowUpRight } from 'lucide-react';

interface GainLossBadgeProps {
  /** 数值（正=涨/盈，负=跌/亏），A 股语义 */
  value: number;
  /** 展示形态：'pct' 百分比 | 'amount' 金额 | 'nav' 净值差 */
  format?: 'pct' | 'amount' | 'nav';
  /** 是否带 +/- 前缀（默认 true，强制双重编码） */
  signed?: boolean;
  subtle?: boolean; // 浅底徽标
}

const fmtPct = (v: number) => `${(v * 100).toFixed(2)}%`;
const fmtAmount = (v: number) =>
  `${v >= 0 ? '+' : '-'}¥${Math.abs(v).toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;

// P0 硬约束：涨跌必须 +/- 号 + 箭头图标双重编码，颜色不单独表意（gain=红/loss=绿）
export function GainLossBadge({ value, format = 'pct', signed = true, subtle = false }: GainLossBadgeProps) {
  const isGain = value > 0;
  const isFlat = Math.abs(value) < 1e-9;
  const colorVar = isFlat ? 'var(--color-muted)' : isGain ? 'var(--color-gain)' : 'var(--color-loss)';
  const bgVar = isFlat ? 'transparent' : isGain ? 'var(--color-gain-subtle)' : 'var(--color-loss-subtle)';
  const Icon = isFlat ? null : isGain ? ArrowUpRight : ArrowDownRight;

  let text: string;
  if (format === 'pct') text = fmtPct(value);
  else if (format === 'amount') text = fmtAmount(value);
  else text = `${(signed && value >= 0 ? '+' : '')}${value.toFixed(4)}`;

  if (signed && format === 'pct' && !isFlat) text = `${isGain ? '+' : ''}${text}`;
  if (signed && format === 'nav' && !isFlat) text = `${isGain ? '+' : '-'}${Math.abs(value).toFixed(4)}`;

  return (
    <span
      className="tnum inline-flex items-center gap-1 rounded-pill px-2 py-0.5 text-sm"
      style={{ color: colorVar, background: subtle ? bgVar : 'transparent' }}
    >
      {Icon && <Icon size={16} strokeWidth={2} aria-hidden />}
      {text}
    </span>
  );
}
