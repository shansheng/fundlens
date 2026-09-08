// 基金穿透页（Look-through）— 组合层面的虚拟底层资产表
// 只读分析层（与策略信号同一原则）：穿透结果绝不改持仓、不产生流水（v9 模型红线）。
// 三问：我实际持有什么 / 暴露是否集中重复 / 隐性重仓是谁。
// 口径红线（规划 v1.1 §2）：不放大原则（披露权重直用）、未穿透桶显式单列、覆盖率常驻、
// 当日贡献仅交易时段展示（基于披露权重的近似值，未穿透部分不计入）。
// 颜色全走 CSS 令牌（红涨绿跌仅用于当日贡献/涨跌列，行业条形单一中性信息色 ramp）。
import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw, CircleAlert, Download, Layers, TriangleAlert, ChevronDown, ChevronRight, Grid3x3, X, RefreshCcw } from 'lucide-react';
import {
  lookthroughOverview,
  lookthroughOverlap,
  lookthroughOverlapDetail,
  lookthroughStyle,
  refreshStockStyle,
  refreshIndexConstituents,
  fetchStockProfiles,
  fetchAllDisclosures,
  type LookthroughResult,
  type IndustrySlice,
  type OverlapResult,
  type OverlapDetailResult,
  type StyleBoxResult,
} from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, EmptyState } from '../components/ui';
import { useNarrow } from '../hooks/useNarrow';

const fmtMv = (v: number) => `¥${v.toLocaleString('zh-CN', { maximumFractionDigits: 0 })}`;
const fmtPct = (v: number) => `${(v * 100).toFixed(1)}%`;

