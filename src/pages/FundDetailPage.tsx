// 单基金详情页 — 估值拆解：披露持仓对净值的贡献 + 当日行情
// 净值走势单图（对齐支付宝/天天基金）：单位净值主曲线 + 累计净值虚线(仅在分红/拆分时叠加) +
// 持仓成本水平参考线(v9 当前均价) + 买入▲/卖出▼/分红◆ 交易标记；
// 底部图例与图中图形同源（线段=曲线、三角形/菱形=标记），消除「图例与图内不一致」。
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams, Link, useNavigate, useSearchParams } from 'react-router-dom';
import { useIsTouch } from '../hooks/useIsTouch';
import { useNarrow } from '../hooks/useNarrow';

// 取 navDate 的 MM-DD 切片（如 09-11）；非法/空返回 null。
function mmdd(navDate?: string | null): string | null {
  if (!navDate || navDate.length < 10) return null;
  return navDate.slice(5);
}
import { ArrowLeft, CircleAlert, Download, History, Pencil, RefreshCw, Trash2, LineChart as LineChartIcon } from 'lucide-react';
import {
  ComposedChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ReferenceLine,
  ResponsiveContainer,
  Scatter,
} from 'recharts';
import {
  getFundDetail,
  fetchDisclosure,
  refreshQuotes,
  deleteFund,
  getFundSeries,
  refreshNavHistory,
  updatePosition,
  updatePositionCost,
  getHoldingChanges,
  fetchDisclosureHistory,
  lookthroughFund,
  isTauri,
  type FundDetailResult,
  type FundSeries,
  type NavPoint,
  type HoldingChangesResult,
  type HoldingChange,
  type FundLookthroughResult,
} from '../api';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, PlatformBadge, EmptyState } from '../components/ui';
import { useTheme } from '../theme';
import { readColorVar } from '../chartTheme';

// 交易标记统一用「圆点」（v2.6.15 图表重做）：买入=实心圆 / 卖出=空心圆 / 分红=小实心圆。
// 形状（实心 / 空心 / 大小）本身承载语义，不依赖红绿颜色 —— 色觉障碍用户同样可区分。
// ringOnly：同日既买又卖时，卖出用「只描边不填充」的外环套住买入实心点，两点互不遮挡。
type DotShapeProps = {
  cx?: number;
  cy?: number;
  fill?: string;
  ring?: string;
  hollow?: boolean;
  ringOnly?: boolean;
  r?: number;
};
const DotMark = ({
  cx = 0,
  cy = 0,
  fill = 'currentColor',
  ring = '#fff',
  hollow = false,
  ringOnly = false,
  r = 4,
}: DotShapeProps) => (
  <circle
    cx={cx}
    cy={cy}
    r={r}
    fill={ringOnly ? 'none' : hollow ? ring : fill}
    stroke={fill}
    strokeWidth={hollow || ringOnly ? 1.8 : 1.4}
  />
);

// 图例形符：与图中图形同源（曲线=线段、虚线=虚线段、买卖/分红=同款圆点），
// 保证「底部图例」与「图内表示」完全一致（大平台单图例惯例）。
function KeySwatch({
  kind,
  color,
  ring = '#fff',
}: {
  kind: 'line' | 'dash' | 'dotFilled' | 'dotHollow' | 'dotSmall';
  color: string;
  ring?: string;
}) {
  if (kind === 'line' || kind === 'dash') {
    return (
      <svg viewBox="0 0 14 12" width={14} height={12} style={{ verticalAlign: '-1px' }} aria-hidden>
        <line
          x1={1}
          y1={6}
          x2={13}
          y2={6}
          stroke={color}
          strokeWidth={kind === 'line' ? 2 : 1.6}
          strokeDasharray={kind === 'dash' ? '3 2' : undefined}
          strokeLinecap="round"
        />
      </svg>
    );
  }
  const hollow = kind === 'dotHollow';
  return (
    <svg viewBox="0 0 14 14" width={14} height={14} style={{ verticalAlign: '-2px' }} aria-hidden>
      <circle
        cx={7}
        cy={7}
        r={kind === 'dotSmall' ? 3 : 4}
        fill={hollow ? ring : color}
        stroke={color}
        strokeWidth={hollow ? 1.8 : 1.4}
      />
    </svg>
  );
}

