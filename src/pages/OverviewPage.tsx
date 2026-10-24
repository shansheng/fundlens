// 持仓总览页 — 组合汇总 + 持仓列表 + 实时刷新（交易时段）
import { useCallback, useEffect, useRef, useState } from 'react';
import { RefreshCw, CircleAlert, Download, CloudDownload, X } from 'lucide-react';
import { getOverview, deleteFund, gridTodaySignals, type OverviewResult, type GridTodayBadge } from '../api';
import { usePlatform } from '../App';
import { GainLossBadge } from '../components/GainLossBadge';
import { Card, StatTile, EmptyState } from '../components/ui';
import PositionTable from '../components/PositionTable';
import { useNarrow } from '../hooks/useNarrow';
import { useFetchTask } from '../hooks/useFetchTask';
import { todayStr } from '../lib/date';
import { estDisplay } from '../lib/estDisplay';

// 取 navDate 的 MM-DD 切片（如 09-11）；非法/空返回 null。
function mmdd(navDate?: string | null): string | null {
  if (!navDate || navDate.length < 10) return null;
  return navDate.slice(5);
}

export default function OverviewPage() {
  const { platform } = usePlatform();
  const [data, setData] = useState<OverviewResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [lastUpd, setLastUpd] = useState('');
  // 今日策略信号徽标（只读轻量命令，不触发计算；跨平台按 fund_code 聚合）
  const [signals, setSignals] = useState<Record<string, GridTodayBadge>>({});
  // 在途节流：刷新（手动按钮 / 自动定时器 / 平台切换）可能重叠触发，
  // 用 ref 守卫丢弃已在途的后续调用，避免慢速命令被叠加、UI 反复转圈。
  const fetchingRef = useRef(false);
  // 窄屏（<lg）双路径：总计区 hero 条 + 持仓卡片；≥lg 桌面四格 + 宽表零回归
  const narrow = useNarrow();

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

  // 批量披露抓取：后台任务 + 轮询进度（2026-09-11 卡死修复——抓取不再阻塞主线程）
  const { progress: discProgress, running: fetchingDisclosure, start: startDisclosureFetch, cancel: cancelDisclosureFetch } = useFetchTask('disclosure_fetch', async (p) => {
    await load();
    if (p.cancelled) {
      alert(`已取消：完成 ${p.done}/${p.total}（成功 ${p.ok} / 失败 ${p.failed}）`);
    } else if (p.failed === 0) {
      alert(`已抓取 ${p.ok}/${p.total} 只基金的披露持仓（${p.finishedAt ?? ''}）。估值已刷新。`);
    } else {
      alert(`抓取完成：${p.ok} 成功 / ${p.failed} 失败（共 ${p.total} 只）。\n失败基金代码：${p.failedCodes.join(', ')}`);
    }
  });

  const handleFetchAllDisclosures = useCallback(async () => {
    if (!confirm('一键抓取所有持仓基金的披露持仓（前十大重仓）？\n将在后台逐只拉取，可随时取消，期间可正常使用其他页面。已清仓基金不抓取。')) return;
    try {
      await startDisclosureFetch();
    } catch (e) {
      alert(`抓取失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [startDisclosureFetch]);

  // 今日净值刷新：同款后台任务 + 轮询进度（2026-09-11——与披露抓取共用通用状态机）
  const { progress: navProgress, running: refreshingNav, start: startNavRefresh, cancel: cancelNavRefresh } = useFetchTask('nav_refresh', async (p) => {
    await load(); // 重载总览，使「实际」标签生效
    if (p.cancelled) {
      alert(`已取消：完成 ${p.done}/${p.total}（成功 ${p.ok} / 跳过 ${p.skipped} / 失败 ${p.failed}）`);
      return;
    }
    const parts: string[] = [];
    if (p.gotToday > 0) parts.push(`已为 ${p.gotToday} 只基金取到今日官方净值（盘面将显示「实际」）`);
    const otherFetched = p.ok - p.gotToday;
    if (otherFetched > 0) parts.push(`${otherFetched} 只更新为最新净值（多为 QDII T+1 滞后，仍显示「估算」）`);
    parts.push(`${p.skipped} 只已是最新无需刷新`);
    let msg = `刷新完成（${p.finishedAt ?? ''}）：${parts.join('；')}。`;
    if (p.failed > 0) msg += `\n抓取失败 ${p.failed} 只：${p.failedCodes.join(', ')}`;
    alert(msg);
  });

  const handleRefreshOfficialNav = useCallback(async () => {
    if (
      !confirm(
        '刷新今日官方净值（仅持仓基金，且只对尚未取到的发起请求）？\n将在后台逐只拉取，可随时取消，期间可正常使用其他功能；盘中今日净值尚未发布，已持有最新净值的基金会自动跳过。',
      )
    )
      return;
    try {
      await startNavRefresh();
    } catch (e) {
      alert(`刷新失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [startNavRefresh]);

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

  // 头条口径：盘中展示估算，盘后/休市展示实际（上一交易日实际）
  const headlineEst = marketSession === 'intraday';
  const today = todayStr();
  const lastNavDate = summary.lastNavDate ?? null;
  // 组合级「实际」数据日期：取各持仓官方净值日（funds.nav_date）的最新值。当日实际收益
  // （actDayPnl）= Σ份额×(official_nav−prev_nav)，描述的是 nav_date 日；而 lastNavDate 只是
  // position_daily 种子写入日（app 运行日），周末/未开机会停留在更早日期（如 10-22），
  // 拿它当实际列标签会张冠李戴（2026-10-24 用户报障：实际/估算两格日期都不对）。
  const portfolioNavDate = positions.reduce<string | null>(
    (max, p) => (p.navDate && (!max || p.navDate > max) ? p.navDate : max),
    null,
  );
  const navDateForLabel = portfolioNavDate ?? lastNavDate;
  const showLastBadge = navDateForLabel != null && navDateForLabel !== today;
  // 「估算收益」格与行级（lib/estDisplay）同口径：实时优先、种子回填。旧实现只加
  // lastDayPnlEst 种子——周五晚没开 app 就停在 10-22 旧值，而行已显 10-23 实时收盘
  // 估值，头部与逐行两张皮（2026-10-24 用户报障：头部估算收益不更新）。
  const estRows = positions.map((p) => ({ p, est: estDisplay(p, marketSession) }));
  const portfolioLastEst = headlineEst
    ? null
    : estRows.reduce<number | null>((acc, { est }) => (est.val == null ? acc : (acc ?? 0) + est.val), null);
  // 组合级估算收益率：非盘中=Σ估算÷Σ(市值−当日实际)（与行级回填口径同分母）；盘中走后端汇总。
  const portfolioEstPct = (() => {
    if (headlineEst) return summary.dayPnlPctEst;
    if (portfolioLastEst == null) return null;
    let base = 0;
    for (const { p, est } of estRows) {
      if (est.val == null) continue;
      base += p.marketValue - p.dayPnlAct;
    }
    // 分母恒为正的「昨收市值」口径；负数/零（测试桩或异常数据）时不给百分比，避免符号翻转
    return base > 0 ? portfolioLastEst / base : null;
  })();
  // 组合级估算日期：优先取实时行的最新 navDate（估算描述该日收盘）；无实时行（全回填）时
  // 用种子日期 lastNavDate。混合行取最新者——头部是汇总口径，以最新数据日期标注。
  const portfolioEstDate = headlineEst
    ? null
    : (() => {
        let max: string | null = null;
        for (const { p, est } of estRows) {
          if (!est.fromLive || est.val == null || !p.navDate) continue;
          if (!max || p.navDate > max) max = p.navDate;
        }
        return max ?? lastNavDate;
      })();

  return (
    <div className="p-3 space-y-2.5">
      <header className="flex items-center justify-between gap-3 flex-wrap">
        <div>
          <h1 className="text-xl font-semibold">持仓总览</h1>
          <p className="text-xs text-muted mt-0.5">
            {marketSession === 'intraday'
              ? '交易时段 · 盘中估算每 15 分钟刷新（本地持仓穿透 + 行情，显示当日估算，实际待收盘）'
              : marketSession === 'post_close'
                ? '盘后 · 当日净值已确认，展示当日实际收益'
                : `休市 · 显示上一交易日${navDateForLabel ? `（${mmdd(navDateForLabel)}）` : ''}收益；开盘后自动切换为当日实时估算`} · 更新于 {lastUpd}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={() => void handleFetchAllDisclosures()}
            disabled={fetchingDisclosure}
            title={fetchingDisclosure && discProgress ? `正在抓取 ${discProgress.current ?? ''}` : undefined}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
          >
            <Download size={16} className={fetchingDisclosure ? 'animate-pulse' : ''} aria-hidden />
            {fetchingDisclosure && discProgress ? `抓取中 ${discProgress.done}/${discProgress.total}` : '抓取披露持仓'}
          </button>
          {fetchingDisclosure && (
            <button
              onClick={() => void cancelDisclosureFetch()}
              title="取消本次批量抓取（当前这只跑完即停）"
              className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 touch-target"
            >
              <X size={16} aria-hidden />
              取消抓取
            </button>
          )}
          <button
            onClick={() => void handleRefreshOfficialNav()}
            disabled={refreshingNav}
            title={refreshingNav && navProgress ? `正在刷新 ${navProgress.current ?? ''}` : undefined}
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 disabled:opacity-50 touch-target"
          >
            <CloudDownload size={16} className={refreshingNav ? 'animate-pulse' : ''} aria-hidden />
            {refreshingNav && navProgress ? `刷新净值中 ${navProgress.done}/${navProgress.total}` : '刷新今日净值'}
          </button>
          {refreshingNav && (
            <button
              onClick={() => void cancelNavRefresh()}
              title="取消本次净值刷新（当前这只跑完即停）"
              className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-2.5 py-1 text-sm text-foreground hover:bg-border/60 touch-target"
            >
              <X size={16} aria-hidden />
              取消刷新
            </button>
          )}
          <button
            onClick={() => void load()}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-2.5 py-1 text-sm text-on-primary hover:bg-primary-hover touch-target"
          >
            <RefreshCw size={16} className={loading ? 'animate-spin' : ''} aria-hidden />
            刷新
          </button>
        </div>
      </header>

      {marketSession === 'closed' && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          休市中（周末/开盘前/节假日），今日无交易，显示上一交易日{navDateForLabel ? `（${mmdd(navDateForLabel)}）` : ''}收益；开盘后自动切换为当日实时估算。
        </div>
      )}

      {positions.some((p) => p.delayNote) && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <CircleAlert size={16} aria-hidden />
          QDII 基金净值 T+1/T+2 确认：海外交易中不报「当日」收益；标 <span className="font-medium">T+1</span> 的估算为上一海外交易日净值变动，并非 A 股当日涨跌。
        </div>
      )}

      {narrow ? (
        // 窄屏组合汇总条：合并「当日估算/实际」为一格（按时段自动选口径+角标），消灭休市双横杠
        <section className="rounded-md border border-border bg-surface p-3 shadow-ring" aria-label="组合汇总">
          <div className="flex items-center justify-between gap-2">
            <div className="text-xs text-muted">总市值</div>
            {marketSession === 'closed' ? (
              <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 text-[11px] font-medium text-muted">
                {showLastBadge ? mmdd(navDateForLabel) : '休市'}
              </span>
            ) : (
              <span
                className={`rounded border px-1.5 py-0.5 text-[11px] font-medium ${
                  marketSession === 'intraday'
                    ? 'border-primary/40 bg-primary/10 text-primary'
                    : 'border-success/40 bg-success/10 text-success'
                }`}
              >
                {marketSession === 'intraday' ? '估算' : '实际'}
              </span>
            )}
          </div>
          <div className="tnum mt-1 text-2xl font-semibold leading-none">
            ¥{summary.totalMarketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}
          </div>
          <div className="mt-1.5 flex items-center justify-between gap-2">
            <GainLossBadge value={headlineEst ? summary.estDayPnl : summary.actDayPnl} format="amount" />
            <span className="inline-flex items-center gap-1.5">
              <GainLossBadge value={headlineEst ? summary.dayPnlPctEst : summary.dayPnlPctAct} format="pct" />
              {showLastBadge && (
                <span className="rounded border border-border bg-border/40 px-1.5 py-0.5 text-[11px] font-medium text-muted">
                  {mmdd(navDateForLabel)}
                </span>
              )}
            </span>
          </div>
          <div className="mt-2 grid grid-cols-2 gap-2 border-t border-border/60 pt-2">
            <div className="min-w-0">
              <div className="text-xs text-muted">累计盈亏</div>
              <GainLossBadge value={summary.totalPnl} format="amount" />
              <div className="mt-0.5">
                <GainLossBadge value={summary.totalPnlPct} format="pct" subtle />
              </div>
            </div>
            <div className="min-w-0 text-right">
              <div className="text-xs text-muted">估算收益</div>
              {headlineEst ? (
                <>
                  <GainLossBadge value={summary.estDayPnl} format="amount" />
                  <div className="mt-0.5">
                    <GainLossBadge value={summary.dayPnlPctEst} format="pct" subtle />
                  </div>
                </>
              ) : portfolioLastEst != null ? (
                <>
                  <GainLossBadge value={portfolioLastEst} format="amount" />
                  {portfolioEstPct != null && (
                    <div className="mt-0.5">
                      <GainLossBadge value={portfolioEstPct} format="pct" subtle />
                    </div>
                  )}
                </>
              ) : (
                <span className="tnum text-sm text-muted">—</span>
              )}
            </div>
          </div>
        </section>
      ) : (
        <div className="grid grid-cols-2 md:grid-cols-4 gap-3">
          <StatTile label="总市值" value={`¥${summary.totalMarketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`} />
          <StatTile
            label="累计盈亏"
            value={<GainLossBadge value={summary.totalPnl} format="amount" />}
            tone={summary.totalPnl > 0 ? 'gain' : summary.totalPnl < 0 ? 'loss' : 'neutral'}
          />
          <StatTile
            label={headlineEst ? '当日估算收益' : portfolioEstDate ? `估算收益 ${mmdd(portfolioEstDate)}` : '估算收益'}
            value={headlineEst ? <GainLossBadge value={summary.estDayPnl} format="amount" /> : portfolioLastEst != null ? <GainLossBadge value={portfolioLastEst} format="amount" /> : '—'}
            sublabel={headlineEst ? <GainLossBadge value={summary.dayPnlPctEst} format="pct" /> : portfolioEstPct != null ? <GainLossBadge value={portfolioEstPct} format="pct" /> : undefined}
            tone={headlineEst ? (summary.estDayPnl > 0 ? 'gain' : summary.estDayPnl < 0 ? 'loss' : 'neutral') : portfolioLastEst != null ? (portfolioLastEst > 0 ? 'gain' : portfolioLastEst < 0 ? 'loss' : 'neutral') : 'neutral'}
          />
          <StatTile
            label={showLastBadge ? (navDateForLabel ? `实际收益 ${mmdd(navDateForLabel)}` : '实际收益') : '当日实际收益'}
            value={headlineEst ? '—' : <GainLossBadge value={summary.actDayPnl} format="amount" />}
            sublabel={!headlineEst ? <GainLossBadge value={summary.dayPnlPctAct} format="pct" /> : undefined}
            tone={!headlineEst ? (summary.actDayPnl > 0 ? 'gain' : summary.actDayPnl < 0 ? 'loss' : 'neutral') : 'neutral'}
          />
        </div>
      )}

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
