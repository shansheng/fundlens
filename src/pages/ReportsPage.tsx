// 周报月报页 — 周报 / 月报 / 盈亏日历（组合市值快照历史）
import { useCallback, useEffect, useState } from 'react';
import { TrendingUp, TrendingDown, RefreshCw, Activity } from 'lucide-react';
import {
  getWeeklyReport,
  getMonthlyReport,
  getPnlCalendar,
  getOverview,
  type PeriodReport,
  type SnapshotPoint,
  type MoverOut,
} from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, EmptyState } from '../components/ui';

type Tab = 'week' | 'month' | 'calendar';

// 轻量 SVG 迷你折线（绘制组合市值走势，红/绿随整体涨跌）
function Sparkline({ points, up }: { points: number[]; up: boolean }) {
  if (points.length < 2) return <div className="h-16" />;
  const w = 280;
  const h = 64;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = max - min || 1;
  const step = w / (points.length - 1);
  const coords = points.map((p, i) => [i * step, h - ((p - min) / span) * (h - 8) - 4]);
  const d = coords.map((c, i) => `${i === 0 ? 'M' : 'L'}${c[0].toFixed(1)},${c[1].toFixed(1)}`).join(' ');
  const color = up ? 'var(--color-gain)' : 'var(--color-loss)';
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="w-full h-16" preserveAspectRatio="none" aria-hidden>
      <path d={d} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

function MoverCard({ label, m }: { label: string; m: MoverOut | null }) {
  if (!m) return null;
  return (
    <div className="bg-surface border border-border rounded-md p-3">
      <div className="text-xs text-muted mb-1">{label}</div>
      <div className="text-sm font-medium">{m.name}</div>
      <div className="mt-1.5 flex items-center gap-2">
        <GainLossBadge value={m.totalPnlPct} format="pct" />
        <GainLossBadge value={m.totalPnl} format="amount" />
      </div>
    </div>
  );
}

function ReportBlock({ report }: { report: PeriodReport }) {
  if (!report.hasHistory) {
    return (
      <EmptyState
        title="暂无可统计的历史"
        hint="快照从你首次打开「持仓总览」起累积；请先去总览页加载一次，之后每日自动记录。"
      />
    );
  }
  const up = report.deltaMv >= 0;
  const mvSeries = report.series.map((s) => s.totalMarketValue);
  return (
    <div className="space-y-4">
      <div className="flex items-baseline gap-2 flex-wrap">
        <span className="text-sm text-muted">统计区间</span>
        <span className="tnum text-sm font-medium">{report.startDate}</span>
        <span className="text-muted">→</span>
        <span className="tnum text-sm font-medium">{report.endDate}</span>
        <span className="text-sm text-muted">· 范围：{report.scope}</span>
      </div>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatTile
          label="期末市值"
          value={`¥${report.endMv.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`}
        />
        <StatTile
          label="市值变动"
          value={<GainLossBadge value={report.deltaMv} format="amount" />}
          tone={up ? 'gain' : 'loss'}
        />
        <StatTile
          label="盈亏变动"
          value={<GainLossBadge value={report.deltaPnl} format="amount" />}
          tone={report.deltaPnl >= 0 ? 'gain' : 'loss'}
        />
        <StatTile
          label="区间收益率"
          value={<GainLossBadge value={report.pnlRate} format="pct" />}
          tone={report.pnlRate >= 0 ? 'gain' : 'loss'}
        />
      </div>

      <Card title="市值走势">
        <Sparkline points={mvSeries} up={up} />
      </Card>

      <div className="grid grid-cols-2 md:grid-cols-3 gap-3">
        <div className="bg-surface border border-border rounded-md p-3 flex items-center gap-2">
          <span className="inline-flex items-center justify-center rounded-md bg-success/10 text-success p-1.5">
            <TrendingUp size={16} aria-hidden />
          </span>
          <div>
            <div className="text-xs text-muted">盈利天数</div>
            <div className="tnum text-lg font-semibold text-success">{report.positiveDays}</div>
          </div>
        </div>
        <div className="bg-surface border border-border rounded-md p-3 flex items-center gap-2">
          <span className="inline-flex items-center justify-center rounded-md bg-danger/10 text-danger p-1.5">
            <TrendingDown size={16} aria-hidden />
          </span>
          <div>
            <div className="text-xs text-muted">亏损天数</div>
            <div className="tnum text-lg font-semibold text-danger">{report.negativeDays}</div>
          </div>
        </div>
        <div className="bg-surface border border-border rounded-md p-3 flex items-center gap-2">
          <span className="inline-flex items-center justify-center rounded-md bg-primary/10 text-primary p-1.5">
            <Activity size={16} aria-hidden />
          </span>
          <div>
            <div className="text-xs text-muted">总天数</div>
            <div className="tnum text-lg font-semibold">{report.series.length}</div>
          </div>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <MoverCard label="区间最佳" m={report.best} />
        <MoverCard label="区间最差" m={report.worst} />
      </div>
    </div>
  );
}

function CalendarHeatmap({ series }: { series: SnapshotPoint[] }) {
  if (series.length === 0) {
    return <EmptyState title="暂无盈亏日历数据" hint="去「持仓总览」加载一次即可开始记录每日市值快照。" />;
  }
  const byDate = new Map<string, SnapshotPoint>();
  for (const s of series) byDate.set(s.date, s);
  const dates = series.map((s) => s.date).sort();
  const minDate = new Date(dates[0] + 'T00:00:00');
  const maxDate = new Date(dates[dates.length - 1] + 'T00:00:00');
  const maxAbs = Math.max(200, ...series.map((s) => Math.abs(s.dayPnl)));

  // 本地 YYYY-MM-DD，避免 toISOString 在 GMT+8 下跨日偏移导致键错位
  const localKey = (d: Date) => {
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
  };

  // 从 minDate 所在周日的 7 列网格，按周铺开（GitHub contribution 风格）
  const start = new Date(minDate);
  start.setDate(start.getDate() - start.getDay());
  const weeks: (SnapshotPoint | null)[][] = [];
  const cursor = new Date(start);
  while (cursor <= maxDate) {
    const week: (SnapshotPoint | null)[] = [];
    for (let i = 0; i < 7; i += 1) {
      const key = localKey(cursor);
      week.push(byDate.get(key) ?? null);
      cursor.setDate(cursor.getDate() + 1);
    }
    weeks.push(week);
  }

  const cell = (s: SnapshotPoint | null, i: number) => {
    if (!s) return <div key={i} className="w-3.5 h-3.5 rounded-sm bg-border/40" />;
    const ratio = Math.min(1, Math.abs(s.dayPnl) / maxAbs);
    const opacity = (0.18 + 0.82 * ratio).toFixed(2);
    const bg = s.dayPnl >= 0 ? `rgba(220,38,38,${opacity})` : `rgba(22,163,74,${opacity})`;
    return (
      <div
        key={i}
        title={`${s.date} · 当日盈亏 ${s.dayPnl >= 0 ? '+' : ''}${s.dayPnl.toLocaleString('zh-CN')}（已剔除出入金）`}
        className="w-3.5 h-3.5 rounded-sm"
        style={{ background: bg }}
      />
    );
  };

  return (
    <div className="space-y-3">
      <p className="flex items-center gap-2 text-xs text-muted">
        当日盈亏（已剔除入金/出金干扰）·
        <span className="inline-flex items-center gap-1"><span className="inline-block w-3 h-3 rounded-sm" style={{ background: 'rgba(220,38,38,0.8)' }} />盈利</span>
        <span className="inline-flex items-center gap-1"><span className="inline-block w-3 h-3 rounded-sm" style={{ background: 'rgba(22,163,74,0.8)' }} />亏损</span>
        <span className="inline-flex items-center gap-1"><span className="inline-block w-3 h-3 rounded-sm bg-border/40" />无数据</span>
      </p>
      <div className="overflow-x-auto">
        <div className="flex gap-1">
          {weeks.map((w, wi) => (
            <div key={wi} className="flex flex-col gap-1">{w.map((s, i) => cell(s, i))}</div>
          ))}
        </div>
      </div>
    </div>
  );
}

export default function ReportsPage() {
  const { platform } = usePlatform();
  const [tab, setTab] = useState<Tab>('week');
  const [week, setWeek] = useState<PeriodReport | null>(null);
  const [month, setMonth] = useState<PeriodReport | null>(null);
  const [calendar, setCalendar] = useState<SnapshotPoint[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  const loadAll = useCallback(async () => {
    setLoading(true);
    try {
      const [w, m, c] = await Promise.all([
        getWeeklyReport(),
        getMonthlyReport(),
        getPnlCalendar(3),
      ]);
      setWeek(w);
      setMonth(m);
      setCalendar(c);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  const recordToday = useCallback(async () => {
    setBusy(true);
    try {
      // 触发一次总览查询，后端会在「当日首查」时落盘今日快照
      await getOverview(platform);
      await loadAll();
    } finally {
      setBusy(false);
    }
  }, [platform, loadAll]);

  const tabs: { key: Tab; label: string }[] = [
    { key: 'week', label: '周报' },
    { key: 'month', label: '月报' },
    { key: 'calendar', label: '盈亏日历' },
  ];

  return (
    <div className="p-6 space-y-5">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">周报月报</h1>
          <p className="text-xs text-muted mt-0.5">组合市值快照历史 · 红涨绿跌（当日盈亏已剔除出入金）</p>
        </div>
        <button
          onClick={() => void recordToday()}
          disabled={busy}
          className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
        >
          <RefreshCw size={16} className={busy ? 'animate-spin' : ''} aria-hidden />
          记录今日快照
        </button>
      </header>

      <div className="flex gap-1 border-b border-border">
        {tabs.map((t) => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-4 py-2 text-sm border-b-2 -mb-px transition-colors ${
              tab === t.key ? 'border-primary text-primary font-medium' : 'border-transparent text-muted hover:text-foreground'
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {loading ? (
        <div className="p-6"><EmptyState title="加载中…" /></div>
      ) : tab === 'week' ? (
        <ReportBlock report={week!} />
      ) : tab === 'month' ? (
        <ReportBlock report={month!} />
      ) : (
        <Card title="盈亏日历（近 3 个月）">
          <CalendarHeatmap series={calendar} />
        </Card>
      )}
    </div>
  );
}
