// 基金穿透页（Look-through）— 组合层面的虚拟底层资产表
// 只读分析层（与策略信号同一原则）：穿透结果绝不改持仓、不产生流水（v9 模型红线）。
// 三问：我实际持有什么 / 暴露是否集中重复 / 隐性重仓是谁。
// 口径红线（规划 v1.1 §2）：不放大原则（披露权重直用）、未穿透桶显式单列、覆盖率常驻、
// 当日贡献仅交易时段展示（基于披露权重的近似值，未穿透部分不计入）。
// 颜色全走 CSS 令牌（红涨绿跌仅用于当日贡献/涨跌列，行业条形单一中性信息色 ramp）。
import { Fragment, useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw, CircleAlert, Download, Layers, TriangleAlert, ChevronDown, ChevronRight } from 'lucide-react';
import {
  lookthroughOverview,
  fetchStockProfiles,
  fetchAllDisclosures,
  type LookthroughResult,
  type IndustrySlice,
} from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, EmptyState } from '../components/ui';
import { useNarrow } from '../hooks/useNarrow';

const fmtMv = (v: number) => `¥${v.toLocaleString('zh-CN', { maximumFractionDigits: 0 })}`;
const fmtPct = (v: number) => `${(v * 100).toFixed(1)}%`;

/** 行业穿透横向条形图（纯 CSS，单一中性信息色 ramp；虚拟桶虚线区隔） */
function IndustryBars({
  slices,
  showDay,
}: {
  slices: IndustrySlice[];
  showDay: boolean;
}) {
  const max = Math.max(...slices.map((s) => s.pct), 1e-9);
  return (
    <div className="space-y-1.5" role="table" aria-label="行业穿透分布">
      {slices.map((s) => (
        <div key={s.key} className="flex items-center gap-2 text-sm" role="row">
          <div className="w-28 shrink-0 truncate text-right text-foreground" title={s.key} role="cell">
            {s.key}
          </div>
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
  const [tab, setTab] = useState<'industry' | 'stock'>('industry');
  // 两级行业切换（已裁定）：大类 L1 / 细分 L2（东财行业名直出），两级分母一致
  const [level, setLevel] = useState<'l1' | 'l2'>('l1');
  const [expanded, setExpanded] = useState<string | null>(null);
  const [fetchingDisclosure, setFetchingDisclosure] = useState(false);
  const [fetchingProfiles, setFetchingProfiles] = useState(false);
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

  useEffect(() => {
    void load();
  }, [load]);

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
          穿透基于最新披露权重（不放大、未穿透单列），季报约滞后 15 个工作日、中报/年报滞后 2~3 个月；
          {showDay
            ? ' 当日贡献为基于披露权重的近似值（未穿透部分不计入；境外股票按其市场行情时点，与 A 股可能不同步）。'
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
          </div>

          {tab === 'industry' && (
            <Card
              title="行业穿透"
              action={
                <div className="flex items-center gap-0.5 rounded-md border border-border bg-background p-0.5" role="group" aria-label="行业层级">
                  <button
                    onClick={() => setLevel('l1')}
                    aria-pressed={level === 'l1'}
                    className={`rounded px-2 py-1 text-xs transition-colors touch-target ${
                      level === 'l1' ? 'bg-primary text-on-primary' : 'text-muted hover:bg-surface'
                    }`}
                  >
                    大类
                  </button>
                  <button
                    onClick={() => setLevel('l2')}
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
              <IndustryBars slices={slices} showDay={showDay} />
              <div className="mt-2 border-t border-border/60 pt-2 text-xs text-muted">
                「未穿透」= 现金 / 债券 / 未披露部分（虚线桶，不放大归一）；「境外资产」= 港股 / 美股（P0 整桶，细分在 P1）；
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
                        <td className="tnum py-1.5 text-right text-muted">{fmtMv(f.unpenetratedMv)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Card>
          )}

          <div className="rounded-md border border-border bg-surface px-3 py-2 text-xs text-muted">
            基金两两重合矩阵（识别「伪分散」）规划在 P1 交付；本页所有输出为持仓结构分析，不构成投资建议。
          </div>
        </>
      )}
    </div>
  );
}
