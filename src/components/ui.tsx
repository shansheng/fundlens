// FundLens 通用 UI 原子组件 — 严格使用 design tokens 与 Lucide 图标（P0：禁止 emoji）
import type { ReactNode } from 'react';
import { TrendingUp, TrendingDown, Wallet, ShoppingBag, PiggyBank, type LucideIcon } from 'lucide-react';
import { PLATFORMS } from '../lib/mockData';

export function Card({ children, className = '', title, action }: { children: ReactNode; className?: string; title?: ReactNode; action?: ReactNode }) {
  return (
    <section className={`bg-surface border border-border rounded-md shadow-ring ${className}`}>
      {(title || action) && (
        <header className="flex items-center justify-between px-4 py-3 border-b border-border">
          <h3 className="text-base font-semibold text-foreground">{title}</h3>
          {action}
        </header>
      )}
      <div className="p-4">{children}</div>
    </section>
  );
}

export function StatTile({ label, value, tone }: { label: string; value: ReactNode; tone?: 'gain' | 'loss' | 'neutral' }) {
  const toneClass = tone === 'gain' ? 'text-gain' : tone === 'loss' ? 'text-loss' : 'text-foreground';
  return (
    <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
      <div className="text-xs text-muted mb-1">{label}</div>
      <div className={`tnum text-xl font-semibold ${toneClass}`}>{value}</div>
    </div>
  );
}

export function EmptyState({ title, hint }: { title: string; hint?: string }) {
  return (
    <div className="flex flex-col items-center justify-center text-center py-16 text-muted">
      <div className="text-sm font-medium text-foreground">{title}</div>
      {hint && <div className="mt-1 text-xs">{hint}</div>}
    </div>
  );
}

const PLATFORM_ICON: Record<string, LucideIcon> = {
  alipay: Wallet,
  jd_finance: ShoppingBag,
  tencent_licai: PiggyBank,
};

export function PlatformBadge({ code }: { code: string }) {
  const pm = PLATFORMS[code];
  const Icon = PLATFORM_ICON[code] ?? Wallet;
  return (
    <span className="inline-flex items-center gap-1.5 rounded-pill border border-border px-2 py-0.5 text-xs text-muted">
      <Icon size={14} strokeWidth={2} aria-hidden style={{ color: pm?.accent ?? 'var(--color-muted)' }} />
      {pm?.name ?? code}
    </span>
  );
}

export function TrendChip({ value, format = 'pct' }: { value: number; format?: 'pct' | 'amount' }) {
  const Icon = value > 0 ? TrendingUp : value < 0 ? TrendingDown : null;
  const color = value > 0 ? 'var(--color-gain)' : value < 0 ? 'var(--color-loss)' : 'var(--color-muted)';
  const text = format === 'pct' ? `${(value * 100).toFixed(2)}%` : `${value >= 0 ? '+' : '-'}¥${Math.abs(value).toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`;
  const prefix = format === 'pct' && value !== 0 ? (value > 0 ? '+' : '') : '';
  return (
    <span className="tnum inline-flex items-center gap-1 text-sm font-medium" style={{ color }}>
      {Icon && <Icon size={16} strokeWidth={2} aria-hidden />}
      {prefix}
      {text}
    </span>
  );
}

const CONFIDENCE_META: Record<string, { label: string; cls: string; title: string }> = {
  high: { label: '高', cls: 'text-success border-success/40 bg-success/10', title: '双口径一致，估算可信度高' },
  medium: { label: '中', cls: 'text-warning border-warning/40 bg-warning/10', title: '双口径小幅分歧，估算可作参考' },
  low: { label: '低', cls: 'text-danger border-danger/40 bg-danger/10', title: '双口径明显分歧，估算谨慎参考' },
  none: { label: '无', cls: 'text-muted border-border bg-border/40', title: '无平台估值可交叉验证' },
};

export function ConfidenceBadge({ level, showLabel = true }: { level?: string; showLabel?: boolean }) {
  const meta = CONFIDENCE_META[level ?? 'none'] ?? CONFIDENCE_META.none;
  return (
    <span
      className={`tnum inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-xs font-normal ${meta.cls}`}
      title={meta.title}
    >
      {showLabel ? `置信度${meta.label}` : meta.label}
    </span>
  );
}
