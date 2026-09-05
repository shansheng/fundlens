// 日报周报月报年报页 — 日/周/月/年 区间报告 + 盈亏日历（组合市值快照历史 + 估算统计）
import { useCallback, useEffect, useMemo, useState } from 'react';
import { TrendingUp, TrendingDown, RefreshCw, Activity, Copy, FileDown, Share2 } from 'lucide-react';
import { save } from '@tauri-apps/plugin-dialog';
import { useTheme } from '../theme';
import { readColorVar } from '../chartTheme';
import {
  getDailyReport,
  getWeeklyReport,
  getMonthlyReport,
  getYearlyReport,
  getPnlCalendar,
  getOverview,
  writeTextFile,
  isTauri,
  type PeriodReport,
  type SnapshotPoint,
  type MoverOut,
} from '../api';
import type { PortfolioSummary } from '../types';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, EmptyState } from '../components/ui';

type PeriodTab = 'day' | 'week' | 'month' | 'year';
type Tab = PeriodTab | 'calendar';

// ---- 数值格式化（用于 Markdown 文本，无法用颜色，改用 +/- 与文字）----
const fmtMoney = (v: number) =>
  `¥${v.toLocaleString('zh-CN', { maximumFractionDigits: 2, minimumFractionDigits: 2 })}`;
const fmtSignedMoney = (v: number) => `${v >= 0 ? '+' : '-'}${fmtMoney(Math.abs(v))}`;
const fmtPct = (v: number) => `${(v * 100).toFixed(2)}%`;
const fmtSignedPct = (v: number) => `${v >= 0 ? '+' : ''}${fmtPct(v)}`;
const trendWord = (v: number) => (v >= 0 ? '涨' : '跌');

