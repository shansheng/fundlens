// 策略信号页 —— 网格策略（valuation_grid 移植）信号建议层
// 只读建议：买入/卖出/观望 + 理由链；「执行」仅引导到记账页，绝不自动改持仓（v9）。
// 信号色沿用 A 股语义：buy=涨红(gain)/sell=跌绿(loss)/hold=muted；图标走 lucide，无 emoji。
import { useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  RefreshCw,
  Radar,
  Settings2,
  TrendingUp,
  TrendingDown,
  Minus,
  TriangleAlert,
  ChevronDown,
  ChevronRight,
  X,
} from 'lucide-react';
import {
  gridListConfig,
  gridEnableFund,
  gridSaveFund,
  gridComputeSignals,
  gridSignalHistory,
  gridSetRegime,
  gridGetSettings,
  gridBackfillOutcomes,
  gridListPending,
  gridPendingCancel,
  type GridConfigOut,
  type GridComputeResult,
  type GridHistoryRow,
  type GridSettingsOut,
  type GridSignalOut,
  type GridOutcomeStatRow,
  type GridPendingRow,
} from '../api';
import { Card, EmptyState } from '../components/ui';

const ACT: Record<string, string> = { buy: '买入', sell: '卖出', hold: '观望' };

function SignalPill({ sig, mini = false }: { sig: { action: string; signalName?: string | null; alert?: boolean }; mini?: boolean }) {
  const a = sig.action === 'buy' ? 'buy' : sig.action === 'sell' ? 'sell' : 'hold';
  const color =
    a === 'buy' ? 'var(--color-gain)' : a === 'sell' ? 'var(--color-loss)' : 'var(--color-muted)';
  const bg =
    a === 'buy' ? 'var(--color-gain-subtle)' : a === 'sell' ? 'var(--color-loss-subtle)' : 'transparent';
  const Icon = a === 'buy' ? TrendingUp : a === 'sell' ? TrendingDown : Minus;
  return (
    <span
      className="tnum inline-flex items-center gap-1 rounded-pill px-2 py-0.5 text-sm"
      style={{ color, background: bg, border: mini ? undefined : '1px solid color-mix(in srgb, ' + color + ' 40%, transparent)' }}
      title={sig.alert ? '需关注' : undefined}
    >
      <Icon size={mini ? 13 : 15} strokeWidth={2.2} aria-hidden />
      {sig.signalName || ACT[a] || sig.action}
      {sig.alert && <TriangleAlert size={13} aria-hidden />}
    </span>
  );
}

function fmtMoney(v: number) {
  return `¥${v.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}`;
}

