// 收益统计页 — 组合总览 + 资产配置全景 + 最优/最差 + 分平台分布 + 估算覆盖率
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useIsTouch } from '../hooks/useIsTouch';
import { PieChart, Pie, Cell, ResponsiveContainer, Tooltip } from 'recharts';
import { getStats, type StatsResult, type PositionRow, type AssetSlice, fundDetailPath } from '../api';
import { usePlatform } from '../App';
import { useTheme } from '../theme';
import { readColorVar } from '../chartTheme';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, PlatformBadge, EmptyState } from '../components/ui';

// 类别 → CSS 令牌（亮/暗两套已在 index.css 定义，随 data-theme 切换）
const CATEGORY_TOKEN: Record<string, string> = {
  equity: '--color-cat-equity',
  fixed: '--color-cat-fixed',
  money: '--color-cat-money',
  qdii: '--color-cat-qdii',
  other: '--color-cat-other',
};

function AssetAllocationCard({ slices }: { slices: AssetSlice[] }) {
  const { theme } = useTheme();
  // 触屏检测（pointer: coarse）→ 饼图 Tooltip 改用 click 触发。
  const isTouch = useIsTouch();
  // 高亮当前类别：圆环弱化其余扇区 + 列表行同步描边（角度不易比较，比例条才是主读数）。
  const [active, setActive] = useState<string | null>(null);
  // 主题变化时重新解析令牌 → 饼图 fill / 图例圆点 / Tooltip 均随主题切换。
  const palette = useMemo(
    () => Object.fromEntries(Object.keys(CATEGORY_TOKEN).map((k) => [k, readColorVar(CATEGORY_TOKEN[k])])),
    [theme],
  );
  const tooltipStyle = useMemo(
    () => ({
      fontSize: 12,
      borderRadius: 8,
      background: readColorVar('--color-surface'),
      border: `1px solid ${readColorVar('--color-border')}`,
      color: readColorVar('--color-foreground'),
    }),
    [theme],
  );
  const total = slices.reduce((s, x) => s + x.marketValue, 0);
  if (total <= 0) return null;
  // 按市值降序：圆环扇区顺序与右侧列表顺序一致，占比一眼可对读。
  const ordered = [...slices].sort((a, b) => b.marketValue - a.marketValue);
  const maxPct = Math.max(...ordered.map((s) => s.pct), 1e-9);
  const summary = ordered.map((s) => `${s.label} ${(s.pct * 100).toFixed(1)}%`).join('，');
  return (
    <Card title="资产配置全景">
      <div className="flex flex-col sm:flex-row items-center gap-5">
        <div className="relative w-44 h-44 shrink-0" role="img" aria-label={`资产配置：${summary}`}>
          <ResponsiveContainer width="100%" height="100%">
            <PieChart>
              <Pie
                data={ordered}
                dataKey="marketValue"
                nameKey="label"
                innerRadius={56}
                outerRadius={84}
                paddingAngle={2}
                stroke="none"
                isAnimationActive={false}
              >
                {ordered.map((s) => (
                  <Cell
                    key={s.category}
                    fill={palette[s.category] ?? palette.other}
                    opacity={active === null || active === s.category ? 1 : 0.3}
                    onMouseEnter={() => setActive(s.category)}
                    onMouseLeave={() => setActive(null)}
                  />
                ))}
              </Pie>
              <Tooltip
                trigger={isTouch ? 'click' : 'hover'}
                contentStyle={tooltipStyle}
                formatter={(v: number, _n, p) => [
                  `¥${v.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}（${(p?.payload?.pct * 100).toFixed(1)}%）`,
                  p?.payload?.label,
                ]}
              />
            </PieChart>
          </ResponsiveContainer>
          <div className="absolute inset-0 flex flex-col items-center justify-center pointer-events-none">
            <span className="text-[11px] text-muted">总市值</span>
            <span className="tnum text-sm font-semibold">¥{(total / 10000).toFixed(1)}万</span>
          </div>
        </div>
        <div className="flex-1 w-full space-y-2">
          {ordered.map((s) => {
            const color = palette[s.category] ?? palette.other;
            const on = active === s.category;
            return (
              <div
                key={s.category}
                className={`flex items-center gap-2 text-sm rounded-sm px-1 -mx-1 transition-colors ${on ? 'bg-surface ring-1 ring-primary/30' : ''}`}
                onMouseEnter={() => setActive(s.category)}
                onMouseLeave={() => setActive(null)}
              >
                <span className="inline-block w-2.5 h-2.5 rounded-full shrink-0" style={{ background: color }} aria-hidden />
                <span className="w-16 shrink-0 truncate text-foreground" title={s.label}>{s.label}</span>
                {/* 比例条：以最大类别为满格基准，宽度直接可比较（不依赖扇区角度） */}
                <span className="h-1.5 min-w-0 flex-1 overflow-hidden rounded-full bg-border/50" aria-hidden>
                  <span className="block h-full rounded-full" style={{ width: `${Math.max((s.pct / maxPct) * 100, 4)}%`, background: color }} />
                </span>
                <span className="tnum w-12 shrink-0 text-right text-muted">{(s.pct * 100).toFixed(1)}%</span>
                <span className="tnum w-24 shrink-0 text-right text-muted">¥{s.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 0 })}</span>
              </div>
            );
          })}
        </div>
      </div>
      <p className="mt-3 text-xs text-muted">
        按基金类型归并：权益类（股票/混合/指数/ETF联接/分级）、固收类（债券/理财）、货币类、QDII。数据来自你导入的持仓，本地聚合。
        比例条以占比最大的类别为满格基准；悬浮/点击圆环可高亮对应类别。
      </p>
    </Card>
  );
}