// 将一份日/周/月/年报（或冷启动时的当前组合快照）渲染为可分享的 Markdown 文本。
function buildReportMarkdown(
  kind: '日报' | '周报' | '月报' | '年报',
  report: PeriodReport,
  summary: PortfolioSummary | null,
): string {
  const now = new Date().toLocaleString('zh-CN', { hour12: false });
  if (!report.hasHistory) {
    const lines = [
      `# FundLens 基金组合${kind}`,
      '> 当前组合快照（历史市值快照不足，以下为实时组合概况）',
      '',
      '## 当前组合',
    ];
    if (summary) {
      const today = summary.actDayPnl !== 0 ? summary.actDayPnl : summary.estDayPnl;
      lines.push(
        `- 总市值：**${fmtMoney(summary.totalMarketValue)}**`,
        `- 累计收益率：**${fmtSignedPct(summary.totalPnlPct)}**`,
        `- 累计盈亏：${fmtSignedMoney(summary.totalPnl)}`,
        `- 今日收益：${fmtSignedMoney(today)}`,
        `- 今日估算收益：${fmtSignedMoney(summary.estDayPnl)}`,
      );
    } else {
      lines.push('- （暂无组合数据，请先到「持仓总览」导入持仓）');
    }
    lines.push(
      '',
      '## 历史累积中',
      `- 已记录 ${report.series.length} 天市值快照`,
      '- 满 1 天生成日报、满 7 天生成周报、满 30 天生成月报、满 365 天生成年报（每日打开「持仓总览」自动记录）',
      '- 估算统计（估算收益/实际收益/偏差）自估算快照启用起累积，早于启用日期的数据为 0',
      '',
      '---',
      '由 FundLens 本地生成 · 数据全部存于本机，红涨绿跌（当日盈亏已剔除入金/出金）',
      `生成时间：${now}`,
    );
    return lines.join('\n');
  }

  // 走势极值
  let maxMv = -Infinity;
  let minMv = Infinity;
  let maxDate = '';
  let minDate = '';
  for (const p of report.series) {
    if (p.totalMarketValue > maxMv) {
      maxMv = p.totalMarketValue;
      maxDate = p.date;
    }
    if (p.totalMarketValue < minMv) {
      minMv = p.totalMarketValue;
      minDate = p.date;
    }
  }
  const best = report.best;
  const worst = report.worst;
  const diffWord = report.estActDiff >= 0 ? '高估' : '低估';
  const lines = [
    `# FundLens 基金组合${kind}`,
    `> 统计区间：${report.startDate} → ${report.endDate}（${report.scope}）`,
    `> 生成时间：${now}`,
    '',
    '## 区间表现',
    `- 期末市值：**${fmtMoney(report.endMv)}**`,
    `- 市值变动：${fmtSignedMoney(report.deltaMv)}（${trendWord(report.deltaMv)}）`,
    `- 盈亏变动：${fmtSignedMoney(report.deltaPnl)}`,
    `- 区间收益率：**${fmtSignedPct(report.pnlRate)}**`,
    '',
    '## 估算 vs 实际（偏差 = 估算 − 实际）',
    `- 区间估算收益：${fmtSignedMoney(report.estDeltaPnl)}（${fmtSignedPct(report.estPnlRate)}）`,
    `- 区间实际收益：${fmtSignedMoney(report.deltaPnl)}（${fmtSignedPct(report.pnlRate)}）`,
    `- 偏差：${fmtSignedMoney(report.estActDiff)}（${fmtSignedPct(report.diffRate)}，估算${diffWord}）`,
    `- 估算口径：盈利 ${report.estPositiveDays} 天 · 亏损 ${report.estNegativeDays} 天`,
    '- 说明：估算统计自估算快照启用起累积，早于启用日期的数据为 0；盘中快照的偏差含未实现成分',
    '',
    '## 交易节奏',
    `- 盈利天数 ${report.positiveDays} · 亏损天数 ${report.negativeDays} · 总天数 ${report.series.length}`,
    '',
    '## 区间最佳 / 最差',
    best
      ? `- 最佳：${best.name}（${fmtSignedPct(best.totalPnlPct)} / ${fmtSignedMoney(best.totalPnl)}）`
      : '- 最佳：—',
    worst
      ? `- 最差：${worst.name}（${fmtSignedPct(worst.totalPnlPct)} / ${fmtSignedMoney(worst.totalPnl)}）`
      : '- 最差：—',
    '',
    '## 走势要点',
    `- 期初市值 ${fmtMoney(report.startMv)} → 期末 ${fmtMoney(report.endMv)}`,
    `- 区间最高市值 ${fmtMoney(maxMv)}（${maxDate}）· 最低 ${fmtMoney(minMv)}（${minDate}）`,
    '',
    '---',
    '由 FundLens 本地生成 · 数据全部存于本机，红涨绿跌（当日盈亏已剔除入金/出金）',
  ];
  return lines.join('\n');
}

// 轻量 SVG 迷你折线（绘制组合市值走势，红/绿随整体涨跌）
function Sparkline({ points, up }: { points: number[]; up: boolean }) {
  const { theme } = useTheme();
  // SVG 属性对 var() 支持不稳定，渲染期解析为 rgb()；useTheme 订阅保证切换主题即重算。
  const color = useMemo(() => readColorVar(up ? '--color-gain' : '--color-loss'), [theme, up]);
  if (points.length < 2) return <div className="h-16" />;
  const w = 280;
  const h = 64;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = max - min || 1;
  const step = w / (points.length - 1);
  const coords = points.map((p, i) => [i * step, h - ((p - min) / span) * (h - 8) - 4]);
  const d = coords.map((c, i) => `${i === 0 ? 'M' : 'L'}${c[0].toFixed(1)},${c[1].toFixed(1)}`).join(' ');
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="w-full h-16" preserveAspectRatio="none" aria-hidden>
      <path d={d} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}