export default function StrategyPage() {
  const [params] = useSearchParams();
  const focusCode = params.get('focus') ?? params.get('code');

  const [configs, setConfigs] = useState<GridConfigOut[] | null>(null);
  const [result, setResult] = useState<GridComputeResult | null>(null);
  const [settings, setSettings] = useState<GridSettingsOut | null>(null);
  const [hist, setHist] = useState<Record<string, GridHistoryRow[]>>({});
  const [openHist, setOpenHist] = useState<string | null>(focusCode);
  const [busy, setBusy] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [regimeDraft, setRegimeDraft] = useState<'neutral' | 'bear'>('neutral');
  const [autoDraft, setAutoDraft] = useState(true);
  const [manualDraft, setManualDraft] = useState(false);
  const [cashDraft, setCashDraft] = useState<string>('');
  const [pending, setPending] = useState<GridPendingRow[]>([]);
  const [stats, setStats] = useState<GridOutcomeStatRow[] | null>(null);
  const [statsUpdated, setStatsUpdated] = useState<number | null>(null);
  const [pendingBusy, setPendingBusy] = useState(false);
  const [showPending, setShowPending] = useState(true);

  const loadPending = useCallback(async () => {
    try {
      const rows = await gridListPending(null, 50);
      setPending(rows);
    } catch {
      /* 忽略挂单加载失败 */
    }
  }, []);

  const loadStats = useCallback(async () => {
    try {
      const r = await gridBackfillOutcomes();
      setStats(r.stats);
      setStatsUpdated(r.updated);
    } catch (e) {
      setError(`复盘回填失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, []);

  const loadAll = useCallback(async () => {
    try {
      const [c, s] = await Promise.all([gridListConfig(), gridGetSettings()]);
      setConfigs(c);
      setSettings(s);
      setRegimeDraft(s.regime === 'bear' ? 'bear' : 'neutral');
      setAutoDraft(s.auto);
      setManualDraft(s.manual);
      setCashDraft(s.cashAvailable ?? '');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void loadAll();
    void loadPending();
  }, [loadAll, loadPending]);

  const signalsByCode = useMemo(() => {
    const m: Record<string, GridSignalOut> = {};
    for (const s of result?.signals ?? []) m[s.fundCode] = s;
    return m;
  }, [result]);

  const handleCompute = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await gridComputeSignals();
      setResult(r);
      await loadPending();
    } catch (e) {
      setError(`信号计算失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [loadPending]);

  const handleToggle = useCallback(
    async (c: GridConfigOut, enabled: boolean) => {
      setBusy(true);
      try {
        await gridEnableFund(c.fundCode, enabled, c.maxPosition);
        await loadAll();
      } catch (e) {
        setError(`更新失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [loadAll],
  );

  const handleMaxPos = useCallback(
    async (c: GridConfigOut, v: string) => {
      const n = Number(v);
      if (!Number.isFinite(n) || n <= 0) return;
      setBusy(true);
      try {
        await gridEnableFund(c.fundCode, c.enabled, n);
        await loadAll();
      } catch (e) {
        setError(`保存失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [loadAll],
  );

  // P1：整行保存——灵敏度 / 卖出费率 / 冷却日（上限保留现值；仅保存改动字段）
  const handleSaveFundParam = useCallback(
    async (c: GridConfigOut, field: 'vol' | 'fee' | 'cooldown', raw: string) => {
      const cur = { vol: c.volSensitivity ?? null, fee: c.sellFeeRate ?? null, cooldown: c.cooldownSellDate ?? null };
      if (field === 'vol') {
        const n = Number(raw);
        if (!raw || !Number.isFinite(n)) return;
        if (n < 0.3 || n > 3.0) {
          setError('波动敏感度须在 0.3 ~ 3.0 之间');
          return;
        }
        cur.vol = Math.round(n * 100) / 100;
      } else if (field === 'fee') {
        const n = Number(raw);
        if (!raw || !Number.isFinite(n)) return;
        if (n < 0 || n > 2.0) {
          setError('卖出费率须在 0 ~ 2% 之间');
          return;
        }
        cur.fee = Math.round(n * 100) / 100 / 100; // % → 小数
      } else {
        cur.cooldown = raw || null;
      }
      setBusy(true);
      try {
        await gridSaveFund(c.fundCode, c.maxPosition ?? null, cur.vol, cur.fee, cur.cooldown);
        await loadAll();
      } catch (e) {
        setError(`保存失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [loadAll],
  );

  const handleCancelPending = useCallback(
    async (row: GridPendingRow) => {
      setPendingBusy(true);
      try {
        await gridPendingCancel(row.fundCode, row.id);
        await loadPending();
      } catch (e) {
        setError(`取消失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setPendingBusy(false);
      }
    },
    [loadPending],
  );

  const handleToggleHist = useCallback(
    async (code: string) => {
      if (openHist === code) {
        setOpenHist(null);
        return;
      }
      setOpenHist(code);
      if (!hist[code]) {
        try {
          const rows = await gridSignalHistory(code, 5);
          setHist((h) => ({ ...h, [code]: rows }));
        } catch {
          /* 忽略历史加载失败 */
        }
      }
    },
    [hist, openHist],
  );

  const handleSaveRegime = useCallback(async () => {
    try {
      const cashRaw = cashDraft.trim();
      const cashNum = cashRaw ? Number(cashRaw) : NaN;
      const cash = Number.isFinite(cashNum) && cashNum > 0 ? cashNum : 0; // 空/0 → 清空闸门
      await gridSetRegime(regimeDraft, autoDraft, manualDraft, cash);
      await loadAll();
      setShowSettings(false);
    } catch (e) {
      setError(`保存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [regimeDraft, autoDraft, manualDraft, cashDraft, loadAll]);

  const enabledCount = configs?.filter((c) => c.enabled).length ?? 0;
  const sellCount = (result?.signals ?? []).filter((s) => s.action === 'sell').length;
  const buyCount = (result?.signals ?? []).filter((s) => s.action === 'buy').length;
  const regimeText = result?.regime ?? settings?.regime ?? 'neutral';

  return (
    <div className="p-4 space-y-4">
      <header className="flex items-center justify-between gap-3 flex-wrap">
        <div>
          <h1 className="text-xl font-semibold flex items-center gap-2">
            <Radar size={20} className="text-primary" aria-hidden />
            策略信号
          </h1>
          <p className="text-xs text-muted mt-0.5">
            低频网格建议层（波动率自适应阈值 · 三级止损 · 止盈评分 · 逐级补仓）——仅供记录参考，非投资建议；买入/卖出请手动在「记账」页操作
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span
            className="inline-flex items-center gap-1.5 rounded-pill border border-border px-2.5 py-1 text-sm"
            title={result?.autoRegime === false ? '手动指认行情模式' : '自动识别（等权合成市场趋势）'}
          >
            <span className="text-xs text-muted">行情</span>
            <span className="font-medium">{regimeText === 'bear' ? '熊市' : '震荡'}</span>
            {result?.autoRegime === false && <span className="text-xs text-muted">手动</span>}
          </span>
          <button
            type="button"
            onClick={() => setShowSettings(true)}
            title="策略设置（行情模式 / 自动识别）"
            className="inline-flex items-center gap-1.5 rounded-md border border-border bg-background px-3 py-1.5 text-sm hover:bg-border/60"
          >
            <Settings2 size={15} aria-hidden />
            设置
          </button>
          <button
            type="button"
            onClick={() => void handleCompute()}
            disabled={busy || enabledCount === 0}
            className="inline-flex items-center gap-1.5 rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover disabled:opacity-50"
          >
            <RefreshCw size={15} className={busy ? 'animate-spin' : ''} aria-hidden />
            {busy ? '计算中…' : '刷新计算'}
          </button>
        </div>
      </header>

      {error && (
        <div className="flex items-center gap-2 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-sm text-danger">
          <TriangleAlert size={15} aria-hidden />
          {error}
        </div>
      )}

      {enabledCount === 0 && (
        <div className="flex items-center gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-sm text-warning">
          <TriangleAlert size={15} aria-hidden />
          尚未启用任何策略基金：从下方持仓卡片中挑 1–2 只权益/指数基金，填「上限」并打开右侧开关即可启用（已按持仓金额排序，货基/理财不在候选）。
        </div>
      )}

      {configs && configs.length > 0 && (
        <div className="rounded-lg border border-border bg-surface overflow-hidden">
          <div className="flex items-center justify-between px-3 py-2">
            <div className="flex items-center gap-3 text-sm">
              <button
                type="button"
                onClick={() => setShowPending((v) => !v)}
                className="inline-flex items-center gap-1 font-medium hover:text-primary"
              >
                {showPending ? <ChevronDown size={15} /> : <ChevronRight size={15} />}
                延迟回补挂单
                <span className="tnum rounded bg-border/60 px-1.5 text-xs font-normal text-muted">
                  {pending.filter((p) => p.status === 'pending').length} 待触发
                </span>
              </button>
              <button
                type="button"
                onClick={() => void loadStats()}
                disabled={pendingBusy || busy}
                className="inline-flex items-center gap-1 text-muted hover:text-foreground disabled:opacity-50"
                title="回填最近信号 T+3/5/10 方向收益并统计胜率（事件研究）"
              >
                <RefreshCw size={13} className={pendingBusy ? 'animate-spin' : ''} aria-hidden />
                刷新复盘
              </button>
            </div>
          </div>

          {showPending && (
            <div className="border-t border-border/60 overflow-x-auto">
              {pending.length === 0 ? (
                <div className="px-3 py-3 text-sm text-muted">
                  暂无延迟回补挂单。卖出信号产生回补计划时自动挂单（28 日内净值回落触发买入建议，可在「记账」页执行）。
                </div>
              ) : (
                <table className="w-full text-xs">
                  <thead>
                    <tr className="text-left text-muted border-b border-border/60">
                      <th className="px-3 py-1.5 font-medium">基金</th>
                      <th className="px-2 py-1.5 font-medium">触发信号</th>
                      <th className="px-2 py-1.5 font-medium text-right">触发净值</th>
                      <th className="px-2 py-1.5 font-medium text-right">金额</th>
                      <th className="px-2 py-1.5 font-medium text-right">回补</th>
                      <th className="px-2 py-1.5 font-medium">到期</th>
                      <th className="px-2 py-1.5 font-medium">状态</th>
                      <th className="px-2 py-1.5" />
                    </tr>
                  </thead>
                  <tbody>
                    {pending.map((p) => {
                      const st =
                        p.status === 'triggered'
                          ? { t: '已触发', c: 'var(--color-gain)' }
                          : p.status === 'cancelled'
                            ? { t: '已取消', c: 'var(--color-muted)' }
                            : p.status === 'expired'
                              ? { t: '已过期', c: 'var(--color-muted)' }
                              : { t: '待触发', c: 'var(--color-loss)' };
                      return (
                        <tr key={p.id} className="border-b border-border/40 last:border-0 align-middle">
                          <td className="px-3 py-1.5 tnum">{p.fundCode}</td>
                          <td className="px-2 py-1.5 text-muted truncate max-w-[180px]" title={p.signalLabel ?? ''}>
                            {p.signalLabel ?? p.sourceSignal ?? '—'}
                          </td>
                          <td className="px-2 py-1.5 tnum text-right">
                            ≤ {p.triggerNav != null ? p.triggerNav.toFixed(4) : '—'}
                          </td>
                          <td className="px-2 py-1.5 tnum text-right">{p.amount != null ? fmtMoney(p.amount) : '—'}</td>
                          <td className="px-2 py-1.5 tnum text-right">
                            {p.ratio != null ? `${Math.round(p.ratio * 100)}%` : '—'}
                          </td>
                          <td className="px-2 py-1.5 tnum text-muted">{p.expireDate ?? '—'}</td>
                          <td className="px-2 py-1.5" style={{ color: st.c }}>
                            {st.t}
                            {p.triggeredDate ? ` ${p.triggeredDate}` : ''}
                          </td>
                          <td className="px-2 py-1.5 text-right">
                            {p.status === 'pending' && (
                              <button
                                type="button"
                                onClick={() => void handleCancelPending(p)}
                                disabled={pendingBusy || busy}
                                className="rounded border border-border px-1.5 py-0.5 text-muted hover:bg-border/60 hover:text-foreground disabled:opacity-50"
                              >
                                取消
                              </button>
                            )}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              )}
            </div>
          )}

          {stats && stats.length > 0 && (
            <div className="border-t border-border/60">
              <div className="px-3 pt-2 text-xs text-muted">
                信号复盘（{statsUpdated ?? 0} 条已回填 · 方向收益：买入后涨 / 卖出后跌计为「对」）
              </div>
              <table className="w-full text-xs">
                <thead>
                  <tr className="text-left text-muted border-b border-border/60">
                    <th className="px-3 py-1.5 font-medium">动作</th>
                    <th className="px-2 py-1.5 font-medium text-right">样本</th>
                    <th className="px-2 py-1.5 font-medium text-right">胜率</th>
                    <th className="px-2 py-1.5 font-medium text-right">平均 T+3</th>
                    <th className="px-2 py-1.5 font-medium text-right">平均 T+5</th>
                    <th className="px-2 py-1.5 font-medium text-right">平均 T+10</th>
                  </tr>
                </thead>
                <tbody>
                  {stats.map((s) => {
                    const color = s.action === 'buy' ? 'var(--color-gain)' : s.action === 'sell' ? 'var(--color-loss)' : 'var(--color-muted)';
                    const cell = (v?: number | null) =>
                      v == null ? <span className="text-muted">—</span> : (
                        <span style={{ color: v >= 0 ? 'var(--color-gain)' : 'var(--color-loss)' }}>
                          {v >= 0 ? '+' : ''}
                          {v.toFixed(2)}%
                        </span>
                      );
                    return (
                      <tr key={s.action} className="border-b border-border/40 last:border-0">
                        <td className="px-3 py-1.5" style={{ color }}>
                          {s.action === 'buy' ? '买入' : s.action === 'sell' ? '卖出' : s.action}
                        </td>
                        <td className="px-2 py-1.5 tnum text-right">{s.count}</td>
                        <td className="px-2 py-1.5 tnum text-right" style={{ color: s.winRate >= 0.5 ? 'var(--color-gain)' : 'var(--color-loss)' }}>
                          {Math.round(s.winRate * 100)}%
                        </td>
                        <td className="px-2 py-1.5 tnum text-right">{cell(s.avgT3)}</td>
                        <td className="px-2 py-1.5 tnum text-right">{cell(s.avgT5)}</td>
                        <td className="px-2 py-1.5 tnum text-right">{cell(s.avgT10)}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      )}

      {(sellCount > 0 || buyCount > 0) && (
        <div className="flex items-center gap-3 rounded-md border border-border bg-surface px-3 py-2 text-sm">
          <span className="text-xs text-muted">今日建议</span>
          {buyCount > 0 && <SignalPill sig={{ action: 'buy', signalName: `买入 ${buyCount}` }} mini />}
          {sellCount > 0 && <SignalPill sig={{ action: 'sell', signalName: `卖出 ${sellCount}` }} mini />}
          <span className="text-xs text-muted ml-auto">共 {enabledCount} 只启用 · {result ? `计算于 ${result.computedAt}` : '尚未计算'}</span>
        </div>
      )}

      {result?.budgetCap != null && result.budgetCap > 0 && (
        <div className="flex items-center gap-3 rounded-md border border-border bg-surface px-3 py-2 text-sm">
          <span className="text-xs text-muted">现金闸门</span>
          <span className="tnum">
            今日买入{' '}
            <span className="font-medium" style={{ color: 'var(--color-gain)' }}>
              {fmtMoney(result.budgetUsed ?? 0)}
            </span>{' '}
            / 上限 {fmtMoney(result.budgetCap)}
          </span>
          <span className="text-xs text-muted">（可投现金预算的 20%，高优先级先买）</span>
        </div>
      )}

      <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
        {(configs ?? []).map((c) => {
          const sig = signalsByCode[c.fundCode];
          const rows = hist[c.fundCode];
          const focus = focusCode === c.fundCode;
          return (
            <Card
              key={c.fundCode}
              title={
                <span className="flex items-center gap-2">
                  <span className={focus ? 'text-primary' : ''}>{c.fundName || c.fundCode}</span>
                  <span className="tnum text-xs text-muted font-normal">{c.fundCode}</span>
                  {c.fundType && <span className="rounded bg-border/60 px-1.5 py-0.5 text-xs font-normal text-muted">{c.fundType}</span>}
                  {!c.enabled && (
                    <span className="rounded border border-border px-1.5 py-0.5 text-xs font-normal text-muted">未启用</span>
                  )}
                </span>
              }
              action={
                <div className="flex items-center gap-2">
                  <label className="flex items-center gap-1.5 text-xs text-muted" title="投入上限 max_position（出金额建议的必需项）">
                    上限
                    <input
                      type="number"
                      min={0}
                      step={1000}
                      defaultValue={c.maxPosition ?? ''}
                      key={`${c.fundCode}-${c.maxPosition ?? 'none'}`}
                      placeholder="如 20000"
                      onBlur={(e) => void handleMaxPos(c, e.target.value)}
                      className="w-24 rounded-md border border-border bg-background px-1.5 py-1 text-right tnum text-sm focus:outline-none focus:ring-1 focus:ring-primary"
                    />
                  </label>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={c.enabled}
                    onClick={() => void handleToggle(c, !c.enabled)}
                    disabled={busy}
                    className={`relative h-5 w-9 rounded-full transition-colors ${c.enabled ? 'bg-primary' : 'bg-border'}`}
                    title={c.enabled ? '停用策略' : '启用策略'}
                  >
                    <span
                      className={`absolute top-0.5 h-4 w-4 rounded-full bg-background transition-all ${c.enabled ? 'left-4.5' : 'left-0.5'}`}
                    />
                  </button>
                </div>
              }
            >
              <div className="space-y-2">
                <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded-md border border-border/60 bg-background/50 px-2 py-1.5 text-xs text-muted">
                  <span className="font-medium">参数</span>
                  <label title="波动敏感度：波动率阈值 = 基准 × 敏感度（0.3~3.0，默认 1.0）">
                    敏感度
                    <input
                      type="number"
                      min={0.3}
                      max={3}
                      step={0.1}
                      key={`${c.fundCode}-vol-${c.volSensitivity ?? 'd'}`}
                      defaultValue={c.volSensitivity ?? 1}
                      onBlur={(e) => void handleSaveFundParam(c, 'vol', e.target.value)}
                      className="ml-1 w-16 rounded border border-border bg-background px-1 py-0.5 text-right tnum focus:outline-none focus:ring-1 focus:ring-primary"
                    />
                  </label>
                  <label title="卖出费率（0~2%，用于净收益估算）">
                    卖出费率%
                    <input
                      type="number"
                      min={0}
                      max={2}
                      step={0.05}
                      key={`${c.fundCode}-fee-${c.sellFeeRate ?? 'd'}`}
                      defaultValue={c.sellFeeRate != null ? Math.round(c.sellFeeRate * 10000) / 100 : 0}
                      onBlur={(e) => void handleSaveFundParam(c, 'fee', e.target.value)}
                      className="ml-1 w-16 rounded border border-border bg-background px-1 py-0.5 text-right tnum focus:outline-none focus:ring-1 focus:ring-primary"
                    />
                  </label>
                  <label title="冷却卖出：该日期前不触发卖出建议（清空以解除）">
                    冷却至
                    <input
                      type="date"
                      key={`${c.fundCode}-cool-${c.cooldownSellDate ?? 'd'}`}
                      defaultValue={c.cooldownSellDate ?? ''}
                      onBlur={(e) => void handleSaveFundParam(c, 'cooldown', e.target.value)}
                      className="ml-1 rounded border border-border bg-background px-1 py-0.5 tnum focus:outline-none focus:ring-1 focus:ring-primary"
                    />
                  </label>
                  <span className="ml-auto text-[11px] text-muted/70">上限 ¥{fmtMoney(c.maxPosition ?? 0)}</span>
                </div>
                {c.enabled && sig ? (
                  <div className="space-y-1.5">
                    <div className="flex items-center gap-2 flex-wrap">
                      <SignalPill sig={sig} />
                      <span className="tnum text-xs text-muted">
                        净值 {sig.currentNav.toFixed(4)} · 今日
                        <span style={{ color: sig.estChangePct >= 0 ? 'var(--color-gain)' : 'var(--color-loss)' }}>
                          {' '}
                          {sig.estChangePct >= 0 ? '+' : ''}
                          {sig.estChangePct.toFixed(2)}%
                        </span>
                        {sig.source === 'estimation' ? '（盘中估值）' : '（净值）'}
                      </span>
                      {sig.totalProfitPct != null && (
                        <span className="tnum text-xs text-muted">累计盈亏 {sig.totalProfitPct >= 0 ? '+' : ''}{sig.totalProfitPct.toFixed(2)}%</span>
                      )}
                    </div>
                    {sig.action !== 'hold' && (
                      <div className="text-sm">
                        {sig.amount != null && (
                          <span className="mr-3 font-medium">
                            建议金额 <span className="tnum">{fmtMoney(sig.amount)}</span>
                          </span>
                        )}
                        {sig.sellShares != null && (
                          <span className="mr-3 font-medium">
                            建议卖出 <span className="tnum">{sig.sellShares.toFixed(2)}</span> 份
                            {sig.sellPct != null && <span className="text-muted">（{sig.sellPct}%）</span>}
                          </span>
                        )}
                        {sig.isRebuy && (
                          <span
                            className="mr-3 rounded px-1.5 py-0.5 text-xs font-medium"
                            style={{ color: 'var(--color-gain)', background: 'var(--color-gain-subtle)' }}
                            title={sig.pendingRebuyId != null ? `挂单 #${sig.pendingRebuyId} 触发` : '延迟回补触发'}
                          >
                            延迟回补触发
                          </span>
                        )}
                        {c.platforms.length > 0 && (
                          <span className="text-xs text-muted">平台：{c.platforms.join(' / ')}</span>
                        )}
                      </div>
                    )}
                    {sig.rebuyPlan && (
                      <div className="rounded-md border border-border bg-background p-2 text-xs space-y-0.5">
                        <div className="font-medium text-muted">
                          回补计划（{sig.rebuyPlan.trend === 'consolidate' ? '震荡' : '强势'} · 回补 {Math.round(sig.rebuyPlan.ratio * 100)}%）
                        </div>
                        <div className="tnum text-muted">
                          净值回落到 <span className="text-foreground">{sig.rebuyPlan.triggerNav.toFixed(4)}</span> 以下时建议买入{' '}
                          <span className="text-foreground">{fmtMoney(sig.rebuyPlan.amount)}</span>
                          {sig.rebuyPlan.discount > 0 && (
                            <>（较当前价回撤 {sig.rebuyPlan.discount.toFixed(2)}）</>
                          )}
                        </div>
                      </div>
                    )}
                    <p className="text-sm text-muted leading-relaxed">{sig.reason}</p>
                    {sig.fifoPlan && sig.fifoPlan.steps.length > 0 && (
                      <div className="rounded-md border border-border bg-background p-2 text-xs space-y-1">
                        <div className="font-medium text-muted">FIFO 卖出计划（{sig.fifoPlan.instruction}）</div>
                        {sig.fifoPlan.steps.map((st, i) => (
                          <div key={i} className="tnum flex justify-between gap-2">
                            <span className="text-muted">
                              {st.isPassthrough ? 'FIFO 穿过' : st.reason || '目标批次'} · {st.buyDate}
                              {st.isFullSell ? '（全卖）' : ''}
                            </span>
                            <span>
                              {st.sellShares != null ? `${st.sellShares.toFixed(2)} 份 · 预计 ` : '预计 '}
                              {st.estimatedNetProfit != null && st.estimatedNetProfit >= 0 ? '+' : ''}
                              {st.estimatedNetProfit != null ? fmtMoney(st.estimatedNetProfit) : '—'}
                            </span>
                          </div>
                        ))}
                        {sig.fifoPlan.hasPassthrough && sig.fifoPlan.passthroughWarning && (
                          <div className="text-warning">{sig.fifoPlan.passthroughWarning}</div>
                        )}
                      </div>
                    )}
                    <div className="flex items-center justify-between pt-0.5">
                      <button
                        type="button"
                        onClick={() => void handleToggleHist(c.fundCode)}
                        className="inline-flex items-center gap-1 text-xs text-muted hover:text-foreground"
                      >
                        {openHist === c.fundCode ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                        信号历史
                      </button>
                      <button
                        type="button"
                        onClick={() => alert(`请到「记账」页手动记录该建议（本应用不自动下单）：\n${ACT[sig.action] ?? sig.action} ${sig.amount ?? sig.sellShares ?? ''} ${c.fundCode}`)}
                        className="rounded-md border border-border px-2 py-1 text-xs hover:bg-border/60"
                      >
                        记录到记账
                      </button>
                    </div>
                    {openHist === c.fundCode && (
                      <div className="rounded-md border border-border bg-background text-xs">
                        {rows && rows.length > 0 ? (
                          <table className="w-full">
                            <tbody>
                              {rows.map((r, i) => (
                                <tr key={i} className="border-b border-border/50 last:border-0">
                                  <td className="px-2 py-1.5 tnum text-muted">{r.signal_date}</td>
                                  <td className="px-2 py-1.5">
                                    <SignalPill sig={{ action: r.action ?? 'hold', signalName: r.signal_name ?? null }} mini />
                                  </td>
                                  <td className="px-2 py-1.5 text-muted truncate max-w-[200px]" title={r.reason ?? ''}>
                                    {r.reason ?? ''}
                                  </td>
                                  {[r.outcome_t3, r.outcome_t5, r.outcome_t10].some((v) => v != null) && (
                                    <td className="px-2 py-1.5 tnum text-right whitespace-nowrap">
                                      {r.outcome_t3 != null && (
                                        <span className="mr-1.5" title="T+3 方向收益">
                                          T3 <span style={{ color: r.outcome_t3 >= 0 ? 'var(--color-gain)' : 'var(--color-loss)' }}>{r.outcome_t3 >= 0 ? '+' : ''}{r.outcome_t3.toFixed(2)}</span>
                                        </span>
                                      )}
                                      {r.outcome_t5 != null && (
                                        <span className="mr-1.5" title="T+5 方向收益">
                                          T5 <span style={{ color: r.outcome_t5 >= 0 ? 'var(--color-gain)' : 'var(--color-loss)' }}>{r.outcome_t5 >= 0 ? '+' : ''}{r.outcome_t5.toFixed(2)}</span>
                                        </span>
                                      )}
                                      {r.outcome_t10 != null && (
                                        <span title="T+10 方向收益">
                                          T10 <span style={{ color: r.outcome_t10 >= 0 ? 'var(--color-gain)' : 'var(--color-loss)' }}>{r.outcome_t10 >= 0 ? '+' : ''}{r.outcome_t10.toFixed(2)}</span>
                                        </span>
                                      )}
                                    </td>
                                  )}
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        ) : (
                          <div className="px-2 py-2 text-muted">暂无历史（执行「刷新计算」后每日自动落库）</div>
                        )}
                      </div>
                    )}
                  </div>
                ) : (
                  <div className="text-sm text-muted">
                    {c.enabled
                      ? '已启用，尚未计算——点击右上「刷新计算」生成今日建议'
                      : `当前持仓 ${c.shares > 0 ? `${c.shares.toFixed(2)} 份 / ${fmtMoney(c.costAmount)}` : '空仓'}。启用后按网格策略给出每日唯一建议`}
                  </div>
                )}
              </div>
            </Card>
          );
        })}
      </div>

      {!configs && <EmptyState title="加载中…" />}
      {configs && configs.length === 0 && (
        <EmptyState
          title="没有可用于策略的持仓"
          hint="本页会列出全部持仓基金供挑选（货基/理财除外）。当前没有可启用的持仓——先到「总览/截图导入」添加 1–2 只权益/指数基金，再回到本页开启网格策略"
        />
      )}

      {showSettings && settings && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
          role="dialog"
          aria-modal="true"
          aria-label="策略设置"
          onClick={() => setShowSettings(false)}
        >
          <div className="w-full max-w-sm rounded-lg border border-border bg-surface p-4 space-y-3" onClick={(e) => e.stopPropagation()}>
            <div className="flex items-center justify-between">
              <h2 className="text-base font-semibold">策略设置</h2>
              <button type="button" onClick={() => setShowSettings(false)} className="rounded p-1 text-muted hover:bg-border/60" aria-label="关闭">
                <X size={16} />
              </button>
            </div>
            <div>
              <div className="text-sm font-medium mb-1.5">行情模式</div>
              <div className="flex gap-2">
                {(['neutral', 'bear'] as const).map((r) => (
                  <button
                    key={r}
                    type="button"
                    onClick={() => {
                      setRegimeDraft(r);
                      setManualDraft(true);
                      setAutoDraft(false);
                    }}
                    className={`flex-1 rounded-md border px-3 py-1.5 text-sm ${
                      regimeDraft === r && manualDraft ? 'border-primary bg-primary/10 text-primary' : 'border-border hover:bg-border/40'
                    }`}
                  >
                    {r === 'bear' ? '熊市（保守建仓）' : '震荡（默认）'}
                  </button>
                ))}
              </div>
            </div>
            <label className="flex items-center justify-between text-sm">
              <span>自动识别行情（20日累跌超 10% 且趋势连跌/走弱 → 熊市）</span>
              <input
                type="checkbox"
                checked={autoDraft}
                onChange={(e) => {
                  setAutoDraft(e.target.checked);
                  if (e.target.checked) setManualDraft(false);
                }}
              />
            </label>
            <div>
              <div className="text-sm font-medium mb-1">可投现金预算（可选）</div>
              <input
                type="number"
                min={0}
                step={1000}
                value={cashDraft}
                onChange={(e) => setCashDraft(e.target.value)}
                placeholder="留空则不设闸门"
                className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-right tnum text-sm focus:outline-none focus:ring-1 focus:ring-primary"
              />
              <div className="mt-1 text-xs text-muted">
                组合级日买入闸门 = 该预算 × 20%，高优先级先买，超限缩额/暂缓。填 0 或留空并保存可关闭。
              </div>
            </div>
            <div className="text-xs text-muted">
              当前：{regimeDraft === 'bear' ? '熊市' : '震荡'} · {autoDraft ? '自动识别开启' : manualDraft ? '手动指认' : '自动识别关闭'}
              {settings?.cashAvailable ? ` · 现金预算 ¥${Number(settings.cashAvailable).toLocaleString('zh-CN')}` : ''}
            </div>
            <div className="flex justify-end gap-2 pt-1">
              <button type="button" onClick={() => setShowSettings(false)} className="rounded-md border border-border px-3 py-1.5 text-sm hover:bg-border/60">
                取消
              </button>
              <button type="button" onClick={() => void handleSaveRegime()} className="rounded-md bg-primary px-3 py-1.5 text-sm text-on-primary hover:bg-primary-hover">
                保存
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