/** 行业穿透横向条形图（纯 CSS，单一中性信息色 ramp；虚拟桶虚线区隔；P1 行可点击钻取成分股） */
function IndustryBars({
  slices,
  showDay,
  selected,
  onSelect,
}: {
  slices: IndustrySlice[];
  showDay: boolean;
  selected: string | null;
  onSelect: (key: string | null) => void;
}) {
  const max = Math.max(...slices.map((s) => s.pct), 1e-9);
  return (
    <div className="space-y-1.5" role="table" aria-label="行业穿透分布">
      {slices.map((s) => (
        <div
          key={s.key}
          className={`flex items-center gap-2 text-sm rounded-sm transition-colors ${selected === s.key ? 'bg-surface ring-1 ring-primary/40' : 'hover:bg-surface/60'}`}
          role="row"
        >
          <button
            className="w-28 shrink-0 truncate text-right text-foreground hover:text-primary touch-target"
            title={s.key}
            aria-pressed={selected === s.key}
            onClick={() => onSelect(selected === s.key ? null : s.key)}
          >
            {selected === s.key && <ChevronDown size={12} className="inline mr-0.5 text-primary" aria-hidden />}
            {s.key}
          </button>
          <div className="h-5 min-w-0 flex-1" role="cell">
            <div
              className={`h-full rounded-sm ${s.isVirtual ? 'border border-dashed border-border bg-transparent' : ''}`}
              style={
                s.isVirtual
                  ? undefined
                  : {
                      width: `${Math.max((s.pct / max) * 100, 1.5)}%`,
                      background: 'color-mix(in srgb, var(--color-primary) 55%, transparent)',
                    }
              }
              aria-hidden
            />
          </div>
          <div className="tnum w-14 shrink-0 text-right font-medium" role="cell">
            {fmtPct(s.pct)}
          </div>
          <div className="tnum w-16 shrink-0 text-right text-xs text-muted" role="cell">
            {fmtMv(s.marketValue)}
          </div>
          <div className="w-20 shrink-0 text-right" role="cell">
            {showDay && s.dayContribution !== null ? (
              <GainLossBadge value={s.dayContribution} format="amount" subtle />
            ) : (
              <span className="text-xs text-muted">—</span>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}

export default function LookthroughPage() {
  const { platform } = usePlatform();
  const [data, setData] = useState<LookthroughResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<'industry' | 'stock' | 'overlap' | 'style'>('industry');
  // 两级行业切换（已裁定）：大类 L1 / 细分 L2（东财行业名直出），两级分母一致
  const [level, setLevel] = useState<'l1' | 'l2'>('l1');
  const [expanded, setExpanded] = useState<string | null>(null);
  // P1 行业钻取：选中的行业 key（L1 大类名或 L2 细分名），点击行业条展开成分股
  const [drillKey, setDrillKey] = useState<string | null>(null);
  // P1 基金重合矩阵（懒加载：首次切到 Tab 时拉取）
  const [overlap, setOverlap] = useState<OverlapResult | null>(null);
  const [overlapLoading, setOverlapLoading] = useState(false);
  // P2 重合矩阵钻取：点击矩阵 cell / 窄屏榜单行
  const [drill, setDrill] = useState<{ codeA: string; codeB: string; nameA: string; nameB: string } | null>(null);
  const [drillData, setDrillData] = useState<OverlapDetailResult | null>(null);
  const [drillLoading, setDrillLoading] = useState(false);
  const [drillError, setDrillError] = useState<string | null>(null);
  // P2 风格箱（懒加载：首次切到 Tab 时拉取）
  const [styleData, setStyleData] = useState<StyleBoxResult | null>(null);
  const [styleLoading, setStyleLoading] = useState(false);
  const [styleError, setStyleError] = useState<string | null>(null);
  const [styleRefreshing, setStyleRefreshing] = useState(false);
  const [fetchingDisclosure, setFetchingDisclosure] = useState(false);
  const [fetchingProfiles, setFetchingProfiles] = useState(false);
  const [fetchingIndex, setFetchingIndex] = useState(false);
  const fetchingRef = useRef(false);
  const narrow = useNarrow();

  const load = useCallback(async () => {
    if (fetchingRef.current) return;
    fetchingRef.current = true;
    setLoading(true);
    setError(null);
    try {
      const r = await lookthroughOverview(platform);
      setData(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      console.error('[FundLens] lookthroughOverview failed:', e);
    } finally {
      setLoading(false);
      fetchingRef.current = false;
    }
  }, [platform]);

  const loadOverlap = useCallback(async () => {
    setOverlapLoading(true);
    try {
      setOverlap(await lookthroughOverlap(platform));
    } catch (e) {
      console.error('[FundLens] lookthroughOverlap failed:', e);
      setOverlap(null);
    } finally {
      setOverlapLoading(false);
    }
  }, [platform]);

  useEffect(() => {
    void load();
  }, [load]);

  const loadStyle = useCallback(async () => {
    setStyleLoading(true);
    setStyleError(null);
    try {
      setStyleData(await lookthroughStyle(platform));
    } catch (e) {
      console.error('[FundLens] lookthroughStyle failed:', e);
      setStyleError(e instanceof Error ? e.message : String(e));
      setStyleData(null);
    } finally {
      setStyleLoading(false);
    }
  }, [platform]);

  const handleStyleRefresh = useCallback(async () => {
    setStyleRefreshing(true);
    try {
      const r = await refreshStockStyle();
      await loadStyle();
      if (r.needed === 0) {
        alert(`全部 ${r.total} 只股票的风格快照已是最新，无需补拉。`);
      } else if (r.failed === 0) {
        alert(`已补拉 ${r.fetched} 只 A 股风格快照（${r.at}）。`);
      } else {
        alert(`补拉完成：${r.fetched} 成功 / ${r.failed} 失败。\n失败代码：${r.failedCodes.join(', ')}`);
      }
    } catch (e) {
      alert(`补拉失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setStyleRefreshing(false);
    }
  }, [loadStyle]);

  // P2：切换到风格箱 Tab 时懒加载（首次切到再拉 lookthroughStyle）
  useEffect(() => {
    if (tab === 'style' && !styleData && !styleLoading) void loadStyle();
  }, [tab, styleData, styleLoading, loadStyle]);

  // P2 重合钻取：点击矩阵 cell / 窄屏榜单行 → 打开共同持仓明细
  const openDrill = useCallback((codeA: string, nameA: string, codeB: string, nameB: string) => {
    setDrill({ codeA, nameA, codeB, nameB });
    setDrillLoading(true);
    setDrillError(null);
    setDrillData(null);
    lookthroughOverlapDetail(codeA, codeB)
      .then((r) => setDrillData(r))
      .catch((e) => setDrillError(e instanceof Error ? e.message : String(e)))
      .finally(() => setDrillLoading(false));
  }, []);

  // P1：切换到重合 Tab 时懒加载（纯 DB 聚合，毫秒级但避免无谓查询）
  useEffect(() => {
    if (tab === 'overlap' && !overlap && !overlapLoading) void loadOverlap();
  }, [tab, overlap, overlapLoading, loadOverlap]);

  // 交易时段每 15 分钟自动刷新（沿用总览节奏；非交易时段后端不发任何行情请求）
  useEffect(() => {
    if (!data?.hasQuotes) return;
    const t = setInterval(() => void load(), 15 * 60 * 1000);
    return () => clearInterval(t);
  }, [data?.hasQuotes, load]);

  const handleFetchAllDisclosures = useCallback(async () => {
    if (!confirm('一键抓取所有基金的披露持仓（前十大重仓）？\n将逐只从公开数据源拉取最新季报持仓，耗时随基金数量增加。')) return;
    setFetchingDisclosure(true);
    try {
      const r = await fetchAllDisclosures();
      await load();
      if (r.failed === 0) {
        alert(`已抓取 ${r.ok}/${r.total} 只基金的披露持仓（${r.at}）。`);
      } else {
        alert(`抓取完成：${r.ok} 成功 / ${r.failed} 失败（共 ${r.total} 只）。\n失败基金代码：${r.failedCodes.join(', ')}`);
      }
    } catch (e) {
      alert(`抓取失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setFetchingDisclosure(false);
    }
  }, [load]);

  const handleFetchProfiles = useCallback(async () => {
    setFetchingProfiles(true);
    try {
      const r = await fetchStockProfiles();
      await load();
      if (r.needed === 0) {
        alert(`全部 ${r.total} 只股票的行业画像已是最新，无需补拉。`);
      } else if (r.failed === 0) {
        alert(`已补拉 ${r.fetched} 只股票的行业画像（${r.at}）。`);
      } else {
        alert(`补拉完成：${r.fetched} 成功 / ${r.failed} 失败。\n失败代码：${r.failedCodes.join(', ')}`);
      }
    } catch (e) {
      alert(`补拉失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setFetchingProfiles(false);
    }
  }, [load]);

  const handleRefreshIndex = useCallback(async () => {
    if (!confirm('按跟踪指数成分补拉穿透数据？\n纯被动指数基金将按跟踪指数最新成分名单 × 流通市值近似权重（×0.95）穿透，非官方披露重仓，耗时随指数数量增加。')) return;
    setFetchingIndex(true);
    try {
      const r = await refreshIndexConstituents();
      await load();
      if (r.failedCodes.length === 0) {
        alert(`已刷新 ${r.refreshedCodes.length} 只指数的成分（共 ${r.totalTargetCodes} 只目标，at ${r.at}）。`);
      } else {
        alert(`刷新完成：${r.refreshedCodes.length} 成功 / ${r.failedCodes.length} 失败（共 ${r.totalTargetCodes} 只目标）。\n失败指数代码：${r.failedCodes.join(', ')}`);
      }
    } catch (e) {
      alert(`刷新失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setFetchingIndex(false);
    }
  }, [load]);

  if (loading && !data) return <div className="p-6"><EmptyState title="加载中…" /></div>;
  if (error) return (
    <div className="p-6 space-y-3">
      <EmptyState title="加载失败" hint={error} />
      <button onClick={() => void load()} className="rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover">重试</button>
    </div>
  );
  if (!data) return <div className="p-6"><EmptyState title="暂无数据" hint="请先在「截图导入」中添加持仓" /></div>;

  const hasDisclosureData = data.stocks.length > 0 || data.funds.length > 0;
  const showDay = data.hasQuotes;
  const slices = level === 'l1' ? data.industriesL1 : data.industriesL2;

  return (
    <div className="p-3 space-y-2.5">
      <header className="flex items-center justify-between gap-3 flex-wrap">
        <div>
          <h1 className="flex items-center gap-2 text-xl font-semibold">
            <Layers size={20} className="text-primary" aria-hidden />
            基金穿透
          </h1>
          <p className="text-xs text-muted mt-0.5">
            组合虚拟底层资产表 · 只读分析 · 更新于 {data.asOf}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void handleFetchAllDisclosures()}
            disabled={fetchingDisclosure}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
          >
            <Download size={16} className={fetchingDisclosure ? 'animate-pulse' : ''} aria-hidden />
            {fetchingDisclosure ? '抓取中…' : '抓取披露持仓'}
          </button>
          <button
            onClick={() => void handleFetchProfiles()}
            disabled={fetchingProfiles}
            title="补拉缺失/过期的 A 股行业画像（东财公开数据源，节流出站）"
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
          >
            <Layers size={16} className={fetchingProfiles ? 'animate-pulse' : ''} aria-hidden />
            {fetchingProfiles ? '补画像中…' : '补行业画像'}
          </button>
          <button
            onClick={() => void handleRefreshIndex()}
            disabled={fetchingIndex}
            title="按跟踪指数成分补拉穿透数据（流通市值近似权重，非官方披露）"
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
          >
            <RefreshCcw size={16} className={fetchingIndex ? 'animate-spin' : ''} aria-hidden />
            {fetchingIndex ? '刷新指数中…' : '刷新指数成分'}
          </button>
          <button
            onClick={() => void load()}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-sm text-on-primary hover:bg-primary-hover touch-target"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} aria-hidden />
            刷新
          </button>
        </div>
      </header>

      {/* 口径条（常驻，warning 语气）：覆盖率 · 报告期分布 · 时滞提示 */}
      <div className="rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-xs text-warning space-y-1">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="inline-flex items-center gap-1 font-medium">
            <CircleAlert size={14} aria-hidden />
            组合覆盖率 {fmtPct(data.coverage)}
          </span>
          <span>报告期：{data.reportPeriods.length > 0 ? data.reportPeriods.join(' / ') : '无披露'}</span>
          <span>总市值 {fmtMv(data.totalMv)}（份额 × 最新官方净值口径）</span>
        </div>
        <p className="leading-relaxed opacity-90">
          指数型基金按跟踪指数最新成分名单 × 流通市值近似权重（×0.95）穿透，其余基金按最新披露重仓权重（不放大、未穿透单列），季报约滞后 15 个工作日、中报/年报滞后 2~3 个月；
          {showDay
            ? ' 当日贡献为基于披露/指数权重的近似值（未穿透部分不计入；境外股票按其市场行情时点，与 A 股可能不同步）。'
            : ' 非交易时段不展示当日贡献。'}
        </p>
      </div>

      {!hasDisclosureData ? (
        <EmptyState
          title="尚无披露持仓数据"
          hint="穿透需要基金的季报/中报重仓披露。点击右上「抓取披露持仓」从公开数据源拉取，随后即可看到行业与个股穿透。"
        />
      ) : (
        <>
          {/* Tab 切换 */}
          <div className="flex items-center gap-2" role="tablist" aria-label="穿透视图">
            <button
              role="tab"
              aria-selected={tab === 'industry'}
              onClick={() => setTab('industry')}
              className={`rounded-md px-3 py-1.5 text-sm touch-target ${
                tab === 'industry' ? 'bg-primary text-on-primary' : 'border border-border text-muted hover:bg-surface'
              }`}
            >
              行业穿透
            </button>
            <button
              role="tab"
              aria-selected={tab === 'stock'}
              onClick={() => setTab('stock')}
              className={`rounded-md px-3 py-1.5 text-sm touch-target ${
                tab === 'stock' ? 'bg-primary text-on-primary' : 'border border-border text-muted hover:bg-surface'
              }`}
            >
              个股穿透
            </button>
            <button
              role="tab"
              aria-selected={tab === 'overlap'}
              onClick={() => setTab('overlap')}
              className={`inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-sm touch-target ${
                tab === 'overlap' ? 'bg-primary text-on-primary' : 'border border-border text-muted hover:bg-surface'
              }`}
            >
              <Grid3x3 size={14} aria-hidden />
              基金重合
            </button>
            <button
              role="tab"
              aria-selected={tab === 'style'}
              onClick={() => setTab('style')}
              className={`inline-flex items-center gap-1 rounded-md px-3 py-1.5 text-sm touch-target ${
                tab === 'style' ? 'bg-primary text-on-primary' : 'border border-border text-muted hover:bg-surface'
              }`}
            >
              <Grid3x3 size={14} aria-hidden />
              风格箱
            </button>
          </div>

          {tab === 'industry' && (
            <Card
              title="行业穿透"
              action={
                <div className="flex items-center gap-0.5 rounded-md border border-border bg-background p-0.5" role="group" aria-label="行业层级">
                  <button
                    onClick={() => { setLevel('l1'); setDrillKey(null); }}
                    aria-pressed={level === 'l1'}
                    className={`rounded px-2 py-1 text-xs transition-colors touch-target ${
                      level === 'l1' ? 'bg-primary text-on-primary' : 'text-muted hover:bg-surface'
                    }`}
                  >
                    大类
                  </button>
                  <button
                    onClick={() => { setLevel('l2'); setDrillKey(null); }}
                    aria-pressed={level === 'l2'}
                    className={`rounded px-2 py-1 text-xs transition-colors touch-target ${
                      level === 'l2' ? 'bg-primary text-on-primary' : 'text-muted hover:bg-surface'
                    }`}
                  >
                    细分
                  </button>
                </div>
              }
            >
              <div className="mb-1.5 flex items-center justify-end gap-2 text-[11px] text-muted">
                <span>占比</span>
                <span>穿透市值</span>
                <span className="w-20 text-right">{showDay ? '当日贡献' : '当日（休市）'}</span>
              </div>
              <IndustryBars slices={slices} showDay={showDay} selected={drillKey} onSelect={setDrillKey} />
              {/* P1 行业钻取：点击行业条展开该行业成分股（前端过滤 stocks，与行业条同口径） */}
              {drillKey && (
                <div className="mt-2 rounded-md border border-primary/30 bg-surface/60 p-2.5">
                  <div className="mb-1.5 flex items-center justify-between">
                    <div className="text-xs font-medium">
                      {drillKey} · 成分股（穿透口径）
                    </div>
                    <button onClick={() => setDrillKey(null)} className="text-xs text-muted hover:text-foreground touch-target">
                      收起
                    </button>
                  </div>
                  <div className="max-h-56 space-y-0.5 overflow-y-auto">
                    {(() => {
                      const members = data.stocks.filter((s) =>
                        level === 'l1' ? s.sectorL1 === drillKey : s.industryL2 === drillKey,
                      );
                      if (members.length === 0) {
                        return <div className="py-2 text-center text-xs text-muted">该桶为现金 / 债券 / 未披露部分，无成分股（不放大原则）。</div>;
                      }
                      return members.map((s) => (
                        <div key={s.stockCode} className="flex items-center justify-between gap-3 text-xs">
                          <span className="min-w-0 truncate">
                            <span className="font-medium">{s.stockName}</span>
                            <span className="tnum text-muted"> {s.stockCode}</span>
                            {s.hiddenWarning && <TriangleAlert size={11} className="ml-1 inline text-warning" aria-label="隐性重仓" />}
                          </span>
                          <span className="tnum shrink-0 text-muted">
                            {fmtPct(s.pct)} · {fmtMv(s.marketValue)} · {s.fundCount} 基金
                          </span>
                        </div>
                      ));
                    })()}
                  </div>
                </div>
              )}
              <div className="mt-2 border-t border-border/60 pt-2 text-xs text-muted">
                点击行业名可钻取成分股；「未穿透」= 现金 / 债券 / 未披露部分（虚线桶，不放大归一）；
                「境外资产」= 港股 / 美股（补行业画像后按行业细分）；
                「未分类」= 行业画像待补（可点「补行业画像」重试）。
              </div>
            </Card>
          )}

          {tab === 'stock' && (
            <Card
              title="个股穿透 · 虚拟重仓表"
              action={
                <div className="flex items-center gap-2 text-xs">
                  <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 tnum">CR5 {(data.cr5 * 100).toFixed(1)}%</span>
                  <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 tnum">CR10 {(data.cr10 * 100).toFixed(1)}%</span>
                </div>
              }
            >
              <div className="mb-2 flex flex-wrap items-center gap-2 text-xs text-muted">
                <span className="inline-flex items-center gap-1">
                  <TriangleAlert size={13} aria-hidden />
                  隐性重仓预警：同一股票经 ≥3 只基金持有且合计穿透占比 &gt;5%
                </span>
              </div>
              {narrow ? (
                /* 窄屏：卡片式 */
                <div className="space-y-2">
                  {data.stocks.slice(0, 30).map((s) => (
                    <div key={s.stockCode} className="rounded-md border border-border bg-background p-2.5">
                      <div className="flex items-center justify-between gap-2">
                        <div className="min-w-0">
                          <div className="flex items-center gap-1.5">
                            <span className="truncate font-medium">{s.stockName}</span>
                            <span className="tnum text-xs text-muted">{s.stockCode}</span>
                            {s.hiddenWarning && (
                              <span className="inline-flex items-center gap-0.5 rounded-pill border border-warning/40 bg-warning/10 px-1.5 py-0.5 text-[11px] text-warning">
                                <TriangleAlert size={11} aria-hidden />
                                隐性重仓
                              </span>
                            )}
                          </div>
                          <div className="mt-0.5 text-xs text-muted">
                            {s.sectorL1} · {s.industryL2} · {s.fundCount} 只基金
                          </div>
                        </div>
                        <div className="text-right">
                          <div className="tnum font-medium">{fmtPct(s.pct)}</div>
                          <div className="tnum text-xs text-muted">{fmtMv(s.marketValue)}</div>
                        </div>
                      </div>
                      {showDay && s.dayChangePct !== null && (
                        <div className="mt-1.5 flex items-center justify-between border-t border-border/60 pt-1.5">
                          <span className="text-xs text-muted">当日涨跌</span>
                          <div className="flex items-center gap-2">
                            <GainLossBadge value={s.dayChangePct} format="pct" subtle />
                            {s.dayContribution !== null && <GainLossBadge value={s.dayContribution} format="amount" subtle />}
                          </div>
                        </div>
                      )}
                    </div>
                  ))}
                </div>
              ) : (
                /* 桌面：宽表（行可展开贡献基金明细） */
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b border-border text-left text-xs text-muted">
                        <th className="py-1.5 pr-2 font-medium">股票</th>
                        <th className="py-1.5 pr-2 font-medium">大类 / 细分</th>
                        <th className="py-1.5 pr-2 text-right font-medium">穿透权重</th>
                        <th className="py-1.5 pr-2 text-right font-medium">穿透市值</th>
                        <th className="py-1.5 pr-2 text-right font-medium">基金数</th>
                        {showDay && <th className="py-1.5 pr-2 text-right font-medium">当日涨跌</th>}
                        {showDay && <th className="py-1.5 text-right font-medium">当日贡献</th>}
                      </tr>
                    </thead>
                    <tbody>
                      {data.stocks.slice(0, 50).map((s) => (
                        <Fragment key={s.stockCode}>
                          <tr
                            className="cursor-pointer border-b border-border/60 hover:bg-surface"
                            onClick={() => setExpanded(expanded === s.stockCode ? null : s.stockCode)}
                          >
                            <td className="py-1.5 pr-2">
                              <span className="inline-flex items-center gap-1">
                                {expanded === s.stockCode ? <ChevronDown size={14} aria-hidden /> : <ChevronRight size={14} aria-hidden />}
                                <span className="font-medium">{s.stockName}</span>
                                <span className="tnum text-xs text-muted">{s.stockCode}</span>
                                {s.hiddenWarning && (
                                  <span
                                    className="inline-flex items-center gap-0.5 rounded-pill border border-warning/40 bg-warning/10 px-1.5 py-0.5 text-[11px] text-warning"
                                    title={`经 ${s.fundCount} 只基金合计穿透占比 ${fmtPct(s.pct)}`}
                                  >
                                    <TriangleAlert size={11} aria-hidden />
                                    隐性重仓
                                  </span>
                                )}
                              </span>
                            </td>
                            <td className="py-1.5 pr-2 text-muted">
                              {s.sectorL1} · {s.industryL2}
                            </td>
                            <td className="tnum py-1.5 pr-2 text-right font-medium">{fmtPct(s.pct)}</td>
                            <td className="tnum py-1.5 pr-2 text-right">{fmtMv(s.marketValue)}</td>
                            <td className="tnum py-1.5 pr-2 text-right">{s.fundCount}</td>
                            {showDay && (
                              <td className="py-1.5 pr-2 text-right">
                                {s.dayChangePct !== null ? <GainLossBadge value={s.dayChangePct} format="pct" subtle /> : <span className="text-muted">—</span>}
                              </td>
                            )}
                            {showDay && (
                              <td className="py-1.5 text-right">
                                {s.dayContribution !== null ? <GainLossBadge value={s.dayContribution} format="amount" subtle /> : <span className="text-muted">—</span>}
                              </td>
                            )}
                          </tr>
                          {expanded === s.stockCode && (
                            <tr className="border-b border-border/60 bg-surface/60">
                              <td colSpan={showDay ? 7 : 5} className="px-6 py-2">
                                <div className="text-xs font-medium text-muted mb-1">贡献基金明细（按贡献市值降序）</div>
                                <div className="space-y-0.5">
                                  {s.funds.map((f) => (
                                    <div key={f.fundCode} className="flex items-center justify-between gap-4 text-xs">
                                      <span className="truncate">
                                        {f.fundName} <span className="tnum text-muted">({f.fundCode})</span>
                                      </span>
                                      <span className="tnum shrink-0 text-muted">
                                        权重 {(f.weight * 100).toFixed(2)}% · 贡献 {fmtMv(f.contributedMv)}
                                      </span>
                                    </div>
                                  ))}
                                </div>
                              </td>
                            </tr>
                          )}
                        </Fragment>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </Card>
          )}

          {tab === 'overlap' && (
            <Card
              title="基金两两重合 · 识别伪分散"
              action={
                overlap ? (
                  <span className={`tnum rounded border px-1.5 py-0.5 text-xs ${overlap.maxWeightOverlap > 0.4 ? 'border-warning/40 bg-warning/10 text-warning' : 'border-border bg-border/40 text-muted'}`}>
                    最高权重重合 {(overlap.maxWeightOverlap * 100).toFixed(0)}%
                  </span>
                ) : undefined
              }
            >
              {overlapLoading && <div className="py-4 text-center text-sm text-muted">计算中…</div>}
              {!overlapLoading && overlap && overlap.funds.length < 2 && (
                <div className="py-4 text-center text-sm text-muted">
                  需要至少 2 只有披露持仓的基金才能计算两两重合。
                  可点右上「抓取披露持仓」补数据。
                </div>
              )}
              {!overlapLoading && overlap && overlap.funds.length >= 2 && (
                <>
                  <div className="mb-2 rounded-md border border-border bg-background px-2.5 py-1.5 text-xs text-muted">
                    <span className="font-medium text-foreground">权重重合度</span> = Σ min(wᵢ, wⱼ)（共同持仓逐股取小权重求和，
                    100% = 完全复制）；<span className="font-medium text-foreground">持股重合</span> = 共同持股数 ÷ 两基金持股并集（Jaccard，集合口径）。
                    经验参考：权重重合 &gt;40% 需警惕伪分散。
                  </div>
                  {/* 高重合对榜单（窄屏主视图）：按权重重合降序 */}
                  <div className="space-y-1">
                    {[...overlap.cells]
                      .sort((a, b) => b.weightOverlap - a.weightOverlap)
                      .slice(0, 10)
                      .map((c) => {
                        const a = overlap.funds[c.i];
                        const b = overlap.funds[c.j];
                        const high = c.weightOverlap > 0.4;
                        return (
                          <div
                            key={`${c.i}-${c.j}`}
                            role="button"
                            tabIndex={0}
                            onClick={() => openDrill(a.code, a.name, b.code, b.name)}
                            onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') openDrill(a.code, a.name, b.code, b.name); }}
                            className={`flex cursor-pointer items-center justify-between gap-3 rounded-md border px-2.5 py-1.5 text-sm ${high ? 'border-warning/40 bg-warning/5' : 'border-border bg-background'}`}
                          >
                            <div className="min-w-0 truncate">
                              <span className="truncate">{a.name}</span>
                              <span className="text-muted"> × </span>
                              <span className="truncate">{b.name}</span>
                              {high && <TriangleAlert size={12} className="ml-1 inline text-warning" aria-label="伪分散预警" />}
                            </div>
                            <div className="tnum flex shrink-0 items-center gap-2 text-xs">
                              <span className={high ? 'font-medium text-warning' : 'text-foreground'}>
                                {(c.weightOverlap * 100).toFixed(1)}%
                              </span>
                              <span className="text-muted">
                                股 {c.commonCount} 只 · J {(c.jaccard * 100).toFixed(0)}%
                              </span>
                            </div>
                          </div>
                        );
                      })}
                  </div>
                  {/* 宽屏：对称矩阵表 */}
                  {!narrow && overlap.funds.length <= 12 && (
                    <div className="mt-3 overflow-x-auto">
                      <table className="w-full text-xs">
                        <thead>
                          <tr className="border-b border-border text-left text-muted">
                            <th className="py-1 pr-2 font-medium">矩阵（权重重合%）</th>
                            {overlap.funds.map((f, idx) => (
                              <th key={f.code} className="py-1 px-1 text-center font-medium" title={`${f.name} (${f.code})`}>
                                {idx + 1}
                              </th>
                            ))}
                          </tr>
                        </thead>
                        <tbody>
                          {overlap.funds.map((fa, i) => (
                            <tr key={fa.code} className="border-b border-border/60">
                              <td className="max-w-36 truncate py-1 pr-2" title={`${fa.name} (${fa.code})`}>
                                <span className="tnum text-muted">{i + 1}</span> {fa.name}
                              </td>
                              {overlap.funds.map((_, j) => {
                                if (i === j) {
                                  return (
                                    <td key={j} className="px-1 py-1 text-center text-muted" title="自身">
                                      ·
                                    </td>
                                  );
                                }
                                const [x, y] = i < j ? [i, j] : [j, i];
                                const cell = overlap.cells.find((c) => c.i === x && c.j === y);
                                const w = cell?.weightOverlap ?? 0;
                                const clickable = !!cell;
                                return (
                                  <td
                                    key={j}
                                    className={`tnum px-1 py-1 text-center ${clickable ? 'cursor-pointer hover:ring-1 hover:ring-primary/50' : ''}`}
                                    role={clickable ? 'button' : undefined}
                                    tabIndex={clickable ? 0 : undefined}
                                    title={cell ? `${overlap.funds[x].name} × ${overlap.funds[y].name}\n权重重合 ${(w * 100).toFixed(1)}% · 共同持股 ${cell.commonCount} 只 · Jaccard ${(cell.jaccard * 100).toFixed(0)}%\n点击查看共同持仓` : '无重合'}
                                    onClick={clickable ? () => openDrill(overlap.funds[x].code, overlap.funds[x].name, overlap.funds[y].code, overlap.funds[y].name) : undefined}
                                    onKeyDown={clickable ? (e) => { if (e.key === 'Enter' || e.key === ' ') openDrill(overlap.funds[x].code, overlap.funds[x].name, overlap.funds[y].code, overlap.funds[y].name); } : undefined}
                                    style={{ background: w > 0 ? `color-mix(in srgb, var(--color-primary) ${Math.min(w * 100, 70)}%, transparent)` : undefined }}
                                  >
                                    {w > 0 ? (w * 100).toFixed(0) : '—'}
                                  </td>
                                );
                              })}
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  )}
                </>
              )}
            </Card>
          )}

          {/* P2：重合矩阵钻取 —— 共同持仓明细 */}
          {drill && (
            <Card
              title={
                <span>
                  共同持仓明细
                  <span className="ml-1.5 text-muted">
                    {drill.nameA} × {drill.nameB}
                  </span>
                </span>
              }
              action={
                <button
                  onClick={() => { setDrill(null); setDrillData(null); setDrillError(null); }}
                  className="inline-flex items-center gap-1 rounded-md border border-border bg-background px-2 py-1 text-xs text-muted hover:bg-border/60 touch-target"
                >
                  <X size={14} aria-hidden />
                  关闭
                </button>
              }
            >
              <div className="mb-2 flex flex-wrap items-center gap-2 text-xs">
                <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 tnum">
                  权重重合 {(drillData?.weightOverlap ?? 0) * 100 >= 0 ? (drillData?.weightOverlap ?? 0) * 100 : 0}%
                </span>
                {drillData && (
                  <>
                    <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 tnum">
                      共同持股 {drillData.commonCount} 只
                    </span>
                    <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 tnum">
                      Jaccard {drillData.jaccard * 100 >= 0 ? (drillData.jaccard * 100).toFixed(0) : 0}%
                    </span>
                  </>
                )}
              </div>
              {drillLoading && <div className="py-4 text-center text-sm text-muted">计算中…</div>}
              {!drillLoading && drillError && (
                <div className="space-y-2 py-3 text-center">
                  <div className="text-sm text-danger">{drillError}</div>
                  <button
                    onClick={() => openDrill(drill.codeA, drill.nameA, drill.codeB, drill.nameB)}
                    className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover touch-target"
                  >
                    <RefreshCcw size={14} aria-hidden />
                    重试
                  </button>
                </div>
              )}
              {!drillLoading && !drillError && drillData && (
                drillData.common.length === 0 ? (
                  <div className="py-4 text-center text-sm text-muted">两基金无共同持仓。</div>
                ) : (
                  <div className="max-h-72 overflow-y-auto">
                    <table className="w-full text-sm">
                      <thead>
                        <tr className="border-b border-border text-left text-xs text-muted">
                          <th className="py-1.5 pr-2 font-medium">股票</th>
                          <th className="py-1.5 pr-2 text-right font-medium">代码</th>
                          <th className="py-1.5 pr-2 text-right font-medium">在A权重</th>
                          <th className="py-1.5 pr-2 text-right font-medium">在B权重</th>
                          <th className="py-1.5 text-right font-medium">重合贡献 (min)</th>
                        </tr>
                      </thead>
                      <tbody>
                        {drillData.common.map((h) => {
                          const m = Math.min(h.weightA, h.weightB);
                          return (
                            <tr key={h.stockCode} className="border-b border-border/60">
                              <td className="py-1.5 pr-2 font-medium">{h.stockName}</td>
                              <td className="tnum py-1.5 pr-2 text-right text-muted">{h.stockCode}</td>
                              <td className="tnum py-1.5 pr-2 text-right">{(h.weightA * 100).toFixed(2)}%</td>
                              <td className="tnum py-1.5 pr-2 text-right">{(h.weightB * 100).toFixed(2)}%</td>
                              <td className="tnum py-1.5 text-right text-muted">{m * 100 >= 0 ? (m * 100).toFixed(2) : '0.00'}%</td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>
                )
              )}
            </Card>
          )}

          {/* 基金穿透明细（覆盖率 / 报告期 / 未穿透市值） */}
          {data.funds.length > 0 && (
            <Card title="基金穿透明细">
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b border-border text-left text-xs text-muted">
                      <th className="py-1.5 pr-2 font-medium">基金</th>
                      <th className="py-1.5 pr-2 text-right font-medium">市值</th>
                      <th className="py-1.5 pr-2 text-right font-medium">覆盖率</th>
                      <th className="py-1.5 pr-2 font-medium">报告期</th>
                      <th className="py-1.5 pr-2 font-medium">穿透口径</th>
                      <th className="py-1.5 text-right font-medium">未穿透市值</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.funds.map((f) => (
                      <tr key={f.code} className="border-b border-border/60">
                        <td className="py-1.5 pr-2">
                          <span className="font-medium">{f.name}</span> <span className="tnum text-xs text-muted">{f.code}</span>
                        </td>
                        <td className="tnum py-1.5 pr-2 text-right">{fmtMv(f.marketValue)}</td>
                        <td className="tnum py-1.5 pr-2 text-right">{fmtPct(f.coverage)}</td>
                        <td className="py-1.5 pr-2 text-muted">{f.reportPeriod ?? '—'}</td>
                        <td className="py-1.5 pr-2">
                          {f.penetrationSource === 'index_constituent' ? (
                            <span
                              className="inline-flex items-center rounded border border-primary/40 bg-primary/10 px-1.5 py-0.5 text-[11px] text-primary"
                              title="按跟踪指数最新成分名单 × 流通市值近似权重(×0.95)，非官方披露"
                            >
                              指数成分
                            </span>
                          ) : (
                            <span className="inline-flex items-center rounded border border-border bg-border/40 px-1.5 py-0.5 text-[11px] text-muted">
                              披露前十大
                            </span>
                          )}
                        </td>
                        <td className="tnum py-1.5 text-right text-muted">{fmtMv(f.unpenetratedMv)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Card>
          )}

          {/* P2：风格箱九宫格（快照估算，东财公开接口，非晨星官方风格箱） */}
          {tab === 'style' && (
            <Card
              title="风格箱 · 持仓规模 × 风格"
              action={
                <button
                  onClick={() => void handleStyleRefresh()}
                  disabled={styleRefreshing}
                  title="补拉缺失 / 过期的 A 股风格快照（东财公开接口，节流出站）"
                  className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
                >
                  <RefreshCcw size={16} className={styleRefreshing ? 'animate-spin' : ''} aria-hidden />
                  {styleRefreshing ? '补快照中…' : '补风格快照'}
                </button>
              }
            >
              {styleLoading && <div className="py-4 text-center text-sm text-muted">计算中…</div>}
              {!styleLoading && styleError && (
                <div className="space-y-2 py-3 text-center">
                  <div className="text-sm text-danger">{styleError}</div>
                  <button
                    onClick={() => void loadStyle()}
                    className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover touch-target"
                  >
                    <RefreshCcw size={14} aria-hidden />
                    重试
                  </button>
                </div>
              )}
              {!styleLoading && !styleError && styleData && styleData.totalMv <= 0 && (
                <div className="py-4 text-center"><EmptyState title="暂无穿透数据" hint="风格箱需要持仓穿透市值，请先抓取披露持仓。" /></div>
              )}
              {!styleLoading && !styleError && styleData && styleData.totalMv > 0 && (() => {
                const coveredStockCount = styleData.cells.reduce((a, c) => a + c.stockCount, 0);
                const overseasPct = styleData.totalMv > 0 ? styleData.overseasMv / styleData.totalMv : 0;
                const noValPct = styleData.totalMv > 0 ? styleData.noValuationMv / styleData.totalMv : 0;
                return (
                  <>
                    <div className="mb-2 space-y-1 rounded-md border border-border bg-background px-2.5 py-1.5 text-xs text-muted">
                      <p>
                        穿透口径不放大；市值与风格为「快照估算」（东财公开接口，非晨星官方风格箱）；
                        分母 = 组合总市值；境外资产 / 无估值市值单列，不计入九宫格。
                      </p>
                      {styleData.snapshotAt ? (
                        <p className="flex flex-wrap items-center gap-x-3 gap-y-0.5">
                          <span>快照：{styleData.snapshotAt}</span>
                          <span>覆盖率 {fmtPct(styleData.coveredPct)}（{coveredStockCount} 只）</span>
                        </p>
                      ) : (
                        <p>尚未取到风格快照，点右上「补风格快照」拉取 A 股风格数据后展示九宫格。</p>
                      )}
                    </div>

                    {narrow ? (
                      /* 窄屏：3×3 网格，格内自带 size + style 小字 */
                      <div className="grid grid-cols-3 gap-1.5">
                        {styleData.cells.map((c) => {
                          const top3 = c.topStocks.slice(0, 3).map((s) => s.stockName).join('、');
                          return (
                            <div
                              key={`${c.size}-${c.style}`}
                              className="rounded-md border border-border bg-surface p-2"
                              title={top3}
                            >
                              <div className="mb-0.5 text-[11px] text-muted">{c.size} · {c.style}</div>
                              <div className={`tnum text-right text-xs font-medium ${c.pct > 0 ? 'text-foreground' : 'text-muted'}`}>
                                {fmtPct(c.pct)}
                              </div>
                              <div className="tnum text-[11px] text-muted">{fmtMv(c.marketValue)}</div>
                              <div className="text-[10px] text-muted">{c.stockCount} 只</div>
                            </div>
                          );
                        })}
                      </div>
                    ) : (
                      /* 桌面：列头（价值/核心/成长）+ 行首（大/中/小）标签 */
                      <div className="space-y-1.5">
                        <div className="flex items-center gap-1.5">
                          <div className="w-7 shrink-0" />
                          {['价值', '核心', '成长'].map((s) => (
                            <div key={s} className="flex-1 text-center text-xs font-medium text-muted">{s}</div>
                          ))}
                        </div>
                        {['大', '中', '小'].map((size) => (
                          <div key={size} className="flex items-center gap-1.5">
                            <div className="w-7 shrink-0 text-center text-xs font-medium text-muted">{size}</div>
                            <div className="grid flex-1 grid-cols-3 gap-1.5">
                              {['价值', '核心', '成长'].map((style) => {
                                const c = styleData.cells.find((x) => x.size === size && x.style === style);
                                const top3 = c ? c.topStocks.slice(0, 3).map((s) => s.stockName).join('、') : '';
                                return (
                                  <div
                                    key={style}
                                    className="rounded-md border border-border bg-surface p-2.5"
                                    title={top3}
                                  >
                                    <div className="flex items-start justify-between">
                                      <span className="text-[11px] text-muted">{style}</span>
                                      <span className={`tnum text-sm font-medium ${c && c.pct > 0 ? 'text-foreground' : 'text-muted'}`}>
                                        {c ? fmtPct(c.pct) : '—'}
                                      </span>
                                    </div>
                                    <div className="tnum mt-0.5 text-xs text-muted">{c ? fmtMv(c.marketValue) : '—'}</div>
                                    <div className="text-[11px] text-muted">{c ? `${c.stockCount} 只` : ''}</div>
                                  </div>
                                );
                              })}
                            </div>
                          </div>
                        ))}
                      </div>
                    )}

                    <div className="mt-2 border-t border-border/60 pt-2 text-xs text-muted">
                      境外资产穿透市值 {fmtMv(styleData.overseasMv)}（占比 {fmtPct(overseasPct)}） · 无市值 / 亏损股估值缺失 {fmtMv(styleData.noValuationMv)}（占比 {fmtPct(noValPct)}）。
                      境外 / 估值缺失不计入九宫格。
                    </div>
                  </>
                );
              })()}
            </Card>
          )}

          <div className="rounded-md border border-border bg-surface px-3 py-2 text-xs text-muted">
            重合口径：基于最新披露期持仓（top10 / 中报年报全量）；报告期不同的基金对，其重合度为跨期近似。
            本页所有输出为持仓结构分析，不构成投资建议。
          </div>
        </>
      )}
    </div>
  );
}
