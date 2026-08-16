// 持仓总览页 — 组合汇总 + 持仓列表 + 实时刷新（交易时段）
import { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { RefreshCw, CircleAlert, Trash2, Download } from 'lucide-react';
import { getOverview, deleteFund, fetchAllDisclosures, type OverviewResult } from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, EmptyState, PlatformBadge, ConfidenceBadge } from '../components/ui';

function DelayTag({ note }: { note: string }) {
  return (
    <span
      title={note}
      className="rounded border border-warning/40 bg-warning/10 px-1 py-0.5 text-xs font-normal text-warning"
    >
      T+1
    </span>
  );
}

export default function OverviewPage() {
  const { platform } = usePlatform();
  const [data, setData] = useState<OverviewResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastUpd, setLastUpd] = useState('');
  const [fetchingDisclosure, setFetchingDisclosure] = useState(false);
  // 在途节流：刷新（手动按钮 / 自动定时器 / 平台切换）可能重叠触发，
  // 用 ref 守卫丢弃已在途的后续调用，避免慢速命令被叠加、UI 反复转圈。
  const fetchingRef = useRef(false);

  const load = useCallback(async () => {
    if (fetchingRef.current) return; // 已有刷新在途，丢弃本次（去抖重叠触发）
    fetchingRef.current = true;
    setLoading(true);
    setError(null);
    try {
      const r = await getOverview(platform);
      setData(r);
      setLastUpd(r.asOf);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      console.error('[FundLens] getOverview failed:', e);
    } finally {
      setLoading(false);
      fetchingRef.current = false;
    }
  }, [platform]);

  const handleDelete = useCallback(
    async (code: string, name: string) => {
      if (!confirm(`确定删除「${name}」及其持仓/披露记录吗？此操作不可撤销。`)) return;
      try {
        await deleteFund(code);
        await load();
      } catch (e) {
        alert(`删除失败：${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [load],
  );

  const handleFetchAllDisclosures = useCallback(async () => {
    if (!confirm('一键抓取所有基金的披露持仓（前十大重仓）？\n将逐只从公开数据源拉取最新季报持仓，耗时随基金数量增加，请耐心等待。')) return;
    setFetchingDisclosure(true);
    try {
      const r = await fetchAllDisclosures();
      await load();
      if (r.failed === 0) {
        alert(`已抓取 ${r.ok}/${r.total} 只基金的披露持仓（${r.at}）。估值已刷新。`);
      } else {
        alert(`抓取完成：${r.ok} 成功 / ${r.failed} 失败（共 ${r.total} 只）。\n失败基金代码：${r.failedCodes.join(', ')}`);
      }
    } catch (e) {
      alert(`抓取失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setFetchingDisclosure(false);
    }
  }, [load]);

  useEffect(() => {
    void load();
  }, [load]);

  // 交易时段每 15 分钟自动刷新（SPEC 要求：抑制出站频率、避免无谓卡顿）；非交易时段不刷新。
  // load() 自带在途节流，手动刷新按钮与上述定时器不会叠加阻塞。
  useEffect(() => {
    if (!data?.trading) return;
    const t = setInterval(() => void load(), 15 * 60 * 1000);
    return () => clearInterval(t);
  }, [data?.trading, load]);

  if (loading && !data) return <div className="p-6"><EmptyState title="加载中…" /></div>;
  if (error) return (
    <div className="p-6 space-y-3">
      <EmptyState title="加载失败" hint={error} />
      <button onClick={() => void load()} className="rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover">重试</button>
    </div>
  );
  if (!data) return <div className="p-6"><EmptyState title="暂无数据" hint="请先在「截图导入」中添加持仓" /></div>;

  const { summary, positions, trading, marketSession } = data;

  // 「当日」列来源角标：交易中=实时 / 盘后=实际 / 休市=上一日
  const daySession = (() => {
    switch (marketSession) {
      case 'intraday':
        return { label: '实时', cls: 'text-primary border-primary/40 bg-primary/10' };
      case 'post_close':
        return { label: '实际', cls: 'text-success border-success/40 bg-success/10' };
      default:
        return { label: '上一日', cls: 'text-muted border-border bg-border/40' };
    }
  })();

  return (
    <div className="p-4 space-y-4">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">持仓总览</h1>
          <p className="text-xs text-muted mt-0.5">
            {trading
              ? '交易时段 · 实时估算每 15 分钟刷新（显示当日估算，实际待收盘）'
              : marketSession === 'post_close'
                ? '盘后 · 当日实际收益已确定（估算与之一致）'
                : '休市 · 估算不可用，实际为上一交易日收益'} · 更新于 {lastUpd}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void handleFetchAllDisclosures()}
            disabled={fetchingDisclosure}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-3 py-1.5 text-sm text-foreground hover:bg-border/60 disabled:opacity-50"
          >
            <Download size={16} className={fetchingDisclosure ? 'animate-pulse' : ''} aria-hidden />
            {fetchingDisclosure ? '抓取中…' : '抓取披露持仓'}
          </button>
          <button
            onClick={() => void load()}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} aria-hidden />
            刷新
          </button>
        </div>
      </header>

      {marketSession === 'prev_day' && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          休市中（周末/开盘前/节假日），当日收益展示上一交易日实际收益；开盘后自动切换为当日实时估算。
        </div>
      )}

      {positions.some((p) => p.delayNote) && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          QDII 基金净值 T+1/T+2 确认：海外交易中不报「当日」收益；标 <span className="font-medium">T+1</span> 的估算为上一海外交易日净值变动，并非 A 股当日涨跌。
        </div>
      )}

      <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
        <StatTile label="总市值" value={`¥${summary.totalMarketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`} />
        <StatTile
          label="累计盈亏"
          value={<GainLossBadge value={summary.totalPnl} format="amount" />}
          tone={summary.totalPnl > 0 ? 'gain' : summary.totalPnl < 0 ? 'loss' : 'neutral'}
        />
        <StatTile
          label="当日估算收益"
          value={marketSession === 'prev_day' ? '—' : <GainLossBadge value={summary.estDayPnl} format="amount" />}
          tone={summary.estDayPnl > 0 ? 'gain' : summary.estDayPnl < 0 ? 'loss' : 'neutral'}
        />
        <StatTile
          label="当日实际收益"
          value={marketSession === 'intraday' ? '—' : <GainLossBadge value={summary.actDayPnl} format="amount" />}
          tone={summary.actDayPnl > 0 ? 'gain' : summary.actDayPnl < 0 ? 'loss' : 'neutral'}
        />
      </div>

      <Card
        title={`持仓明细（${positions.length}）`}
        action={
          <span className="text-xs text-muted flex items-center gap-1.5">
            置信度
            <ConfidenceBadge level="high" showLabel={false} />
            高
            <ConfidenceBadge level="medium" showLabel={false} />
            中
            <ConfidenceBadge level="low" showLabel={false} />
            低
          </span>
        }
      >
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-xs text-muted border-b border-border">
                <th className="py-1.5 pr-2 font-medium">基金</th>
                <th className="py-2 pr-3 font-medium">平台</th>
                <th className="py-2 pr-3 font-medium text-right">估算净值</th>
                <th className="py-2 pr-3 font-medium text-right">
                  当日
                  <span className={`ml-1 inline-block rounded border px-1 py-0.5 text-xs font-normal ${daySession.cls}`}>{daySession.label}</span>
                </th>
                <th className="py-2 pr-3 font-medium text-right">市值</th>
                <th className="py-2 pr-3 font-medium text-right">累计盈亏</th>
                <th className="py-2 pr-3 font-medium text-right">估值</th>
                <th className="py-2 pr-3 font-medium text-right">操作</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((p) => (
                <tr key={p.fund.code} className="border-b border-border/60 last:border-0 hover:bg-background/60">
                  <td className="py-1.5 pr-2">
                    <Link to={`/fund/${p.fund.code}`} className="font-medium text-foreground hover:text-primary">
                      {p.fund.name}
                    </Link>
                    <div className="text-xs text-muted tnum">{p.fund.code}</div>
                    {!p.fund.valuationApplicable && (
                      <span className="mt-0.5 inline-block rounded bg-border/60 px-1.5 py-0.5 text-xs text-muted">模型不适用</span>
                    )}
                  </td>
                  <td className="py-1.5 pr-2"><PlatformBadge code={p.fund.platform} /></td>
                  <td className="py-1.5 pr-2 text-right tnum">{p.estNav.toFixed(4)}</td>
                  <td className="py-1.5 pr-2 text-right">
                    {marketSession === 'prev_day' || p.delayNote === 'T+1·海外交易中' ? (
                      <span className="inline-flex items-center gap-1 text-muted">
                        <span>—</span>
                        {p.delayNote && <DelayTag note={p.delayNote} />}
                      </span>
                    ) : (
                      <span className="inline-flex items-center gap-1.5">
                        <GainLossBadge value={p.estChangePct} format="pct" />
                        {p.delayNote === 'T+1·海外净值' && <DelayTag note={p.delayNote} />}
                      </span>
                    )}
                  </td>
                  <td className="py-1.5 pr-2 text-right tnum">¥{p.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
                  <td className="py-1.5 pr-2 text-right">
                    <GainLossBadge value={p.totalPnl} format="amount" />
                    <div className="text-xs text-muted tnum"><GainLossBadge value={p.totalPnlPct} format="pct" /></div>
                  </td>
                  <td className="py-1.5 pr-2 text-right">
                    <div className="text-xs text-muted">
                      {p.valuationSource === 'realtime'
                        ? '实时'
                        : p.estimated
                          ? `${(p.disclosedWeightSum * 100).toFixed(0)}%`
                          : '—'}
                    </div>
                    {p.valuationSource && p.confidence && p.confidence !== 'none' && (
                      <div className="mt-0.5 flex justify-end">
                        <ConfidenceBadge level={p.confidence} showLabel={false} />
                      </div>
                    )}
                  </td>
                  <td className="py-1.5 pr-2 text-right">
                    <button
                      onClick={() => void handleDelete(p.fund.code, p.fund.name)}
                      title="删除持仓"
                      className="inline-flex items-center justify-center rounded p-1.5 text-muted hover:bg-border/60 hover:text-danger"
                    >
                      <Trash2 size={16} aria-hidden />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Card>
    </div>
  );
}
