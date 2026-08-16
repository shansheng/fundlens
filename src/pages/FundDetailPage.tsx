// 单基金详情页 — 估值拆解：披露持仓对净值的贡献 + 当日行情
// 新增：基金净值走势图（含买入/卖出/分红点）+ 持仓成本走势图
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useParams, Link, useNavigate } from 'react-router-dom';
import { ArrowLeft, CircleAlert, Download, RefreshCw, Trash2, LineChart as LineChartIcon, TrendingUp } from 'lucide-react';
import {
  ComposedChart,
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
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
  isTauri,
  type FundDetailResult,
  type FundSeries,
  type NavPoint,
} from '../api';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, PlatformBadge, EmptyState, ConfidenceBadge } from '../components/ui';

// 从设计令牌（CSS 变量）读取图表颜色，避免源码硬编码 hex（P0 合规）。
// 运行时读取后转为 rgb() 字符串传给 recharts（SVG 属性对 var() 支持不稳定）。
function readColorVar(name: string): string {
  if (typeof window === 'undefined') return 'currentColor';
  const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return raw ? `rgb(${raw})` : 'currentColor';
}

// 交易标记形状（买入▲红 / 卖出▼绿 / 分红◆琥珀），纯 SVG，不使用 emoji。
const UpTriangle = (props: { cx?: number; cy?: number; fill?: string }) => {
  const { cx = 0, cy = 0, fill } = props;
  return <polygon points={`${cx},${cy - 6} ${cx - 5},${cy + 5} ${cx + 5},${cy + 5}`} fill={fill} stroke="#fff" strokeWidth={0.6} />;
};
const DownTriangle = (props: { cx?: number; cy?: number; fill?: string }) => {
  const { cx = 0, cy = 0, fill } = props;
  return <polygon points={`${cx},${cy + 6} ${cx - 5},${cy - 5} ${cx + 5},${cy - 5}`} fill={fill} stroke="#fff" strokeWidth={0.6} />;
};
const Diamond = (props: { cx?: number; cy?: number; fill?: string }) => {
  const { cx = 0, cy = 0, fill } = props;
  return <polygon points={`${cx},${cy - 6} ${cx - 5},${cy} ${cx},${cy + 6} ${cx + 5},${cy}`} fill={fill} stroke="#fff" strokeWidth={0.6} />;
};

