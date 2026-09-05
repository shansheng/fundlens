// 收益统计页 — 组合总览 + 资产配置全景 + 最优/最差 + 分平台分布 + 估算覆盖率
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { PieChart, Pie, Cell, ResponsiveContainer, Tooltip } from 'recharts';
import { getStats, type StatsResult, type PositionRow, type AssetSlice } from '../api';
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
  return (
    <Card title="资产配置全景">
      <div className="flex flex-col sm:flex-row items-center gap-5">
        <div className="relative w-44 h-44 shrink-0">
          <ResponsiveContainer width="100%" height="100%">
            <PieChart>
              <Pie
                data={slices}
                dataKey="marketValue"
                nameKey="label"
                innerRadius={56}
                outerRadius={84}
                paddingAngle={2}
                stroke="none"
              >
                {slices.map((s) => (
                  <Cell key={s.category} fill={palette[s.category] ?? palette.other} />
                ))}
              </Pie>
              <Tooltip contentStyle={tooltipStyle} formatter={(v: number, _n, p) => [
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
          {slices.map((s) => (
            <div key={s.category} className="flex items-center gap-2 text-sm">
              <span className="inline-block w-2.5 h-2.5 rounded-sm shrink-0" style={{ background: palette[s.category] ?? palette.other }} />
              <span className="text-foreground">{s.label}</span>
              <span className="ml-auto tnum text-muted">{(s.pct * 100).toFixed(1)}%</span>
              <span className="tnum w-28 text-right text-muted">¥{s.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 0 })}</span>
            </div>
          ))}
        </div>
      </div>
      <p className="mt-3 text-xs text-muted">
        按基金类型归并：权益类（股票/混合/指数/ETF联接/分级）、固收类（债券/理财）、货币类、QDII。数据来自你导入的持仓，本地聚合。
      </p>
    </Card>
  );
}

function BestWorst({ label, p }: { label: string; p: PositionRow | null }) {
  if (!p) return null;
  return (
    <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
      <div className="text-xs text-muted mb-1">{label}</div>
      <Link to={`/fund/${p.fund.code}`} className="font-medium text-foreground hover:text-primary">
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
    <div className="p-6 space-y-5">
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
          <table className="w-full text-sm">
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