function BestWorst({ label, p }: { label: string; p: PositionRow | null }) {
  if (!p) return null;
  return (
    <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
      <div className="text-xs text-muted mb-1">{label}</div>
      <Link to={fundDetailPath(p.fund.code, p.fund.platform)} className="font-medium text-foreground hover:text-primary">
        {p.fund.name}
      </Link>
      <div className="mt-2 flex items-center gap-3">
        <GainLossBadge value={p.totalPnlPct} format="pct" />
        <GainLossBadge value={p.totalPnl} format="amount" />
      </div>
    </div>
  );
}

export default function StatsPage() {
  const { platform } = usePlatform();
  const [data, setData] = useState<StatsResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await getStats(platform);
      setData(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      console.error('[FundLens] getStats failed:', e);
    } finally {
      setLoading(false);
    }
  }, [platform]);

  useEffect(() => {
    void load();
  }, [load]);

  if (loading && !data) return <div className="p-6"><EmptyState title="加载中…" /></div>;
  if (error) return (
    <div className="p-6 space-y-3">
      <EmptyState title="加载失败" hint={error} />
      <button onClick={() => void load()} className="rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover">重试</button>
    </div>
  );
  if (!data) return <div className="p-6"><EmptyState title="暂无数据" /></div>;

  const { summary, best, worst, byPlatform, estimatedCoverage, assetAllocation } = data;

  return (
    <div className="p-4 sm:p-6 space-y-5">
      <header>
        <h1 className="text-xl font-semibold">收益统计</h1>
        <p className="text-xs text-muted mt-0.5">基于本地自算估值与持仓成本汇总</p>
      </header>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatTile label="总市值" value={`¥${summary.totalMarketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`} />
        <StatTile
          label="累计收益率"
          value={<GainLossBadge value={summary.totalPnlPct} format="pct" />}
          tone={summary.totalPnlPct > 0 ? 'gain' : summary.totalPnlPct < 0 ? 'loss' : 'neutral'}
        />
        <StatTile
          label="累计盈亏"
          value={<GainLossBadge value={summary.totalPnl} format="amount" />}
          tone={summary.totalPnl > 0 ? 'gain' : summary.totalPnl < 0 ? 'loss' : 'neutral'}
        />
        <StatTile label="估算覆盖率" value={`${(estimatedCoverage * 100).toFixed(0)}%`} />
      </div>

      <AssetAllocationCard slices={assetAllocation} />

      <div className="grid grid-cols-2 gap-3">
        <BestWorst label="表现最佳" p={best} />
        <BestWorst label="表现最差" p={worst} />
      </div>

      <Card title="分平台分布">
          <div className="overflow-x-auto">
          <table className="w-full text-sm min-w-[300px] sm:min-w-[360px]">
            <thead>
              <tr className="text-left text-xs text-muted border-b border-border">
                <th className="py-2 pr-3 font-medium">平台</th>
                <th className="py-2 pr-3 font-medium text-right">市值</th>
                <th className="py-2 pr-3 font-medium text-right">盈亏</th>
              </tr>
            </thead>
            <tbody>
              {byPlatform.map((b) => (
                <tr key={b.platform} className="border-b border-border/60 last:border-0">
                  <td className="py-2.5 pr-3"><PlatformBadge code={b.platform} /></td>
                  <td className="py-2.5 pr-3 text-right tnum">¥{b.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                  <td className="py-2.5 pr-3 text-right"><GainLossBadge value={b.totalPnl} format="amount" /></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Card>
    </div>
  );
}