// 取某交易日期对应的「最近一个交易日净值点」（向前取），返回该净值点的日期与净值。
// 交易日期本身可能是周末/非交易日，必须映射到真实存在的净值轴分类，买卖点才能精确落在净值线上。
function navPointAt(navPoints: NavPoint[], date: string): { date: string; nav: number } {
  if (navPoints.length === 0) return { date, nav: 0 };
  let result = { date: navPoints[0].date, nav: navPoints[0].nav };
  for (const p of navPoints) {
    if (p.date <= date) result = { date: p.date, nav: p.nav };
    else break;
  }
  return result;
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
  return (
    <span className="rounded bg-border/60 px-1.5 py-0.5 text-xs text-foreground">
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

export default function FundDetailPage() {
  const { code = '' } = useParams();
  const navigate = useNavigate();
  const [data, setData] = useState<FundDetailResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);

  // 净值/成本走势
  const [range, setRange] = useState('all');
  const [series, setSeries] = useState<FundSeries | null>(null);
  const [navRefreshing, setNavRefreshing] = useState(false);
  const autoRefreshed = useRef<Record<string, boolean>>({});

  const chartColors = useMemo(
    () => ({
      primary: readColorVar('--color-primary'),
      gain: readColorVar('--color-gain'),
      loss: readColorVar('--color-loss'),
      warning: readColorVar('--color-warning'),
      muted: readColorVar('--color-muted'),
      border: readColorVar('--color-border'),
      foreground: readColorVar('--color-foreground'),
    }),
    [],
  );

  const load = useCallback(async () => {
    setLoading(true);
    const r = await getFundDetail(code);
    setData(r);
    setLoading(false);
  }, [code]);

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

  // 成本图：以净值时间轴为骨架，按交易日期向前携带成本状态，形成阶梯线；右轴叠加净值/单位成本。
  // 注意：此 useMemo 必须放在任何提前 return 之前，否则加载态(hooks 少)与数据态(hooks 多)的
  // Hook 数量不一致，会触发 "Rendered more hooks than during the previous render" 报错。
  const costMerged = useMemo(() => {
    const navPts = series?.navPoints ?? [];
    const costPts = series?.costPoints ?? [];
    const sortedCost = [...costPts].sort((a, b) => (a.date < b.date ? -1 : 1));
    let ci = 0;
    return navPts.map((nav) => {
      while (ci < sortedCost.length && sortedCost[ci].date <= nav.date) ci += 1;
      const cur = ci > 0 ? sortedCost[ci - 1] : null;
      return {
        date: nav.date,
        nav: nav.nav,
        cumulativeCost: cur ? cur.cumulativeCost : 0,
        unitCost: cur ? cur.unitCost : 0,
      };
    });
  }, [series]);

  if (loading && !data) return <div className="p-6"><EmptyState title="加载中…" /></div>;
  if (!data) return <div className="p-6"><EmptyState title="未找到基金" hint={code} /></div>;

  const { fund, valuation, quotes, trading, realtimeEstimate } = data;
  // 是否已抓取披露持仓（披露期非空且口径有效）
  const hasDisclosure =
    !!fund.reportPeriod && (fund.disclosureType === 'top10' || fund.disclosureType === 'full');
  const disclosureTypeLabel =
    fund.disclosureType === 'full' ? ' · 完整持仓' : fund.disclosureType === 'top10' ? ' · 前十大重仓' : '';
  // 优先采用盘中实时估值（平台直接计算）作为头条涨跌幅；否则回落本地自算估值
  const hasRealtime = !!realtimeEstimate;
  const headlineNav = hasRealtime ? realtimeEstimate!.estNav : valuation.estNav;
  const headlinePct = hasRealtime ? realtimeEstimate!.estChangePct : valuation.estChangePct;
  const hasEstimate = hasRealtime || valuation.estimated;
  // 基准性质标注：按基准指数名称判定宽基 / 行业 / 债券，而非 fund_type。
  const BROAD_BASE = ['沪深300', '中证500', '中证1000', '创业板指', '科创50', '上证50', '深证成指'];
  const benchmarkKind = !valuation.benchmarkName
    ? null
    : valuation.benchmarkName === '上证国债'
      ? '债券基准'
      : BROAD_BASE.includes(valuation.benchmarkName)
        ? '宽基基准'
        : '行业指数';

  // ---- 走势图数据准备 ----
  const navPoints = series?.navPoints ?? [];
  const costPoints = series?.costPoints ?? [];
  const markers = series?.txnMarkers ?? [];

  // 净值图上叠加的买卖/分红点（映射到对应日期最近一个交易日的净值点，确保精确落在净值线）
  const buyData = markers.filter((m) => m.txnType === 'buy').map((m) => navPointAt(navPoints, m.date));
  const sellData = markers.filter((m) => m.txnType === 'sell').map((m) => navPointAt(navPoints, m.date));
  const divData = markers.filter((m) => m.txnType === 'dividend').map((m) => navPointAt(navPoints, m.date));

  const fmtDateTick = (v: string) => (typeof v === 'string' && v.length >= 10 ? v.slice(5) : v);
  const fmtMoney = (v: number) =>
    Number(v).toLocaleString('zh-CN', { maximumFractionDigits: 0 });
  const tooltipFormatter = (value: number, name?: string | number) => {
    const n = typeof name === 'string' ? name : '';
    if (n === '单位净值' || n === '累计净值' || n === '净值' || n === '单位成本') {
      return Number(value).toFixed(4);
    }
    return Number(value).toLocaleString('zh-CN', { maximumFractionDigits: 2 });
  };

  return (
    <div className="p-6 space-y-5">
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
        <StatTile label="披露占比" value={hasRealtime ? '实时估值' : `${(valuation.disclosedWeightSum * 100).toFixed(1)}%`} />
      </div>

      {hasRealtime && (
        <div className="text-xs text-muted">
          采用盘中实时估值（天天基金 @ {realtimeEstimate!.gztime}）
          {data.delayNote && ' · QDII 为上一海外交易日估值（T+1）'}
        </div>
      )}

      {hasEstimate && !hasRealtime && (
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
          <span>
            估算覆盖度 <strong className="text-foreground">{(valuation.disclosedWeightSum * 100).toFixed(1)}%</strong>
            （前几大重仓占净值）
          </span>
          <span className="text-border">·</span>
          <span>未覆盖部分按基准指数近似</span>
        </div>
      )}

      {!hasEstimate && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          {valuation.reason ?? '无法估算'}
        </div>
      )}

      {!trading && hasEstimate && (
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

      <Card title="双口径估算 · 交叉验证">
        <div className="space-y-2 text-sm">
          <div className="flex items-center justify-between gap-3">
            <span className="text-muted">平台实时估值{hasRealtime ? `（@ ${realtimeEstimate!.gztime}）` : '（非交易时段）'}</span>
            {hasRealtime ? (
              <GainLossBadge value={realtimeEstimate!.estChangePct} format="pct" />
            ) : (
              <span className="text-muted">—</span>
            )}
          </div>
          <div className="flex items-center justify-between gap-3">
            <span className="text-muted">持仓穿透估值（本地·含基准近似）</span>
            {valuation.estimated ? (
              <GainLossBadge value={valuation.estChangePct} format="pct" />
            ) : (
              <span className="text-muted">—</span>
            )}
          </div>
          <div className="flex items-center justify-between gap-3 border-t border-border pt-2">
            <span className="text-muted">交叉验证置信度</span>
            <ConfidenceBadge level={valuation.confidence} />
          </div>
          {valuation.divergence !== undefined && valuation.divergence > 0 && (
            <p className="text-xs text-muted">
              两口径分歧 <strong className="text-foreground">{((valuation.divergence ?? 0) * 100).toFixed(2)}</strong> 个百分点
              {valuation.confidence === 'low' ? '：差异较大，估算谨慎参考' : ''}
            </p>
          )}
          {valuation.consensusEstChangePct !== undefined && valuation.consensusEstChangePct !== null && (
            <div className="flex items-center justify-between gap-3 border-t border-border pt-2">
              <span className="text-muted">多源共识（平台×0.6 ＋ 穿透×0.4）</span>
              <GainLossBadge value={valuation.consensusEstChangePct} format="pct" />
            </div>
          )}
          {valuation.benchmarkName && (
            <p className="text-xs text-muted">
              未披露部分（占净值 <strong className="text-foreground">{((valuation.benchmarkWeight ?? 0) * 100).toFixed(1)}%</strong>）按
              {benchmarkKind === '宽基基准' ? ' 宽基基准 ' : benchmarkKind === '债券基准' ? ' 债券基准 ' : ' 标的/行业指数 '}
              <strong className="text-foreground"> {valuation.benchmarkName} </strong>
              当日涨跌 <GainLossBadge value={valuation.benchmarkReturn ?? 0} format="pct" /> 近似。
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
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-muted border-b border-border">
                <th className="py-2 pr-3 font-medium">个股</th>
                <th className="py-2 pr-3 font-medium text-right">占净值</th>
                <th className="py-2 pr-3 font-medium text-right">现价</th>
                <th className="py-2 pr-3 font-medium text-right">昨收</th>
                <th className="py-2 pr-3 font-medium text-right">当日涨跌</th>
                <th className="py-2 pr-3 font-medium text-right">对净值贡献</th>
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
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        <p className="mt-3 text-xs text-muted">
          估值公式：估算净值 = 官方净值 × (1 + Σ 占比ᵢ × (现价ᵢ / 昨收ᵢ − 1))。未披露部分（现金/债券/非前十大）按基准指数当日涨跌近似。
        </p>
      </Card>

      {/* ===== 交易记录 ===== */}
      <Card title="交易记录">
        {data.transactions.length === 0 ? (
          <EmptyState title="暂无交易记录" hint="导入交易截图或手动记账后，该基金的所有买卖/分红将在此展示" />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
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
                    <td className="py-2.5 pr-3 text-right tnum">{t.price != null ? t.price.toFixed(4) : '—'}</td>
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

        {navPoints.length === 0 ? (
          <EmptyState
            title={navRefreshing ? '正在拉取历史净值…' : '暂无净值数据'}
            hint={navRefreshing ? '' : '点击右上角「刷新」自动拉取东财历史净值'}
          />
        ) : (
          <>
            <ResponsiveContainer width="100%" height={280}>
              <ComposedChart data={navPoints} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
                <CartesianGrid stroke={chartColors.border} strokeDasharray="3 3" />
                <XAxis
                  dataKey="date"
                  tick={{ fontSize: 11, fill: chartColors.muted }}
                  tickFormatter={fmtDateTick}
                  minTickGap={28}
                />
                <YAxis
                  tick={{ fontSize: 11, fill: chartColors.muted }}
                  domain={['auto', 'auto']}
                  width={52}
                  tickFormatter={(v: number) => v.toFixed(3)}
                />
                <Tooltip
                  formatter={tooltipFormatter}
                  labelFormatter={(l) => `日期 ${l}`}
                  contentStyle={{ fontSize: 12, borderRadius: 8 }}
                />
                <Legend wrapperStyle={{ fontSize: 12 }} />
                <Line type="monotone" dataKey="nav" name="单位净值" stroke={chartColors.primary} dot={false} strokeWidth={1.6} />
                <Line type="monotone" dataKey="accNav" name="累计净值" stroke={chartColors.muted} dot={false} strokeWidth={1.2} strokeDasharray="4 3" />
                {buyData.length > 0 && (
                  <Scatter data={buyData} dataKey="nav" name="买入" shape={<UpTriangle fill={chartColors.gain} />} legendType="none" isAnimationActive={false} />
                )}
                {sellData.length > 0 && (
                  <Scatter data={sellData} dataKey="nav" name="卖出" shape={<DownTriangle fill={chartColors.loss} />} legendType="none" isAnimationActive={false} />
                )}
                {divData.length > 0 && (
                  <Scatter data={divData} dataKey="nav" name="分红" shape={<Diamond fill={chartColors.warning} />} legendType="none" isAnimationActive={false} />
                )}
              </ComposedChart>
            </ResponsiveContainer>
            <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.gain }} /> 买入</span>
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.loss }} /> 卖出</span>
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rotate-45" style={{ background: chartColors.warning }} /> 分红</span>
              <span className="text-muted/80">买卖/分红点落在对应日期的净值线上</span>
            </div>
          </>
        )}
      </Card>

      {/* ===== 持仓成本走势图 ===== */}
      <Card
        title={
          <span className="inline-flex items-center gap-1.5">
            <TrendingUp size={15} aria-hidden /> 持仓成本走势
          </span>
        }
      >
        {costPoints.length === 0 ? (
          <EmptyState
            title="暂无交易记录"
            hint="导入买/卖/分红流水后，将展示累计成本与单位成本走势"
          />
        ) : (
          <>
            <ResponsiveContainer width="100%" height={280}>
              <LineChart data={costMerged} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
                <CartesianGrid stroke={chartColors.border} strokeDasharray="3 3" />
                <XAxis
                  dataKey="date"
                  tick={{ fontSize: 11, fill: chartColors.muted }}
                  tickFormatter={fmtDateTick}
                  minTickGap={28}
                />
                <YAxis
                  yAxisId="left"
                  tick={{ fontSize: 11, fill: chartColors.muted }}
                  width={56}
                  tickFormatter={fmtMoney}
                  label={{ value: '累计成本', angle: -90, position: 'insideLeft', fontSize: 11, fill: chartColors.muted }}
                />
                <YAxis
                  yAxisId="right"
                  orientation="right"
                  tick={{ fontSize: 11, fill: chartColors.muted }}
                  width={52}
                  tickFormatter={(v: number) => v.toFixed(3)}
                  label={{ value: '净值/单位成本', angle: 90, position: 'insideRight', fontSize: 11, fill: chartColors.muted }}
                />
                <Tooltip
                  formatter={tooltipFormatter}
                  labelFormatter={(l) => `日期 ${l}`}
                  contentStyle={{ fontSize: 12, borderRadius: 8 }}
                />
                <Legend wrapperStyle={{ fontSize: 12 }} />
                <Line yAxisId="left" type="stepAfter" dataKey="cumulativeCost" name="累计成本" stroke={chartColors.primary} dot={false} strokeWidth={1.8} />
                <Line yAxisId="right" type="monotone" dataKey="unitCost" name="单位成本" stroke={chartColors.gain} dot={false} strokeWidth={1.6} strokeDasharray="5 3" />
                <Line yAxisId="right" type="monotone" dataKey="nav" name="净值" stroke={chartColors.muted} dot={false} strokeWidth={1.2} strokeDasharray="2 2" />
                {buyData.length > 0 && (
                  <Scatter yAxisId="right" data={buyData} dataKey="nav" name="买入" shape={<UpTriangle fill={chartColors.gain} />} legendType="none" isAnimationActive={false} />
                )}
                {sellData.length > 0 && (
                  <Scatter yAxisId="right" data={sellData} dataKey="nav" name="卖出" shape={<DownTriangle fill={chartColors.loss} />} legendType="none" isAnimationActive={false} />
                )}
                {divData.length > 0 && (
                  <Scatter yAxisId="right" data={divData} dataKey="nav" name="分红" shape={<Diamond fill={chartColors.warning} />} legendType="none" isAnimationActive={false} />
                )}
              </LineChart>
            </ResponsiveContainer>
            <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted">
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.primary }} /> 累计成本</span>
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.gain }} /> 单位成本</span>
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.muted }} /> 净值</span>
              <span className="inline-flex items-center gap-1"><span className="inline-block h-2.5 w-2.5 rounded-sm" style={{ background: chartColors.loss }} /> 卖出</span>
              <span>单位成本低于净值即浮盈</span>
            </div>
          </>
        )}
      </Card>
    </div>
  );
}