// 「日期 → 该日期当日或之前最近一个净值点」的下标（二分）。交易日期可能是周末/非交易日，
// 必须映射到真实存在的净值点；返回 -1 表示早于序列首日。
function nearestNavIndex(navPoints: NavPoint[], date: string): number {
  let lo = 0;
  let hi = navPoints.length - 1;
  let ans = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (navPoints[mid].date <= date) {
      ans = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return ans;
}

// 净值走势图的单行数据。t = 数值型时间键（UTC 毫秒），买卖/分红与净值共用同一行的 (t, nav)，
// 因此圆点必然精确落在净值线上（不再出现「标记偏离曲线」）。
export type NavChartRow = {
  t: number;
  date: string;
  nav: number;
  accNav: number;
  buy: number | null;
  sell: number | null;
  div: number | null;
};

export const DAY_MS = 86_400_000;

// 'YYYY-MM-DD' → UTC 毫秒时间键。用 UTC 而非本地时间，避免 GMT+8 下日期键整体偏移一天。
export function toTimeKey(d: string): number {
  const [y, m, dd] = d.split('-').map(Number);
  return Date.UTC(y || 1970, (m || 1) - 1, dd || 1);
}

/**
 * 把「净值序列 + 交易/分红标记」组装成图表数据行。
 * 关键不变量：标记值恒等于所在行的 nav —— 圆点与净值线共用同一个 (t, nav)，必然落在线上。
 * 交易日落在周末/非交易日 → 归到当日或之前最近一个净值点；早于区间首日 → 归到首日。
 * shares≤0 的非分红流水（导入缺净值时的占位记录）不打点。
 */
export function buildNavChartRows(navPoints: NavPoint[], markers: { date: string; txnType: string; shares: number }[]): NavChartRow[] {
  const rows: NavChartRow[] = navPoints.map((p) => ({
    t: toTimeKey(p.date),
    date: p.date,
    nav: p.nav,
    accNav: p.accNav,
    buy: null,
    sell: null,
    div: null,
  }));
  for (const m of markers) {
    if (m.txnType !== 'dividend' && !(m.shares > 0)) continue;
    if (rows.length === 0) continue;
    const idx = Math.max(0, nearestNavIndex(navPoints, m.date));
    const row = rows[idx];
    if (m.txnType === 'buy') row.buy = row.nav;
    else if (m.txnType === 'sell') row.sell = row.nav;
    else if (m.txnType === 'dividend') row.div = row.nav;
  }
  return rows;
}

/** 时间轴刻度：在 [tMin, tMax] 上取 count 个「日期等距」的点（与数据点疏密无关）。 */
export function evenlySpacedTicks(tMin: number, tMax: number, count: number): number[] {
  if (count <= 1 || tMax <= tMin) return [tMin];
  return Array.from({ length: count }, (_, i) => tMin + ((tMax - tMin) * i) / (count - 1));
}

type NavChartColors = {
  surface: string;
  border: string;
  foreground: string;
  muted: string;
  gain: string;
  loss: string;
  warning: string;
  primary: string;
};

// 自定义 Tooltip：除净值外，把「当天有买入/卖出/分红」直接说出来 —— 图上的点不必再去对照表格。
function NavTooltip({ active, payload, colors }: { active?: boolean; payload?: { payload?: NavChartRow }[]; colors: NavChartColors }) {
  const row = payload?.[0]?.payload;
  if (!active || !row) return null;
  const dot = (color: string, hollow = false) => (
    <span
      aria-hidden
      style={{
        display: 'inline-block',
        width: 8,
        height: 8,
        borderRadius: 999,
        marginRight: 5,
        background: hollow ? colors.surface : color,
        border: `1.6px solid ${color}`,
      }}
    />
  );
  return (
    <div
      style={{
        fontSize: 12,
        lineHeight: 1.7,
        borderRadius: 8,
        background: colors.surface,
        border: `1px solid ${colors.border}`,
        color: colors.foreground,
        padding: '6px 10px',
        boxShadow: '0 4px 14px rgba(0,0,0,0.10)',
      }}
    >
      <div className="tnum" style={{ fontWeight: 600 }}>{row.date}</div>
      <div className="tnum">
        <span style={{ color: colors.muted }}>单位净值 </span>
        {row.nav.toFixed(4)}
      </div>
      {row.accNav > 0 && (
        <div className="tnum">
          <span style={{ color: colors.muted }}>累计净值 </span>
          {row.accNav.toFixed(4)}
        </div>
      )}
      {row.buy !== null && <div>{dot(colors.gain)}买入</div>}
      {row.sell !== null && <div>{dot(colors.loss, true)}卖出</div>}
      {row.div !== null && <div>{dot(colors.warning)}分红</div>}
    </div>
  );
}

// 走势图本体（独立成组件：页面用 ResponsiveContainer 注入宽高，测试可直接给固定宽高渲染，
// 从而对「圆点是否精确落在净值线上」做几何断言）。
export type NavChartProps = {
  rows: NavChartRow[];
  xTicks: number[];
  yDomain: [number, number];
  yTicks: number[];
  yDecimals: number;
  costLevel: number | null;
  hasAccNav: boolean;
  isTouch: boolean;
  narrow: boolean;
  colors: NavChartColors;
  tickFormatter: (v: number) => string;
  /** 由 ResponsiveContainer 注入；测试里显式传入 */
  width?: number;
  height?: number;
};

export function NavChart({
  rows,
  xTicks,
  yDomain,
  yTicks,
  yDecimals,
  costLevel,
  hasAccNav,
  isTouch,
  narrow,
  colors,
  tickFormatter,
  width,
  height,
}: NavChartProps) {
  const divData = rows.filter((r) => r.div !== null);
  // 同日既买又卖 → 卖出改用「只描边的外环」，否则两个圆点圆心重合、其中一个被完全遮住。
  const bothBuyData = rows.filter((r) => r.buy !== null && r.sell !== null);
  const buyPlainData = rows.filter((r) => r.buy !== null && r.sell === null);
  const sellPlainData = rows.filter((r) => r.sell !== null && r.buy === null);
  return (
    <ComposedChart width={width} height={height} data={rows} margin={{ top: 10, right: 14, left: 0, bottom: 0 }}>
      <CartesianGrid stroke={colors.border} strokeDasharray="3 3" vertical={false} />
      {/* 时间轴：type="number" + 数值型时间键 → 刻度严格按日期等距，与买卖点疏密无关 */}
      <XAxis
        dataKey="t"
        type="number"
        domain={['dataMin', 'dataMax']}
        ticks={xTicks}
        tickFormatter={tickFormatter}
        tick={{ fontSize: 11, fill: colors.muted }}
        tickMargin={8}
        tickLine={false}
        axisLine={{ stroke: colors.border }}
        padding={{ left: 8, right: 8 }}
      />
      <YAxis
        tick={{ fontSize: 11, fill: colors.muted }}
        domain={yDomain}
        ticks={yTicks}
        width={narrow ? 46 : 62}
        tickFormatter={(v: number) => v.toFixed(yDecimals)}
        tickLine={false}
        axisLine={false}
        label={
          narrow
            ? undefined
            : { value: '净值（元）', angle: -90, position: 'insideLeft', offset: 6, fontSize: 11, fill: colors.muted }
        }
      />
      {costLevel != null && (
        <ReferenceLine
          y={costLevel}
          stroke={colors.warning}
          strokeWidth={1.4}
          strokeDasharray="6 3"
          label={{
            value: `成本 ${costLevel.toFixed(4)}`,
            position: 'insideTopRight',
            fontSize: 11,
            fill: colors.warning,
          }}
        />
      )}
      <Tooltip
        trigger={isTouch ? 'click' : 'hover'}
        content={<NavTooltip colors={colors} />}
        cursor={{ stroke: colors.border, strokeDasharray: '3 3' }}
      />
      {/* type="linear"：净值序列不做曲线外推（平滑样条会画出实际不存在的取值） */}
      <Line
        type="linear"
        dataKey="nav"
        name="单位净值"
        stroke={colors.primary}
        strokeWidth={1.8}
        isAnimationActive={false}
        activeDot={{ r: 4, fill: colors.primary, stroke: colors.surface, strokeWidth: 1.4 }}
        dot={rows.length <= 12 ? { r: 2.5, fill: colors.primary, strokeWidth: 0 } : false}
      />
      {hasAccNav && (
        <Line
          type="linear"
          dataKey="accNav"
          name="累计净值"
          stroke={colors.muted}
          strokeWidth={1.2}
          strokeDasharray="4 3"
          isAnimationActive={false}
          dot={rows.length <= 12 ? { r: 2, fill: colors.muted, strokeWidth: 0 } : false}
        />
      )}
      {/* 买卖/分红圆点：数据行与净值线共用同一个 (t, nav)，圆点必然落在线上 */}
      {buyPlainData.length > 0 && (
        <Scatter
          data={buyPlainData}
          dataKey="nav"
          name="买入"
          shape={<DotMark fill={colors.gain} ring={colors.surface} />}
          legendType="none"
          isAnimationActive={false}
        />
      )}
      {bothBuyData.length > 0 && (
        <>
          <Scatter
            data={bothBuyData}
            dataKey="nav"
            name="买入"
            shape={<DotMark fill={colors.gain} ring={colors.surface} />}
            legendType="none"
            isAnimationActive={false}
          />
          <Scatter
            data={bothBuyData}
            dataKey="nav"
            name="卖出"
            shape={<DotMark fill={colors.loss} r={6.8} ringOnly />}
            legendType="none"
            isAnimationActive={false}
          />
        </>
      )}
      {sellPlainData.length > 0 && (
        <Scatter
          data={sellPlainData}
          dataKey="nav"
          name="卖出"
          shape={<DotMark fill={colors.loss} ring={colors.surface} hollow />}
          legendType="none"
          isAnimationActive={false}
        />
      )}
      {divData.length > 0 && (
        <Scatter
          data={divData}
          dataKey="nav"
          name="分红"
          shape={<DotMark fill={colors.warning} ring={colors.surface} r={3} />}
          legendType="none"
          isAnimationActive={false}
        />
      )}
    </ComposedChart>
  );
}

const RANGES: { key: string; label: string }[] = [
  { key: '1m', label: '近1月' },
  { key: '3m', label: '近3月' },
  { key: '6m', label: '近6月' },
  { key: 'all', label: '全部' },
];

// 交易类型标签（中性胶囊，避免与「买入负/卖出正」现金流符号混淆）
function TxnTag({ type }: { type: string }) {
  const map: Record<string, string> = {
    buy: '买入',
    sell: '卖出',
    dividend: '分红',
    deposit: '入金',
    withdraw: '出金',
  };
  // 红绿语义与 LedgerPage TxnBadge 一致（买入=红/流出，卖出=绿/流入），两页不漂移
  const cls =
    type === 'buy'
      ? 'text-danger bg-danger/10'
      : type === 'sell'
        ? 'text-success bg-success/10'
        : 'text-foreground bg-border/60';
  return (
    <span className={`rounded px-1.5 py-0.5 text-xs font-medium ${cls}`}>
      {map[type] ?? type}
    </span>
  );
}

// 交易来源标注
function sourceLabel(s: string): string {
  switch (s) {
    case 'import_txn':
      return '交易导入';
    case 'manual_txn':
      return '手动';
    case 'import':
      return '持仓导入';
    case 'manual_set':
      return '手动基线';
    default:
      return s;
  }
}

// 估值来源标签（realtime / local / none）→ 中文 + 配色（复用 design tokens，禁止 emoji）
const SOURCE_META: Record<string, { label: string; cls: string }> = {
  realtime: { label: '盘中实时估值', cls: 'text-primary border-primary/40 bg-primary/10' },
  local: { label: '本地穿透估算', cls: 'text-foreground border-border bg-border/40' },
  none: { label: '无估值', cls: 'text-muted border-border bg-border/40' },
};

function SourceBadge({ source }: { source?: string }) {
  const meta = SOURCE_META[source ?? 'none'] ?? SOURCE_META.none;
  return (
    <span className={`tnum inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-xs ${meta.cls}`}>
      {meta.label}
    </span>
  );
}

// 穿透估值覆盖度进度条（双段：已披露穿透 + 基准近似），直接可视化 disclosed_weight_sum 与
// benchmark_weight 两段占比，把后台「透明计算」的口径摊开给用户。
function CoverageBar({ covered, benchmark }: { covered: number; benchmark: number }) {
  const cov = Math.max(0, Math.min(1, covered));
  const bench = Math.max(0, Math.min(1 - cov, benchmark));
  const tone = cov >= 0.6 ? 'bg-success' : cov >= 0.3 ? 'bg-warning' : 'bg-danger';
  return (
    <div>
      <div className="flex items-center justify-between text-xs text-muted mb-1">
        <span>穿透估值覆盖度</span>
        <span className="tnum text-foreground font-medium">{(cov * 100).toFixed(1)}%</span>
      </div>
      <div
        className="flex h-2.5 w-full overflow-hidden rounded-full bg-border/60"
        role="img"
        aria-label={`穿透估值覆盖度 ${(cov * 100).toFixed(1)}%，其中基准近似 ${(bench * 100).toFixed(1)}%`}
      >
        <div className={tone} style={{ width: `${cov * 100}%` }} />
        <div className="bg-primary opacity-70" style={{ width: `${bench * 100}%` }} />
      </div>
      <div className="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
        <span className="inline-flex items-center gap-1.5">
          <span className={`inline-block h-2.5 w-2.5 rounded-sm ${tone}`} />
          已披露穿透 {(cov * 100).toFixed(1)}%
        </span>
        <span className="inline-flex items-center gap-1.5">
          <span className="inline-block h-2.5 w-2.5 rounded-sm bg-primary opacity-70" />
          基准近似 {(bench * 100).toFixed(1)}%
        </span>
      </div>
    </div>
  );
}

// 「较上期」单元格：新增/加仓=红(▲)，减仓/退出=绿(▼)，持平=灰。A 股语义，红涨绿跌。
function renderHoldingChange(ch: HoldingChange | undefined) {
  if (!ch) return <span className="text-muted">—</span>;
  const isUp = ch.delta > 0;
  const isFlat = Math.abs(ch.delta) < 1e-9;
  const color = isFlat ? 'var(--color-muted)' : isUp ? 'var(--color-gain)' : 'var(--color-loss)';
  const tag =
    ch.changeType === 'new' ? '新增' :
    ch.changeType === 'increase' ? '加仓' :
    ch.changeType === 'decrease' ? '减仓' : '持平';
  const pctTxt =
    ch.changeType === 'new' || ch.changeType === 'exit'
      ? ''
      : ` ${isUp ? '+' : ''}${(ch.delta * 100).toFixed(2)}%`;
  const arrow = isFlat ? '' : isUp ? '▲' : '▼';
  return (
    <span className="tnum inline-flex items-center justify-end gap-0.5" style={{ color }}>
      {tag}
      {pctTxt && <span>{pctTxt}</span>}
      <span aria-hidden>{arrow}</span>
    </span>
  );
}

export default function FundDetailPage() {
  const { code = '' } = useParams();
  // 入口平台（同基金可跨多平台各持有一行，份额/成本不同）：由持仓列表跳转时带在 query 上。
  const [searchParams] = useSearchParams();
  const platform = searchParams.get('platform') || null;
  const navigate = useNavigate();
  const [data, setData] = useState<FundDetailResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 持仓份额内联编辑态
  const [editingShares, setEditingShares] = useState(false);
  const [sharesInput, setSharesInput] = useState('');
  // 持仓成本价（单位成本）内联编辑态
  const [editingCost, setEditingCost] = useState(false);
  const [costInput, setCostInput] = useState('');

  // 净值/成本走势
  const [range, setRange] = useState('all');
  const [series, setSeries] = useState<FundSeries | null>(null);
  const [navRefreshing, setNavRefreshing] = useState(false);
  const autoRefreshed = useRef<Record<string, boolean>>({});

  // 披露持仓「较上期」变化（多期共存后对比展示）；补录历史期次的提示信息
  const [holdingChanges, setHoldingChanges] = useState<HoldingChangesResult | null>(null);
  const [backfillMsg, setBackfillMsg] = useState('');
  // P1：单基金穿透（行业分布，分母=该基金市值）
  const [lt, setLt] = useState<FundLookthroughResult | null>(null);

  // P1：单基金穿透懒加载（有披露数据才有意义；失败静默不阻塞页面）
  useEffect(() => {
    if (!code || !isTauri) return;
    let alive = true;
    lookthroughFund(code)
      .then((r) => { if (alive) setLt(r); })
      .catch(() => { /* 静默：无披露/货基时后端返回空壳 */ });
    return () => { alive = false; };
  }, [code]);

  // 订阅主题：切换浅/深色时重新读取设计令牌，使图表颜色与提示框同步。
  const { theme } = useTheme();
  // 触屏检测（pointer: coarse）→ 图表 Tooltip 改用 click 触发，适配移动端/触控屏。
  const isTouch = useIsTouch();
  // 窄屏（<md）：交易/估值拆解表收窄 min-w 并切换小一号字，桌面零回归
  const narrow = useNarrow();

  const chartColors = useMemo(
    () => ({
      primary: readColorVar('--color-primary'),
      gain: readColorVar('--color-gain'),
      loss: readColorVar('--color-loss'),
      warning: readColorVar('--color-warning'),
      muted: readColorVar('--color-muted'),
      border: readColorVar('--color-border'),
      surface: readColorVar('--color-surface'),
      foreground: readColorVar('--color-foreground'),
    }),
    // theme 变化时强制重算（readColorVar 读取的是运行时计算样式）。
    [theme],
  );

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await getFundDetail(code, platform);
      setData(r);
      if (isTauri) {
        try {
          setHoldingChanges(await getHoldingChanges(code));
        } catch {
          setHoldingChanges(null);
        }
      }
    } catch (e) {
      // 缺了这段时：命令失败 → setLoading(false) 不执行 → 页面永久停在「加载中…」。
      // 与 OverviewPage / StatsPage 同一范式：错误上屏 + 可重试，不吞异常。
      setError(e instanceof Error ? e.message : String(e));
      console.error('[FundLens] getFundDetail failed:', e);
    } finally {
      setLoading(false);
    }
  }, [code, platform]);

  // 载入（或按区间刷新）走势数据；缓存为空时自动尝试拉取一次。
  const loadSeries = useCallback(async () => {
    const r = await getFundSeries(code, range);
    setSeries(r);
    if (r.navPoints.length === 0 && isTauri && !autoRefreshed.current[`${code}:${range}`]) {
      autoRefreshed.current[`${code}:${range}`] = true;
      setNavRefreshing(true);
      try {
        await refreshNavHistory(code);
        const r2 = await getFundSeries(code, range);
        setSeries(r2);
      } catch {
        // 忽略，用户可手动刷新
      } finally {
        setNavRefreshing(false);
      }
    }
  }, [code, range]);

  const refreshSeries = useCallback(async () => {
    setNavRefreshing(true);
    try {
      await refreshNavHistory(code);
      const r = await getFundSeries(code, range);
      setSeries(r);
    } catch (e) {
      alert(`刷新净值走势失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setNavRefreshing(false);
    }
  }, [code, range]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    void loadSeries();
  }, [loadSeries]);

  const runAction = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      try {
        await fn();
        await load();
      } finally {
        setBusy(false);
      }
    },
    [load],
  );

  const handleDelete = useCallback(async () => {
    if (!confirm(`确定删除「${data?.fund.name}」及其持仓/披露记录吗？此操作不可撤销。`)) return;
    setBusy(true);
    try {
      await deleteFund(code);
      navigate('/overview');
    } catch (e) {
      alert(`删除失败：${e instanceof Error ? e.message : String(e)}`);
      setBusy(false);
    }
  }, [code, data, navigate]);

  // ---- 持仓份额内联编辑 ----
  const startEditShares = useCallback(() => {
    setSharesInput(String(data?.position.shares ?? 0));
    setEditingShares(true);
  }, [data]);

  const cancelEditShares = useCallback(() => {
    setEditingShares(false);
    setSharesInput('');
  }, []);

  const saveShares = useCallback(async () => {
    const v = parseFloat(sharesInput);
    if (!Number.isFinite(v) || v < 0) {
      alert('请输入有效的非负份额');
      return;
    }
    const newShares = Math.round(v * 100) / 100;
    // 保持单位成本不变：持仓成本随份额等比变化；市值/累计盈亏由后端按"份额×最新净值"重算。
    const newCost = (data?.position.avgCost ?? 0) * newShares;
    setEditingShares(false);
    setSharesInput('');
    await runAction(() => updatePosition(code, newShares, newCost, data?.fund.platform));
  }, [sharesInput, data, code, runAction]);

  // ---- 持仓成本价（单位成本）内联编辑 ----
  const startEditCost = useCallback(() => {
    setCostInput(String(data?.position.avgCost ?? 0));
    setEditingCost(true);
  }, [data]);

  const cancelEditCost = useCallback(() => {
    setEditingCost(false);
    setCostInput('');
  }, []);

  const saveCost = useCallback(async () => {
    const v = parseFloat(costInput);
    if (!Number.isFinite(v) || v < 0) {
      alert('请输入有效的非负成本价');
      return;
    }
    const newCostPrice = Math.round(v * 10000) / 10000;
    // 修改成本价：后端按「当前份额 × 成本价」重算持仓成本，只更新基线，不产生操作记录（不写流水）。
    setEditingCost(false);
    setCostInput('');
    await runAction(() => updatePositionCost(code, newCostPrice, data?.fund.platform));
  }, [costInput, data, code, runAction]);

  if (loading && !data) return <div className="p-6"><EmptyState title="加载中…" /></div>;
  if (error) return (
    <div className="p-6 space-y-3">
      <EmptyState title="加载失败" hint={error} />
      <button onClick={() => void load()} className="rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover">重试</button>
    </div>
  );
  if (!data) return <div className="p-6"><EmptyState title="未找到基金" hint={code} /></div>;

  const { fund, valuation, quotes, marketSession } = data;
  // 是否已抓取披露持仓（披露期非空且口径有效）
  const hasDisclosure =
    !!fund.reportPeriod && (fund.disclosureType === 'top10' || fund.disclosureType === 'full');
  const disclosureTypeLabel =
    fund.disclosureType === 'full' ? ' · 完整持仓' : fund.disclosureType === 'top10' ? ' · 前十大重仓' : '';
  // 本地持仓穿透自算估值（平台实时估值接口已停用），头条涨跌幅直接使用本地估值。
  const headlineNav = valuation.estNav;
  const headlinePct = valuation.estChangePct;
  const hasEstimate = valuation.estimated;
  // 基准性质标注：按基准指数名称判定宽基 / 行业 / 债券，而非 fund_type。
  const BROAD_BASE = ['沪深300', '中证500', '中证1000', '创业板指', '科创50', '上证50', '深证成指'];
  const benchmarkKind = !valuation.benchmarkName
    ? null
    : valuation.benchmarkName === '上证国债'
      ? '债券基准'
      : BROAD_BASE.includes(valuation.benchmarkName)
        ? '宽基基准'
        : '行业指数';
  // 指数/ETF 类基金：其未披露部分用的基准即「跟踪指数」，展示上明确点出，避免与沪深300 混为一谈。
  // 判定覆盖 指数型(008)/ETF联接(009)/分级(006) 及名称含 指数/ETF/联接：优先以估值引擎产出的
  // valuation_method==='index' 为准（指数实时估值优先），未估算时退回 fund_type_label 兜底。
  const isIndexFund = valuation.valuationMethod === 'index' || fund.fundTypeLabel === '指数型';
  const benchmarkLabel = !valuation.benchmarkName
    ? null
    : isIndexFund
      ? '跟踪指数'
      : benchmarkKind === '债券基准'
        ? '债券基准'
        : benchmarkKind === '宽基基准'
          ? '宽基基准'
          : '行业指数';

  // ---- 走势图数据准备 ----
  // ⚠️ v2.6.15 图表重做：X 轴改为「真正的时间轴」。每行带数值型时间键 t（UTC 毫秒），
  // XAxis 用 type="number" + domain=[dataMin,dataMax]，刻度位置只由日期决定，
  // 不再随净值点/交易点的疏密发生视觉变形（旧版按分类下标排布，日期疏密会被拉平）。
  // 交易/分红点**写入同一数据行**（与净值线共用 (t, nav)）→ 圆点必然精确落在净值线上。
  const navPoints = series?.navPoints ?? [];
  const costPoints = series?.costPoints ?? [];
  const markers = series?.txnMarkers ?? [];

  const navRows = buildNavChartRows(navPoints, markers);

  // 成本线 = 当前持仓均价（v9 后端 cost_points 输出两端同值，即水平横线），在净值图上画横向参考线。
  const costLevel = costPoints.length > 0 && costPoints[0].unitCost > 0 ? costPoints[0].unitCost : null;
  // 累计净值仅在确实与单位净值不同（发生过分红/拆分）时叠加，避免无意义重复曲线。
  const hasAccNav = navRows.some((p) => p.accNav > 0 && Math.abs(p.accNav - p.nav) > 1e-9);
  // Y 轴数值域：纳入净值/累计净值/成本线，上下留呼吸区，保证参考线与曲线均不被裁切。
  const yVals = navRows.flatMap((p) => (p.accNav > 0 ? [p.nav, p.accNav] : [p.nav]));
  if (costLevel != null) yVals.push(costLevel);
  const yLo = yVals.length > 0 ? Math.min(...yVals) : 0;
  const yHi = yVals.length > 0 ? Math.max(...yVals) : 1;
  const ySpan = yHi - yLo;
  const yPad = ySpan > 1e-9 ? ySpan * 0.08 : Math.max(yHi * 0.02, 0.01);
  // Y 轴取「整齐步长」并把域扩到步长整数倍：recharts 在自定义域上会在两端补不等距刻度，
  // 网格线间距就会忽宽忽窄。显式给出 yTicks 后，横向网格线严格等距。
  const niceStep = (span: number) => {
    if (!(span > 0)) return 0.01;
    const raw = span / 5; // 目标 5 个区间
    const mag = 10 ** Math.floor(Math.log10(raw));
    const n = raw / mag;
    const mult = n <= 1 ? 1 : n <= 2 ? 2 : n <= 2.5 ? 2.5 : n <= 5 ? 5 : 10;
    return mult * mag;
  };
  const yStep = niceStep(ySpan);
  const yDomain: [number, number] = [
    Math.max(0, Math.floor((yLo - yPad) / yStep) * yStep),
    Math.ceil((yHi + yPad) / yStep) * yStep,
  ];
  const yTickCount = Math.max(1, Math.round((yDomain[1] - yDomain[0]) / yStep));
  const yTicks = Array.from({ length: yTickCount + 1 }, (_, i) => +(yDomain[0] + i * yStep).toFixed(6));
  // Y 轴小数位随步长自适应（步长 ≥1 元 → 2 位 / ≥0.05 → 3 位 / 更小 → 4 位），避免标签互相挤压。
  const yDecimals = yStep >= 1 ? 2 : yStep >= 0.05 ? 3 : 4;

  // ---- 区间概览：这张图「说明了什么」直接写在图上方 ----
  const firstNav = navRows.length > 0 ? navRows[0].nav : 0;
  const lastNav = navRows.length > 0 ? navRows[navRows.length - 1].nav : 0;
  const rangeChangePct = firstNav > 0 ? (lastNav - firstNav) / firstNav : 0;
  const rangeHigh = navRows.length > 0 ? Math.max(...navRows.map((r) => r.nav)) : 0;
  const rangeLow = navRows.length > 0 ? Math.min(...navRows.map((r) => r.nav)) : 0;

  // X 轴刻度：按「日期等距」取固定数量刻度（不再按数据点密度），标签不会挤在一侧。
  const tMin = navRows.length > 0 ? navRows[0].t : 0;
  const tMax = navRows.length > 0 ? navRows[navRows.length - 1].t : 0;
  const spanDays = Math.max(1, Math.round((tMax - tMin) / DAY_MS));
  const tickCount = narrow ? 3 : 5;
  const xTicks = navRows.length <= 2 ? navRows.map((r) => r.t) : evenlySpacedTicks(tMin, tMax, tickCount);
  const fmtTimeTick = (v: number) => {
    const d = new Date(v);
    const mm = String(d.getUTCMonth() + 1).padStart(2, '0');
    const dd = String(d.getUTCDate()).padStart(2, '0');
    return spanDays > 330 ? `${d.getUTCFullYear()}-${mm}` : `${mm}-${dd}`;
  };

  return (
    <div className="p-4 sm:p-6 space-y-5">
      <Link to="/overview" className="inline-flex items-center gap-1 text-sm text-muted hover:text-primary">
        <ArrowLeft size={16} aria-hidden /> 返回总览
      </Link>

      <header className="flex items-start justify-between">
        <div>
          <h1 className="text-xl font-semibold">{fund.name}</h1>
          <div className="mt-1 flex flex-wrap items-center gap-2 text-sm text-muted">
            <span className="tnum">{fund.code}</span>
            <PlatformBadge code={fund.platform} />
            {fund.fundTypeLabel && fund.fundTypeLabel !== '未知' && (
              <span className="rounded bg-border/60 px-1.5 py-0.5 text-xs">{fund.fundTypeLabel}</span>
            )}
            {hasDisclosure ? (
              <span>
                披露期：{fund.reportPeriod}
                {disclosureTypeLabel}
              </span>
            ) : (
              <span className="text-muted">未抓取披露持仓</span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void runAction(() => fetchDisclosure(code))}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-background/60 disabled:opacity-50"
          >
            <Download size={16} className={busy ? 'animate-spin' : ''} aria-hidden />
            抓取披露持仓
          </button>
          <button
            onClick={() =>
              void runAction(async () => {
                const r = await fetchDisclosureHistory(code, 8);
                setBackfillMsg(
                  r.storedPeriods.length > 0
                    ? `已补录 ${r.storedPeriods.length} 个期次：${r.storedPeriods.join('、')}；当前共 ${r.allPeriods.length} 期`
                    : '近 8 期均无更早披露数据，历史已是最新',
                );
                setHoldingChanges(await getHoldingChanges(code));
              })
            }
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-background/60 disabled:opacity-50"
          >
            <History size={16} className={busy ? 'animate-spin' : ''} aria-hidden />
            补录历史持仓
          </button>
          <button
            onClick={() => void runAction(() => refreshQuotes())}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-background/60 disabled:opacity-50"
          >
            <RefreshCw size={16} className={busy ? 'animate-spin' : ''} aria-hidden />
            刷新行情
          </button>
          <button
            onClick={() => void handleDelete()}
            disabled={busy}
            className="inline-flex items-center gap-1.5 rounded-md border border-danger/50 px-3 py-1.5 text-sm text-danger hover:bg-danger/10 disabled:opacity-50"
          >
            <Trash2 size={16} aria-hidden />
            删除
          </button>
        </div>
      </header>

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatTile label="官方净值" value={fund.officialNav.toFixed(4)} />
        <StatTile
          label="估算净值"
          value={hasEstimate ? headlineNav.toFixed(4) : '—'}
          tone={hasEstimate && headlinePct > 0 ? 'gain' : hasEstimate && headlinePct < 0 ? 'loss' : 'neutral'}
        />
        <StatTile
          label="估算涨跌"
          value={hasEstimate ? <GainLossBadge value={headlinePct} format="pct" /> : '—'}
          tone={hasEstimate && headlinePct > 0 ? 'gain' : hasEstimate && headlinePct < 0 ? 'loss' : 'neutral'}
        />
        <StatTile label="披露占比" value={`${(valuation.disclosedWeightSum * 100).toFixed(1)}%`} />
      </div>

      {/* ===== 我的持仓（业界标准指标，与总览页同口径） ===== */}
      <Card
        title={
          <span className="inline-flex items-center gap-2">
            我的持仓
            <span
              className={
                'rounded px-1.5 py-0.5 text-xs ' +
                (marketSession === 'intraday'
                  ? 'bg-primary/10 text-primary'
                  : marketSession === 'post_close'
                    ? 'bg-success/10 text-success'
                    : 'bg-border/60 text-muted')
              }
            >
              {marketSession === 'intraday' ? '盘中·估算' : marketSession === 'post_close' ? '盘后·实际' : '休市·上一交易日'}
            </span>
          </span>
        }
      >
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 md:grid-cols-3">
          {editingShares ? (
            <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
              <div className="text-xs text-muted mb-1">持仓份额</div>
              <div className="flex items-center gap-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.01"
                  value={sharesInput}
                  autoFocus
                  onChange={(e) => setSharesInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void saveShares();
                    else if (e.key === 'Escape') cancelEditShares();
                  }}
                  className="tnum w-28 rounded border border-border bg-background px-2 py-1 text-xl font-semibold focus:outline-none focus:ring-1 focus:ring-primary"
                  aria-label="编辑持仓份额"
                />
                <button
                  onClick={() => void saveShares()}
                  disabled={busy}
                  className="rounded-md border border-primary px-2 py-1 text-xs text-primary hover:bg-primary/10 disabled:opacity-50"
                >
                  保存
                </button>
                <button
                  onClick={cancelEditShares}
                  disabled={busy}
                  className="rounded-md border border-border px-2 py-1 text-xs text-muted hover:bg-background/60 disabled:opacity-50"
                >
                  取消
                </button>
              </div>
            </div>
          ) : (
            <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
              <div className="flex items-center justify-between gap-2 mb-1">
                <span className="text-xs text-muted">持仓份额</span>
                <button
                  onClick={startEditShares}
                  disabled={busy}
                  className="inline-flex items-center text-muted hover:text-primary disabled:opacity-50"
                  aria-label="编辑持仓份额"
                  title="编辑份额"
                >
                  <Pencil size={13} aria-hidden />
                </button>
              </div>
              <div className="tnum text-xl font-semibold text-foreground">
                {data.position.shares.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}
              </div>
            </div>
          )}
          {editingCost ? (
            <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
              <div className="text-xs text-muted mb-1">单位成本</div>
              <div className="flex items-center gap-1.5">
                <input
                  type="number"
                  min="0"
                  step="0.0001"
                  value={costInput}
                  autoFocus
                  onChange={(e) => setCostInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void saveCost();
                    else if (e.key === 'Escape') cancelEditCost();
                  }}
                  className="tnum w-28 rounded border border-border bg-background px-2 py-1 text-xl font-semibold focus:outline-none focus:ring-1 focus:ring-primary"
                  aria-label="编辑单位成本"
                />
                <button
                  onClick={() => void saveCost()}
                  disabled={busy}
                  className="rounded-md border border-primary px-2 py-1 text-xs text-primary hover:bg-primary/10 disabled:opacity-50"
                >
                  保存
                </button>
                <button
                  onClick={cancelEditCost}
                  disabled={busy}
                  className="rounded-md border border-border px-2 py-1 text-xs text-muted hover:bg-background/60 disabled:opacity-50"
                >
                  取消
                </button>
              </div>
            </div>
          ) : (
            <div className="bg-surface border border-border rounded-md p-4 shadow-ring">
              <div className="flex items-center justify-between gap-2 mb-1">
                <span className="text-xs text-muted">单位成本</span>
                <button
                  onClick={startEditCost}
                  disabled={busy}
                  className="inline-flex items-center text-muted hover:text-primary disabled:opacity-50"
                  aria-label="编辑单位成本"
                  title="编辑成本价（按份额×成本价重算持仓成本，不产生操作记录）"
                >
                  <Pencil size={13} aria-hidden />
                </button>
              </div>
              <div className="tnum text-xl font-semibold text-foreground">
                {data.position.avgCost.toFixed(4)}
              </div>
            </div>
          )}
          <StatTile label="持仓成本" value={`¥${data.position.costAmount.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`} />
          <StatTile
            label="市值"
            value={`¥${data.position.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`}
          />
          <StatTile
            label="累计盈亏"
            value={<GainLossBadge value={data.position.totalPnl} format="amount" />}
            tone={data.position.totalPnl > 0 ? 'gain' : data.position.totalPnl < 0 ? 'loss' : 'neutral'}
            sublabel={<span className="tnum">{data.position.totalPnlPct > 0 ? '+' : ''}{(data.position.totalPnlPct * 100).toFixed(2)}%</span>}
          />
          <StatTile
            label="当日收益"
            value={<GainLossBadge value={data.position.dayPnl} format="amount" />}
            tone={data.position.dayPnl > 0 ? 'gain' : data.position.dayPnl < 0 ? 'loss' : 'neutral'}
            sublabel={
              <span className="flex items-center gap-1 tnum">
                <span className={`rounded border px-1 py-0.5 text-xs font-normal ${data.position.dayIsToday ? 'text-success border-success/40 bg-success/10' : 'text-primary border-primary/40 bg-primary/10'}`}>
                  {data.position.dayIsToday ? '当日实际' : (data.position.lastNavDate ? mmdd(data.position.lastNavDate) : '实际')}
                </span>
                {data.position.dayPnlPct > 0 ? '+' : ''}
                {(data.position.dayPnlPct * 100).toFixed(2)}%
              </span>
            }
          />
          {data.position.estimated && (
            <StatTile
              label={marketSession === 'intraday' ? '当日估算收益' : (data.position.lastNavDate ? `估算收益 ${mmdd(data.position.lastNavDate)}` : '估算收益')}
              value={
                marketSession === 'intraday'
                  ? <GainLossBadge value={data.position.dayPnlEst} format="amount" />
                  : data.position.lastDayPnlEst != null
                    ? <GainLossBadge value={data.position.lastDayPnlEst} format="amount" />
                    : '—'
              }
              tone={
                marketSession === 'intraday'
                  ? (data.position.dayPnlEst > 0 ? 'gain' : data.position.dayPnlEst < 0 ? 'loss' : 'neutral')
                  : (data.position.lastDayPnlEst != null
                    ? (data.position.lastDayPnlEst > 0 ? 'gain' : data.position.lastDayPnlEst < 0 ? 'loss' : 'neutral')
                    : 'neutral')
              }
            />
          )}
        </div>
        {!data.position.estimated && (
          <p className="mt-2 text-xs text-muted">
            货币/理财型：净值恒定≈1，仅展示累计持有收益，无当日浮动估算。
          </p>
        )}
      </Card>

      {!hasEstimate && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          {valuation.reason ?? '无法估算'}
        </div>
      )}

      {hasEstimate && (
        <Card title="估值透明度 · 核心差异化">
          <div className="space-y-4">
            <div className="flex flex-wrap items-center gap-2">
              <SourceBadge source={data.valuationSource} />
            </div>
            <CoverageBar covered={valuation.disclosedWeightSum} benchmark={valuation.benchmarkWeight ?? 0} />
            {valuation.benchmarkName && (
              <p className="text-xs text-muted">
                {benchmarkLabel === '跟踪指数' ? (
                  <>
                    跟踪指数：
                    <strong className="text-foreground">{valuation.benchmarkName}</strong>
                    （指数型基金按该指数当日涨跌计算），覆盖未披露仓位（现金 / 债券 / 非前十大）占净值{' '}
                    <strong className="text-foreground">
                      {((valuation.benchmarkWeight ?? 0) * 100).toFixed(1)}%
                    </strong>
                    。
                  </>
                ) : (
                  <>
                    基准近似来源：
                    <strong className="text-foreground">{valuation.benchmarkName}</strong>
                    {benchmarkLabel ? `（${benchmarkLabel}）` : ''}
                    ，占净值{' '}
                    <strong className="text-foreground">
                      {((valuation.benchmarkWeight ?? 0) * 100).toFixed(1)}%
                    </strong>
                    ，用于近似未披露仓位（现金 / 债券 / 非前十大）。
                  </>
                )}
              </p>
            )}
            <p className="rounded-md bg-background/60 border border-border px-3 py-2 text-xs text-muted tnum leading-relaxed">
              {valuation.valuationMethod === 'index' ? (
                <>
                  指数型基金：估算净值 = 官方净值 × (1 + <strong className="text-foreground">「{valuation.benchmarkName ?? '跟踪指数'}」当日涨跌</strong>)，
                  成分股穿透（<GainLossBadge value={valuation.penetrationEstChangePct ?? 0} format="pct" />）仅作<strong className="text-foreground">参考口径</strong>。
                </>
              ) : (
                <>估算净值 = 官方净值 × (1 + Σ 披露占比ᵢ × 个股当日涨跌ᵢ + 未覆盖占比 × 基准指数当日涨跌)</>
              )}
            </p>
          </div>
        </Card>
      )}

      {marketSession !== 'intraday' && hasEstimate && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          非交易时段：个股现价=最新收盘价，估算基于当日涨跌幅（≈ 下一交易日官方净值变动），仅供参考。
        </div>
      )}

      {data.delayNote && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          {data.delayNote === 'T+1·海外交易中'
            ? 'QDII 基金：海外市场交易中，平台实时估值仍在形成、非终值，暂不展示「当日」收益；下方估算为上一海外交易日净值变动（T+1）。'
            : 'QDII 基金：净值 T+1/T+2 确认，下方估算反映上一海外交易日变动，并非 A 股当日涨跌。'}
        </div>
      )}

      <Card title="估值口径">
        <div className="space-y-2 text-sm">
          <div className="flex items-center justify-between gap-3">
            <span className="text-muted">
              {valuation.valuationMethod === 'index' ? '指数实时估值（跟踪指数）' : '持仓穿透估值（本地·含基准近似）'}
            </span>
            {valuation.estimated && data.delayNote !== 'T+1·海外交易中' ? (
              <GainLossBadge value={valuation.estChangePct} format="pct" />
            ) : (
              <span className="text-muted">—</span>
            )}
          </div>
          {valuation.benchmarkName && (
            <p className="text-xs text-muted">
              未披露部分（占净值 <strong className="text-foreground">{((valuation.benchmarkWeight ?? 0) * 100).toFixed(1)}%</strong>）按
              {benchmarkLabel === '跟踪指数' ? ' 跟踪指数 ' : benchmarkLabel === '宽基基准' ? ' 宽基基准 ' : benchmarkLabel === '债券基准' ? ' 债券基准 ' : ' 标的/行业指数 '}
              <strong className="text-foreground"> {valuation.benchmarkName} </strong>
              当日涨跌 <GainLossBadge value={valuation.benchmarkReturn ?? 0} format="pct" /> 近似。
            </p>
          )}
          {valuation.estimated &&
            !valuation.benchmarkName &&
            (valuation.benchmarkWeight ?? 0) > 0.03 && (
              <p className="text-xs text-warning">
                未披露部分（占净值{' '}
                <strong className="text-foreground">{((valuation.benchmarkWeight ?? 0) * 100).toFixed(1)}%</strong>
                ）暂无基准/跟踪指数行情，当前估算把该部分按「零波动」近似，实际可能被低估/高估（P2-11 提示）。
              </p>
            )}
        </div>
      </Card>

      <Card title="透明计算 · 非黑箱">
        <p className="text-xs text-muted leading-relaxed">
          本基金的估值为<strong className="text-foreground">本地基于你导入的披露持仓 + 公开个股行情</strong>透明计算，口径完全摊开，不依赖任何第三方「估值服务」：
        </p>
        <div className="mt-2 rounded-md bg-background/60 border border-border px-3 py-2 text-sm tnum">
          估算净值 = 官方净值 × (1 + Σ 披露持仓占比ᵢ × 个股当日涨跌ᵢ + (1 − 覆盖度) × 基准指数当日涨跌)
        </div>
        <p className="mt-2 text-xs text-muted leading-relaxed">
          覆盖度越高（见上方指标），估算越贴近真实；未披露仓位（现金/债券/非前十大）按<strong className="text-foreground">对应基准指数</strong>近似（指数/ETF用其跟踪指数、债券用国债指数、其余用沪深300），<strong className="text-foreground">绝不假装精确</strong>。
          盘中为估算、盘后为当日实际，仅供参考，<strong className="text-foreground">非投资建议</strong>。
        </p>
      </Card>

      <Card title="估值拆解 — 披露持仓贡献">
        <div className="overflow-x-auto">
        <table className={`w-full min-w-[420px] sm:min-w-[520px] ${narrow ? 'text-xs' : 'text-sm'}`}>
          <thead>
              <tr className="text-left text-xs text-muted border-b border-border">
                <th className="py-2 pr-3 font-medium">个股</th>
                <th className="py-2 pr-3 font-medium text-right">占净值</th>
                <th className="py-2 pr-3 font-medium text-right">现价</th>
                <th className="py-2 pr-3 font-medium text-right">昨收</th>
                <th className="py-2 pr-3 font-medium text-right">当日涨跌</th>
                <th className="py-2 pr-3 font-medium text-right">对净值贡献</th>
                <th className="py-2 pr-3 font-medium text-right whitespace-nowrap">
                  较上期{holdingChanges?.hasPrev && holdingChanges.prevPeriod ? `（${holdingChanges.prevPeriod}）` : ''}
                </th>
              </tr>
            </thead>
            <tbody>
              {valuation.holdings.map((h) => {
                const q = quotes.find((x) => x.stockCode === h.stockCode);
                return (
                  <tr key={h.stockCode} className="border-b border-border/60 last:border-0">
                    <td className="py-2.5 pr-3">
                      {h.stockName}
                      <span className="ml-2 text-xs text-muted tnum">{h.stockCode}</span>
                    </td>
                    <td className="py-2.5 pr-3 text-right tnum">{(h.weight * 100).toFixed(2)}%</td>
                    <td className="py-2.5 pr-3 text-right tnum">{q ? q.price.toFixed(2) : '—'}</td>
                    <td className="py-2.5 pr-3 text-right tnum">{q ? q.prevClose.toFixed(2) : '—'}</td>
                    <td className="py-2.5 pr-3 text-right"><GainLossBadge value={h.priceReturn} format="pct" /></td>
                    <td className="py-2.5 pr-3 text-right"><GainLossBadge value={h.contribution} format="pct" /></td>
                    <td className="py-2.5 pr-3 text-right">
                      {renderHoldingChange(
                        holdingChanges?.changes.find((c) => c.stockCode === h.stockCode),
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <p className="mt-3 text-xs text-muted">
          估值公式：估算净值 = 官方净值 × (1 + Σ 占比ᵢ × (现价ᵢ / 昨收ᵢ − 1))。未披露部分（现金/债券/非前十大）按基准指数当日涨跌近似。
        </p>
        {backfillMsg && <p className="mt-1 text-xs text-primary">{backfillMsg}</p>}
        {hasDisclosure && holdingChanges && !holdingChanges.hasPrev && (
          <p className="mt-1 text-xs text-muted">
            当前仅有 {holdingChanges.currPeriod || fund.reportPeriod} 一期披露，无「上期」可对比。点击「补录历史持仓」可抓取更早期次。
          </p>
        )}
      </Card>

      {/* ===== P1 单基金穿透（行业分布，分母=该基金市值） ===== */}
      {lt && lt.industriesL1.length > 0 && lt.coverage > 0 && (
        <Card
          title="基金穿透 · 行业分布"
          action={
            <span className="tnum rounded border border-border bg-border/40 px-1.5 py-0.5 text-xs text-muted">
              覆盖率 {(lt.coverage * 100).toFixed(0)}% · {lt.reportPeriod ?? '—'}
            </span>
          }
        >
          <div className="space-y-1">
            {lt.industriesL1.map((sl) => {
              const max = Math.max(...lt.industriesL1.map((x) => x.pct), 1e-9);
              return (
                <div key={sl.key} className="flex items-center gap-2 text-sm">
                  <div className="w-24 shrink-0 truncate text-right" title={sl.key}>{sl.key}</div>
                  <div className="h-4 min-w-0 flex-1">
                    <div
                      className={`h-full rounded-sm ${sl.isVirtual ? 'border border-dashed border-border' : ''}`}
                      style={
                        sl.isVirtual
                          ? undefined
                          : {
                              width: `${Math.max((sl.pct / max) * 100, 1.5)}%`,
                              background: 'color-mix(in srgb, var(--color-primary) 55%, transparent)',
                            }
                      }
                      aria-hidden
                    />
                  </div>
                  <div className="tnum w-14 shrink-0 text-right font-medium">{(sl.pct * 100).toFixed(1)}%</div>
                </div>
              );
            })}
          </div>
          {lt.topStocks.length > 0 && (
            <div className="mt-3 border-t border-border/60 pt-2">
              <div className="mb-1 text-xs font-medium text-muted">穿透前十大重仓</div>
              <div className="flex flex-wrap gap-1.5">
                {lt.topStocks.map((st) => (
                  <span key={st.stockCode} className="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
                    {st.stockName}
                    <span className="tnum ml-1 text-muted">{(st.pct * 100).toFixed(1)}%</span>
                  </span>
                ))}
              </div>
            </div>
          )}
          <p className="mt-2 text-xs text-muted">
            分母 = 该基金市值（{lt.fundName}）；口径与「基金穿透」页一致：披露权重直用、不放大，未穿透部分单列。
          </p>
        </Card>
      )}

      {/* ===== 交易记录 ===== */}
      <Card title="交易记录">
        {data.transactions.length === 0 ? (
          <EmptyState title="暂无交易记录" hint="导入交易截图或手动记账后，该基金的所有买卖/分红将在此展示" />
        ) : (
          <div className="overflow-x-auto">
            <table className={`w-full min-w-[420px] sm:min-w-[520px] ${narrow ? 'text-xs' : 'text-sm'}`}>
              <thead>
                <tr className="text-left text-xs text-muted border-b border-border">
                  <th className="py-2 pr-3 font-medium whitespace-nowrap">日期</th>
                  <th className="py-2 pr-3 font-medium">类型</th>
                  <th className="py-2 pr-3 font-medium text-right">份额</th>
                  <th className="py-2 pr-3 font-medium text-right">金额</th>
                  <th className="py-2 pr-3 font-medium text-right">价格</th>
                  <th className="py-2 pr-3 font-medium">来源</th>
                </tr>
              </thead>
              <tbody>
                {data.transactions.map((t) => (
                  <tr key={t.id} className="border-b border-border/60 last:border-0">
                    <td className="py-2.5 pr-3 tnum whitespace-nowrap">
                      {t.txnDate}
                      {t.txnTime ? <span className="text-muted"> {t.txnTime}</span> : null}
                    </td>
                    <td className="py-2.5 pr-3">
                      <TxnTag type={t.txnType} />
                    </td>
                    <td className="py-2.5 pr-3 text-right tnum">{t.shares != null ? t.shares.toFixed(2) : '—'}</td>
                    <td className="py-2.5 pr-3 text-right tnum">¥{t.amount.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                    <td className="py-2.5 pr-3 text-right tnum">{t.price != null && t.price > 0 ? t.price.toFixed(4) : '—'}</td>
                    <td className="py-2.5 pr-3 text-xs text-muted">{sourceLabel(t.source)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {/* ===== 基金净值走势图 ===== */}
      <Card
        title={
          <span className="inline-flex items-center gap-1.5">
            <LineChartIcon size={15} aria-hidden /> 净值走势
          </span>
        }
      >
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-1.5">
            {RANGES.map((r) => (
              <button
                key={r.key}
                onClick={() => setRange(r.key)}
                className={
                  'rounded-md border px-2.5 py-1 text-xs transition-colors ' +
                  (range === r.key
                    ? 'border-primary text-primary'
                    : 'border-border text-muted hover:text-foreground')
                }
              >
                {r.label}
              </button>
            ))}
          </div>
          <button
            onClick={() => void refreshSeries()}
            disabled={navRefreshing}
            className="inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1 text-xs text-muted hover:text-foreground disabled:opacity-50"
          >
            <RefreshCw size={13} className={navRefreshing ? 'animate-spin' : ''} aria-hidden />
            刷新
          </button>
        </div>

        {navRows.length === 0 ? (
          <EmptyState
            title={navRefreshing ? '正在拉取历史净值…' : '暂无净值数据'}
            hint={navRefreshing ? '' : '点击右上角「刷新」自动拉取东财历史净值'}
          />
        ) : (
          <>
            {/* 区间概览：把「这张图说明了什么」放在图上方，不必自己数格子 */}
            <div className="mb-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
              <span className="inline-flex items-center gap-1.5">
                区间涨跌
                <GainLossBadge value={rangeChangePct} format="pct" />
              </span>
              {navRows.length > 1 && (
                <span className="tnum">
                  区间最高 <span className="text-foreground">{rangeHigh.toFixed(4)}</span>
                  <span className="mx-1.5 opacity-40">|</span>
                  最低 <span className="text-foreground">{rangeLow.toFixed(4)}</span>
                </span>
              )}
              <span className="tnum">{navRows.length} 个净值日</span>
            </div>
            <ResponsiveContainer width="100%" height={narrow ? 230 : 300}>
              <NavChart
                rows={navRows}
                xTicks={xTicks}
                yDomain={yDomain}
                yTicks={yTicks}
                yDecimals={yDecimals}
                costLevel={costLevel}
                hasAccNav={hasAccNav}
                isTouch={isTouch}
                narrow={narrow}
                colors={chartColors}
                tickFormatter={fmtTimeTick}
              />
            </ResponsiveContainer>
            <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted">
              <span className="inline-flex items-center gap-1.5"><KeySwatch kind="line" color={chartColors.primary} /> 单位净值</span>
              {hasAccNav && (
                <span className="inline-flex items-center gap-1.5"><KeySwatch kind="dash" color={chartColors.muted} /> 累计净值</span>
              )}
              {costLevel != null && (
                <span className="inline-flex items-center gap-1.5"><KeySwatch kind="dash" color={chartColors.warning} /> 持仓成本 {costLevel.toFixed(4)}</span>
              )}
              <span className="inline-flex items-center gap-1.5"><KeySwatch kind="dotFilled" color={chartColors.gain} ring={chartColors.surface} /> 买入</span>
              <span className="inline-flex items-center gap-1.5"><KeySwatch kind="dotHollow" color={chartColors.loss} ring={chartColors.surface} /> 卖出</span>
              <span className="inline-flex items-center gap-1.5"><KeySwatch kind="dotSmall" color={chartColors.warning} ring={chartColors.surface} /> 分红</span>
            </div>
            <p className="mt-1.5 text-xs text-muted/80">
              横轴按日期等距（非交易日不占位）；圆点即交易/分红日，实心=买入、空心=卖出、小圆=分红，均落在当日净值线上
              （同日买卖显示为「买入点 + 外圈卖出环」）
              {costLevel != null ? '；净值高于成本线即持仓浮盈' : ''}
            </p>
            {navRows.length === 1 && (
              <p className="mt-2 rounded-md border border-border bg-background/60 px-3 py-2 text-xs text-muted">
                目前仅记录到 1 个净值日（最近一次刷新写入）。每天打开「持仓总览」会自动积累，多日后走势完整显示；
                也可点击右上角「刷新」尝试拉取历史净值。
              </p>
            )}
          </>
        )}
      </Card>

    </div>
  );
}
