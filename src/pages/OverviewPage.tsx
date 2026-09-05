// 持仓总览页 — 组合汇总 + 持仓列表 + 实时刷新（交易时段）
import { useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw, CircleAlert, Download, CloudDownload } from 'lucide-react';
import { getOverview, deleteFund, fetchAllDisclosures, refreshOfficialNav, gridTodaySignals, type OverviewResult, type GridTodayBadge } from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, EmptyState } from '../components/ui';
import PositionTable from '../components/PositionTable';

export default function OverviewPage() {
  const { platform } = usePlatform();
  const [data, setData] = useState<OverviewResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastUpd, setLastUpd] = useState('');
  const [fetchingDisclosure, setFetchingDisclosure] = useState(false);
  const [refreshingNav, setRefreshingNav] = useState(false);
  // 今日策略信号徽标（只读轻量命令，不触发计算；跨平台按 fund_code 聚合）
  const [signals, setSignals] = useState<Record<string, GridTodayBadge>>({});
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
      // 顺带轻读今日策略信号（失败静默：未启用策略时该命令为空列表）
      void gridTodaySignals()
        .then((list) => setSignals(Object.fromEntries(list.map((b) => [b.fundCode, b]))))
        .catch(() => {});
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

  const handleRefreshOfficialNav = useCallback(async () => {
    if (
      !confirm(
        '刷新今日官方净值（仅对尚未取到的基金发起请求）？\n盘后点击可为已收盘基金补全当日实际收益；盘中今日净值尚未发布，已持有最新净值的基金会自动跳过。',
      )
    )
      return;
    setRefreshingNav(true);
    try {
      const r = await refreshOfficialNav();
      await load(); // 重载总览，使「实际」标签生效
      const parts: string[] = [];
      if (r.gotToday > 0) parts.push(`已为 ${r.gotToday} 只基金取到今日官方净值（盘面将显示「实际」）`);
      const otherFetched = r.fetched - r.gotToday;
      if (otherFetched > 0) parts.push(`${otherFetched} 只更新为最新净值（多为 QDII T+1 滞后，仍显示「估算」）`);
      parts.push(`${r.skipped} 只已是最新无需刷新`);
      let msg = `刷新完成（${r.at}）：${parts.join('；')}。`;
      if (r.failed > 0) msg += `\n抓取失败 ${r.failed} 只：${r.failedCodes.join(', ')}`;
      alert(msg);
    } catch (e) {
      alert(`刷新失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setRefreshingNav(false);
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

  const { summary, positions, marketSession } = data;

  // 头条口径：盘中展示估算，盘后/休市展示实际
  const headlineEst = marketSession === 'intraday';
  const showDay = marketSession !== 'closed';

  return (
    <div className="p-4 space-y-4">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold">持仓总览</h1>
          <p className="text-xs text-muted mt-0.5">
            {marketSession === 'intraday'
              ? '交易时段 · 盘中估算每 15 分钟刷新（本地持仓穿透 + 行情，显示当日估算，实际待收盘）'
              : marketSession === 'post_close'
                ? '盘后 · 当日净值已确认，展示当日实际收益'
                : '休市 · 今日无交易，数据为最近交易日'} · 更新于 {lastUpd}
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
            onClick={() => void handleRefreshOfficialNav()}
            disabled={refreshingNav}
            title="仅对尚未取到今日官方净值的基金发起请求（盘后补全当日实际收益）"
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-3 py-1.5 text-sm text-foreground hover:bg-border/60 disabled:opacity-50"
          >
            <CloudDownload size={16} className={refreshingNav ? 'animate-pulse' : ''} aria-hidden />
            {refreshingNav ? '刷新净值中…' : '刷新今日净值'}
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

      {marketSession === 'closed' && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          休市中（周末/开盘前/节假日），今日无交易，当日收益暂不展示；开盘后自动切换为当日实时估算。
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
          value={showDay ? <GainLossBadge value={summary.estDayPnl} format="amount" /> : '—'}
          sublabel={showDay ? <GainLossBadge value={summary.dayPnlPctEst} format="pct" /> : undefined}
          tone={summary.estDayPnl > 0 ? 'gain' : summary.estDayPnl < 0 ? 'loss' : 'neutral'}
        />
        <StatTile
          label="当日实际收益"
          value={headlineEst || !showDay ? '—' : <GainLossBadge value={summary.actDayPnl} format="amount" />}
          sublabel={!headlineEst && showDay ? <GainLossBadge value={summary.dayPnlPctAct} format="pct" /> : undefined}
          tone={summary.actDayPnl > 0 ? 'gain' : summary.actDayPnl < 0 ? 'loss' : 'neutral'}
        />
      </div>

      {summary.risk && (
        <Card title="进阶风险（基于历史净值序列）">
          <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-sm">
            <div>
              <div className="text-xs text-muted mb-0.5">区间累计收益</div>
              <GainLossBadge value={summary.risk.cumulativeReturnPct / 100} format="pct" />
            </div>
            <div>
              <div className="text-xs text-muted mb-0.5">年化收益</div>
              <GainLossBadge value={summary.risk.annualizedReturnPct / 100} format="pct" />
            </div>
            <div>
              <div className="text-xs text-muted mb-0.5">年化波动率</div>
              <span className="tnum font-medium">{summary.risk.annualizedVolPct.toFixed(2)}%</span>
            </div>
            <div>
              <div className="text-xs text-muted mb-0.5">最大回撤</div>
              <span className="tnum font-medium text-loss">{summary.risk.maxDrawdownPct.toFixed(2)}%</span>
            </div>
          </div>
          <div className="mt-2 text-xs text-muted">统计区间 {summary.risk.days} 个交易日（按当前份额恒定近似聚合组合净值）</div>
        </Card>
      )}

      <PositionTable
        positions={positions}
        marketSession={marketSession}
        signals={signals}
        onDelete={handleDelete}
      />
    </div>
  );
}