// 双线迷你折线：实线=实际市值，虚线=估算市值（估算为 0/null 的点视为缺失，线段自动断开）
function DualSparkline({ actual, est, up }: { actual: number[]; est: (number | null)[]; up: boolean }) {
  const { theme } = useTheme();
  const color = useMemo(() => readColorVar(up ? '--color-gain' : '--color-loss'), [theme, up]);
  const estColor = useMemo(() => readColorVar('--color-primary'), [theme]);
  if (actual.length < 2) return <div className="h-16" />;
  const w = 280;
  const h = 64;
  const vals = actual.filter((v) => Number.isFinite(v));
  const min = Math.min(...vals);
  const max = Math.max(...vals);
  const span = max - min || 1;
  const step = w / (actual.length - 1);
  const X = (i: number) => i * step;
  const Y = (v: number) => h - ((v - min) / span) * (h - 8) - 4;
  const actualD = actual
    .map((v, i) => `${i === 0 ? 'M' : 'L'}${X(i).toFixed(1)},${Y(v).toFixed(1)}`)
    .join(' ');
  let estD = '';
  let pen = false;
  est.forEach((v, i) => {
    if (v !== null && Number.isFinite(v) && v !== 0) {
      estD += `${pen ? 'L' : 'M'}${X(i).toFixed(1)},${Y(v).toFixed(1)}`;
      pen = true;
    } else {
      pen = false;
    }
  });
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className="w-full h-16" preserveAspectRatio="none" aria-hidden>
      <path d={actualD} fill="none" stroke={color} strokeWidth={2} strokeLinejoin="round" strokeLinecap="round" />
      {estD && (
        <path
          d={estD}
          fill="none"
          stroke={estColor}
          strokeWidth={1.5}
          strokeDasharray="4 3"
          strokeLinejoin="round"
          strokeLinecap="round"
          opacity={0.85}
        />
      )}
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

function ReportBlock({ report, summary }: { report: PeriodReport; summary: PortfolioSummary | null }) {
  // 冷启动：历史快照不足时，展示「当前组合快照」+ 已有日序列，避免报告页一片空白。
  if (!report.hasHistory) {
    const today = summary ? (summary.actDayPnl !== 0 ? summary.actDayPnl : summary.estDayPnl) : 0;
    const todayTone = today > 0 ? 'gain' : today < 0 ? 'loss' : 'neutral';
    const estTone = summary && summary.estDayPnl > 0 ? 'gain' : summary && summary.estDayPnl < 0 ? 'loss' : 'neutral';
    return (
      <div className="space-y-4">
        <EmptyState
          title="历史快照不足，先看当前组合"
          hint="快照从你首次打开「持仓总览」起每日自动累积；满 1 天出日报、满 7 天出周报、满 30 天出月报、满 365 天出年报。"
        />
        {summary && (
          <Card title="当前组合快照">
            <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
              <StatTile label="总市值" value={fmtMoney(summary.totalMarketValue)} />
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
              <StatTile
                label="今日收益"
                value={<GainLossBadge value={today} format="amount" />}
                tone={todayTone}
              />
              <StatTile
                label="今日估算收益"
                value={<GainLossBadge value={summary.estDayPnl} format="amount" />}
                tone={estTone}
              />
            </div>
          </Card>
        )}
        {report.series.length > 0 && (
          <Card title={`已记录 ${report.series.length} 天市值快照`}>
            <Sparkline
              points={report.series.map((s) => s.totalMarketValue)}
              up={
                report.series[report.series.length - 1].totalMarketValue >=
                report.series[0].totalMarketValue
              }
            />
          </Card>
        )}
      </div>
    );
  }
  const up = report.deltaMv >= 0;
  const mvSeries = report.series.map((s) => s.totalMarketValue);
  const hasEst = report.series.some((s) => s.estMarketValue > 0);
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

      <Card title="市值走势（实线=实际 · 虚线=估算）">
        <DualSparkline
          actual={mvSeries}
          est={report.series.map((s) => (s.estMarketValue > 0 ? s.estMarketValue : null))}
          up={up}
        />
        {!hasEst && (
          <p className="text-xs text-muted mt-1">估算市值自估算快照启用起累积，当前区间暂无估算数据。</p>
        )}
      </Card>

      <Card title="估算 vs 实际（偏差 = 估算 − 实际）">
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <StatTile
            label="区间估算收益"
            value={<GainLossBadge value={report.estDeltaPnl} format="amount" />}
            tone={report.estDeltaPnl >= 0 ? 'gain' : 'loss'}
          />
          <StatTile
            label="区间实际收益"
            value={<GainLossBadge value={report.deltaPnl} format="amount" />}
            tone={report.deltaPnl >= 0 ? 'gain' : 'loss'}
          />
          <StatTile
            label="偏差（估算−实际）"
            value={<GainLossBadge value={report.estActDiff} format="amount" />}
            tone={report.estActDiff >= 0 ? 'gain' : 'loss'}
          />
          <StatTile
            label="估算收益率"
            value={<GainLossBadge value={report.estPnlRate} format="pct" />}
            tone={report.estPnlRate >= 0 ? 'gain' : 'loss'}
          />
        </div>
        <p className="text-xs text-muted mt-2">
          偏差率 {fmtSignedPct(report.diffRate)}（{report.estActDiff >= 0 ? '估算高估' : '估算低估'}）·
          估算口径盈利 {report.estPositiveDays} 天 / 亏损 {report.estNegativeDays} 天 ·
          估算统计自估算快照启用起累积，早于启用日期的数据为 0
        </p>
      </Card>

      <div className="grid grid-cols-2 md:grid-cols-5 gap-3">
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
          <span className="inline-flex items-center justify-center rounded-md bg-success/10 text-success p-1.5">
            <TrendingUp size={16} aria-hidden />
          </span>
          <div>
            <div className="text-xs text-muted">估算盈利天数</div>
            <div className="tnum text-lg font-semibold text-success">{report.estPositiveDays}</div>
          </div>
        </div>
        <div className="bg-surface border border-border rounded-md p-3 flex items-center gap-2">
          <span className="inline-flex items-center justify-center rounded-md bg-danger/10 text-danger p-1.5">
            <TrendingDown size={16} aria-hidden />
          </span>
          <div>
            <div className="text-xs text-muted">估算亏损天数</div>
            <div className="tnum text-lg font-semibold text-danger">{report.estNegativeDays}</div>
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
  const [reports, setReports] = useState<Record<PeriodTab, PeriodReport | null>>({
    day: null,
    week: null,
    month: null,
    year: null,
  });
  const [calendar, setCalendar] = useState<SnapshotPoint[]>([]);
  const [summary, setSummary] = useState<PortfolioSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  // 分享 / 导出
  const [shareOpen, setShareOpen] = useState(false);
  const [shareMsg, setShareMsg] = useState<string | null>(null);

  const loadAll = useCallback(async () => {
    setLoading(true);
    try {
      const [d, w, m, y, c, ov] = await Promise.all([
        getDailyReport(),
        getWeeklyReport(),
        getMonthlyReport(),
        getYearlyReport(),
        getPnlCalendar(3),
        getOverview(platform),
      ]);
      setReports({ day: d, week: w, month: m, year: y });
      setCalendar(c);
      setSummary(ov.summary);
    } finally {
      setLoading(false);
    }
  }, [platform]);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  const recordToday = useCallback(async () => {
    setBusy(true);
    try {
      // 触发一次总览查询，后端会在「当日首查」时落盘今日快照（含估算列）
      await getOverview(platform);
      await loadAll();
    } finally {
      setBusy(false);
    }
  }, [platform, loadAll]);

  const activeTab: PeriodTab | null = tab === 'calendar' ? null : tab;
  const activeReport = activeTab ? reports[activeTab] : null;

  const kindLabel = (t: PeriodTab): '日报' | '周报' | '月报' | '年报' =>
    t === 'day' ? '日报' : t === 'week' ? '周报' : t === 'month' ? '月报' : '年报';

  const handleCopy = useCallback(async () => {
    if (!activeReport || !activeTab) return;
    const md = buildReportMarkdown(kindLabel(activeTab), activeReport, summary);
    try {
      await navigator.clipboard.writeText(md);
      setShareMsg('已复制 Markdown 到剪贴板，可直接粘贴到微信 / 备忘录分享。');
    } catch {
      setShareMsg('复制失败（浏览器限制）。请展开下方预览，手动选中复制。');
    }
  }, [activeReport, activeTab, summary]);

  const handleSave = useCallback(async () => {
    if (!activeReport || !activeTab) return;
    if (!isTauri) {
      setShareMsg('浏览器预览模式不支持保存文件，请使用桌面端。');
      return;
    }
    const kl = kindLabel(activeTab);
    const md = buildReportMarkdown(kl, activeReport, summary);
    const stamp = new Date().toISOString().slice(0, 10);
    const target = await save({
      defaultPath: `fundlens-${kl}-${stamp}.md`,
      filters: [{ name: 'Markdown', extensions: ['md'] }],
    });
    if (!target) return; // 用户取消
    try {
      await writeTextFile(target as string, md);
      setShareMsg(`已保存：${target}`);
    } catch (e) {
      setShareMsg(`保存失败：${(e as Error).message ?? String(e)}`);
    }
  }, [activeReport, activeTab, summary]);

  const tabs: { key: Tab; label: string }[] = [
    { key: 'day', label: '日报' },
    { key: 'week', label: '周报' },
    { key: 'month', label: '月报' },
    { key: 'year', label: '年报' },
    { key: 'calendar', label: '盈亏日历' },
  ];

  return (
    <div className="p-6 space-y-5">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">日报周报月报年报</h1>
          <p className="text-xs text-muted mt-0.5">
            组合市值快照历史 + 估算统计 · 红涨绿跌（当日盈亏已剔除出入金）
          </p>
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

      <div className="flex gap-1 border-b border-border overflow-x-auto">
        {tabs.map((t) => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-4 py-2 text-sm border-b-2 -mb-px whitespace-nowrap transition-colors ${
              tab === t.key ? 'border-primary text-primary font-medium' : 'border-transparent text-muted hover:text-foreground'
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>

      {/* 分享 / 导出（四种区间报告可用；盈亏日历无区间报告，隐藏） */}
      {activeReport && (
        <div className="flex flex-wrap items-center gap-2">
          <button
            onClick={() => void handleCopy()}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60"
          >
            <Copy size={15} aria-hidden /> 复制 Markdown
          </button>
          <button
            onClick={() => void handleSave()}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60"
          >
            <FileDown size={15} aria-hidden /> 保存为 .md
          </button>
          <button
            onClick={() => setShareOpen((v) => !v)}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60"
          >
            <Share2 size={15} aria-hidden /> {shareOpen ? '收起预览' : '预览'}
          </button>
          {shareMsg && <span className="text-xs text-muted">{shareMsg}</span>}
        </div>
      )}
      {shareOpen && activeReport && activeTab && (
        <Card title="Markdown 预览（可手动选中复制）">
          <pre className="max-h-80 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-background p-3 text-xs leading-relaxed text-muted">
            {buildReportMarkdown(kindLabel(activeTab), activeReport, summary)}
          </pre>
        </Card>
      )}

      {loading ? (
        <div className="p-6"><EmptyState title="加载中…" /></div>
      ) : activeTab ? (
        <ReportBlock report={reports[activeTab]!} summary={summary} />
      ) : (
        <Card title="盈亏日历（近 3 个月）">
          <CalendarHeatmap series={calendar} />
        </Card>
      )}
    </div>
  );
}
