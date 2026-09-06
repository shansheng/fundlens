// 持仓明细表 —— 含「点击表头排序」（仅数值列：当日% / 估算收益 / 估算收益率 /
// 市值 / 累计盈亏 / 累计盈亏率），单列三态循环（升序→降序→恢复默认），排序状态
// 持久化到 localStorage。排序为纯视图变换，不影响底层数据与自动刷新。
//
// 列配置（2026-08-21 精简）：持仓占比、估算净值列已隐藏；当日估算收益拆为
// 「估算收益」「估算收益率」两列；累计盈亏拆为「累计盈亏」「累计盈亏率」两列；
// 估值列仅保留第一行（指数实时/披露持仓占比），去掉「本地自算」副行。
//
// 性能：行组件用 React.memo 包裹，排序仅改变行的先后顺序（props 引用不变），
// 不会重渲染每一行的 GainLossBadge/SVG，避免快速连点时主线程被打满卡死。
import { memo, useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { Link } from 'react-router-dom';
import { ChevronUp, ChevronDown, ChevronsUpDown, Trash2, TrendingUp, TrendingDown, Minus } from 'lucide-react';
import { type PositionRow, type GridTodayBadge } from '../api';
import { GainLossBadge } from './GainLossBadge';
import { Card, PlatformBadge } from './ui';
import { useNarrow } from '../hooks/useNarrow';

type MarketSession = 'intraday' | 'post_close' | 'closed';

type SortKey = 'estChangePct' | 'dayPnlEst' | 'dayPnlPctEst' | 'marketValue' | 'totalPnl' | 'totalPnlPct';
const ALLOWED: SortKey[] = ['estChangePct', 'dayPnlEst', 'dayPnlPctEst', 'marketValue', 'totalPnl', 'totalPnlPct'];
const LS_KEY = 'fundlens.overview.sort';

type SortState = { key: SortKey | null; dir: 'asc' | 'desc' | null };

// 当日口径描述（桌面表格行与窄屏卡片共用同一语义，避免两处口径漂移）：
// 当日列仅在 QDII 海外交易中隐藏（—）；其余时段（含开盘前/周末/休盘=closed）均展示：
// 有上一次净值实际→上一次净值（「上次」/ 盘后当日确认则「实际」），盘中→当日估算。
// 「估算收益」列维持原行为：休市 / 海外交易中隐藏（—），盘中 / 盘后展示估算口径。
function describeDay(p: PositionRow, marketSession: MarketSession) {
  const hideDay = p.delayNote === 'T+1·海外交易中';
  const hideEst = marketSession === 'closed' || p.delayNote === 'T+1·海外交易中';
  const useActual = p.hasDayActual;
  // 净值日期标签（全平台）：去掉年份只留月日（如 0825），避免与当年混淆且更紧凑；
  // 完整日期（含年）在悬停 title 呈现。
  const dayTag = useActual
    ? p.dayIsToday
      ? '实际'
      : p.navDate && p.navDate.length >= 10 && p.navDate[4] === '-' && p.navDate[7] === '-'
        ? p.navDate.slice(5).replace('-', '')
        : p.navDate
          ? p.navDate.replace(/-/g, '')
          : '上次'
    : '估算';
  const dayTagCls = useActual
    ? 'text-success border-success/40 bg-success/10'
    : 'text-primary border-primary/40 bg-primary/10';
  return { hideDay, hideEst, useActual, dayTag, dayTagCls };
}

function loadSort(): SortState {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (raw) {
      const p = JSON.parse(raw) as Partial<SortState>;
      if (
        typeof p.key === 'string' &&
        (ALLOWED as string[]).includes(p.key) &&
        (p.dir === 'asc' || p.dir === 'desc' || p.dir === null)
      ) {
        return { key: p.key as SortKey, dir: p.dir };
      }
    }
  } catch {
    /* 忽略损坏的本地存储 */
  }
  return { key: null, dir: null };
}

function sortValue(p: PositionRow, key: SortKey): number {
  // 「当日」列排序必须按单元格实际展示口径：有真实官方口径（实际/上次）按 dayPnlPctAct，
  // 否则按 dayPnlPctEst——否则出现"列上显示实际涨跌、排序却按估算涨跌"的错位。
  if (key === 'estChangePct') {
    return p.hasDayActual ? p.dayPnlPctAct : p.dayPnlPctEst;
  }
  return p[key];
}

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

// 策略信号小徽标（A 股语义：买入=涨红 gain / 卖出=跌绿 loss / 观望=muted）
function SignalTag({ sig }: { sig: GridTodayBadge }) {
  const a = sig.action === 'buy' ? 'buy' : sig.action === 'sell' ? 'sell' : 'hold';
  const color = a === 'buy' ? 'var(--color-gain)' : a === 'sell' ? 'var(--color-loss)' : 'var(--color-muted)';
  const bg = a === 'buy' ? 'var(--color-gain-subtle)' : a === 'sell' ? 'var(--color-loss-subtle)' : 'transparent';
  const Icon = a === 'buy' ? TrendingUp : a === 'sell' ? TrendingDown : Minus;
  return (
    <span
      className="tnum inline-flex items-center gap-1 rounded-pill px-1.5 py-0.5 text-xs"
      style={{ color, background: bg }}
    >
      <Icon size={13} strokeWidth={2.2} aria-hidden />
      {sig.signalName ?? (a === 'buy' ? '买入' : a === 'sell' ? '卖出' : '观望')}
    </span>
  );
}

function SortableHeader({
  label,
  k,
  sortKey,
  sortDir,
  onSort,
  badge,
}: {
  label: string;
  k: SortKey;
  sortKey: SortKey | null;
  sortDir: 'asc' | 'desc' | null;
  onSort: (k: SortKey) => void;
  badge?: ReactNode;
}) {
  const active = sortKey === k;
  return (
    <th
      scope="col"
      aria-sort={active ? (sortDir === 'asc' ? 'ascending' : 'descending') : 'none'}
      onClick={() => onSort(k)}
      className="py-2 pr-3 font-medium text-right cursor-pointer select-none whitespace-nowrap hover:text-foreground"
      title={`点击按「${label}」排序`}
    >
      <span className="inline-flex items-center justify-end gap-1">
        {label}
        {badge}
        {!active && <ChevronsUpDown size={13} className="opacity-30" aria-hidden />}
        {active && sortDir === 'asc' && <ChevronUp size={13} className="text-primary" aria-hidden />}
        {active && sortDir === 'desc' && <ChevronDown size={13} className="text-primary" aria-hidden />}
      </span>
    </th>
  );
}

// 单行视图：memo 化，排序时 props 引用不变则跳过重渲染（关键性能修复）
const PositionRowView = memo(function PositionRowView({
  p,
  marketSession,
  sig,
  onDelete,
  mobile = false,
}: {
  p: PositionRow;
  marketSession: MarketSession;
  /** 今日策略信号徽标（按 fund_code 聚合，跨平台同一只基金显示同一信号） */
  sig?: GridTodayBadge;
  onDelete: (code: string, name: string) => void;
  /** 窄屏精简表：宽基金名列 / 当日标签下沉 / 平台并入基金列仅图标 / 隐藏估算收益(金额)等次要列 */
  mobile?: boolean;
}) {
  const { hideDay, hideEst, useActual, dayTag, dayTagCls } = describeDay(p, marketSession);
  // ---------- 窄屏行（8 列，整体小一号字）：基金(名无编号) / 当日 / 估算 / 市值 / 累计盈亏 / 平台 / 信号 / 操作 ----------
  if (mobile) {
    return (
      <tr className="border-b border-border/60 last:border-0 hover:bg-background/60">
        <td className="py-2 pr-2 align-top">
          <div className="w-[120px]">
            <Link
              to={`/fund/${p.fund.code}`}
              title={p.fund.name}
              className="block break-words font-medium leading-snug text-foreground hover:text-primary"
              style={{
                display: '-webkit-box',
                WebkitLineClamp: 2,
                WebkitBoxOrient: 'vertical',
                overflow: 'hidden',
              }}
            >
              {p.fund.name}
            </Link>
            {!p.fund.valuationApplicable && (
              <span className="mt-0.5 inline-block rounded bg-border/60 px-1 py-0.5 text-[11px] text-muted">
                模型不适用
              </span>
            )}
          </div>
        </td>
        <td className="py-2 pr-2 text-right align-top">
          {hideDay ? (
            <span className="inline-flex items-center gap-1 text-muted">
              <span>—</span>
              {p.delayNote && <DelayTag note={p.delayNote} />}
            </span>
          ) : (
            <div className="flex flex-col items-end gap-0.5">
              <GainLossBadge value={useActual ? p.dayPnlPctAct : p.dayPnlPctEst} format="pct" />
              <span
                className={`rounded border px-1 py-px text-[11px] font-normal leading-none ${dayTagCls}`}
                title={useActual && !p.dayIsToday && p.navDate ? `上一次净值 ${p.navDate}` : undefined}
              >
                {dayTag}
              </span>
            </div>
          )}
        </td>
        <td className="py-2 pr-2 text-right align-top">
          {hideEst ? (
            <span className="text-muted">—</span>
          ) : (
            <div className="flex flex-col items-end gap-0.5">
              <GainLossBadge value={p.dayPnlPctEst} format="pct" />
              {p.delayNote === 'T+1·海外净值' && <DelayTag note={p.delayNote} />}
            </div>
          )}
        </td>
        <td className="py-2 pr-2 text-right align-top tnum">
          ¥{p.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}
        </td>
        <td className="py-2 pr-2 text-right align-top">
          <GainLossBadge value={p.totalPnl} format="amount" />
        </td>
        <td className="py-2 pr-2 align-top">
          <div className="flex w-8 items-center justify-center">
            <PlatformBadge code={p.fund.platform} iconOnly />
          </div>
        </td>
        <td className="py-2 pr-2 align-top">
          {sig && sig.signalName && (
            <div className="flex w-[114px] justify-start">
              <Link to={`/strategy?focus=${p.fund.code}`} title="查看策略建议详情" className="inline-flex">
                <SignalTag sig={sig} />
              </Link>
            </div>
          )}
        </td>
        <td className="py-1.5 pr-1 text-right align-top">
          <button
            onClick={() => void onDelete(p.fund.code, p.fund.name)}
            title="删除持仓"
            className="inline-flex items-center justify-center rounded p-2 text-muted hover:bg-border/60 hover:text-danger"
          >
            <Trash2 size={16} aria-hidden />
          </button>
        </td>
      </tr>
    );
  }
  // ---------- 桌面/宽屏行：完整 11 列 ----------
  return (
    <tr key={p.fund.code} className="border-b border-border/60 last:border-0 hover:bg-background/60">
      <td className="py-2.5 pr-2">
        <Link to={`/fund/${p.fund.code}`} className="font-medium text-foreground hover:text-primary">
          {p.fund.name}
        </Link>
        <div className="text-xs text-muted tnum">{p.fund.code}</div>
        {!p.fund.valuationApplicable && (
          <span className="mt-0.5 inline-block rounded bg-border/60 px-1.5 py-0.5 text-xs text-muted">模型不适用</span>
        )}
      </td>
      <td className="py-2.5 pr-2"><PlatformBadge code={p.fund.platform} /></td>
      <td className="py-1.5 pr-2 text-right">
        {hideDay ? (
          <span className="inline-flex items-center gap-1 text-muted">
            <span>—</span>
            {p.delayNote && <DelayTag note={p.delayNote} />}
          </span>
        ) : (
          <span className="inline-flex items-center gap-1.5">
            <GainLossBadge value={useActual ? p.dayPnlPctAct : p.dayPnlPctEst} format="pct" />
            <span
              className={`rounded border px-1 py-0.5 text-xs font-normal ${dayTagCls}`}
              title={useActual && !p.dayIsToday && p.navDate ? `上一次净值 ${p.navDate}` : undefined}
            >
              {dayTag}
            </span>
            {p.delayNote === 'T+1·海外净值' && <DelayTag note={p.delayNote} />}
          </span>
        )}
      </td>
      <td className="py-1.5 pr-2 text-right">
        {hideEst ? (
          <span className="inline-flex items-center gap-1 text-muted">
            <span>—</span>
            {p.delayNote && <DelayTag note={p.delayNote} />}
          </span>
        ) : (
          <GainLossBadge value={p.dayPnlEst} format="amount" />
        )}
      </td>
      <td className="py-1.5 pr-2 text-right">
        {hideEst ? (
          <span className="inline-flex items-center gap-1 text-muted">
            <span>—</span>
            {p.delayNote && <DelayTag note={p.delayNote} />}
          </span>
        ) : (
          <GainLossBadge value={p.dayPnlPctEst} format="pct" />
        )}
      </td>
      <td className="py-1.5 pr-2 text-right tnum">¥{p.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
      <td className="py-1.5 pr-2 text-right">
        <GainLossBadge value={p.totalPnl} format="amount" />
      </td>
      <td className="py-1.5 pr-2 text-right">
        <GainLossBadge value={p.totalPnlPct} format="pct" />
      </td>
      <td className="py-1.5 pr-2 text-right">
        <div className="text-xs text-muted">
          {p.valuationMethod === 'index'
            ? '指数实时'
            : p.estimated
              ? `${(p.disclosedWeightSum * 100).toFixed(0)}%`
              : '—'}
        </div>
      </td>
      <td className="py-1.5 pr-2 text-center">
        {sig && sig.signalName ? (
          <Link to={`/strategy?focus=${p.fund.code}`} title="查看策略建议详情" className="inline-flex">
            <SignalTag sig={sig} />
          </Link>
        ) : null}
      </td>
      <td className="py-1.5 pr-2 text-right">
        <button
          onClick={() => void onDelete(p.fund.code, p.fund.name)}
          title="删除持仓"
          className="inline-flex items-center justify-center rounded p-2 text-muted hover:bg-border/60 hover:text-danger"
        >
          <Trash2 size={16} aria-hidden />
        </button>
      </td>
    </tr>
  );
});

export default function PositionTable({
  positions,
  marketSession,
  signals,
  onDelete,
}: {
  positions: PositionRow[];
  marketSession: MarketSession;
  /** 今日策略信号（fund_code → 徽标），由总览页轻读 grid_today_signals 注入 */
  signals?: Record<string, GridTodayBadge>;
  onDelete: (code: string, name: string) => void;
}) {
  const narrow = useNarrow();
  const [sort, setSort] = useState<SortState>(loadSort);

  useEffect(() => {
    try {
      localStorage.setItem(LS_KEY, JSON.stringify(sort));
    } catch {
      /* 隐私模式等场景下静默跳过 */
    }
  }, [sort]);

  // 合并为单一 state + 函数式更新，避免两次 setState 产生的中间态；useCallback 稳定引用
  const toggleSort = useCallback((k: SortKey) => {
    setSort((s) => {
      if (s.key !== k) return { key: k, dir: 'asc' };
      if (s.dir === 'asc') return { key: k, dir: 'desc' };
      if (s.dir === 'desc') return { key: null, dir: null };
      return { key: k, dir: 'asc' };
    });
  }, []);

  const sortedPositions = useMemo(() => {
    const { key, dir } = sort;
    if (!key || !dir) return positions;
    const sign = dir === 'asc' ? 1 : -1;
    return [...positions].sort((a, b) => {
      const av = sortValue(a, key);
      const bv = sortValue(b, key);
      if (Number.isNaN(av) && Number.isNaN(bv)) return 0;
      if (Number.isNaN(av)) return 1;
      if (Number.isNaN(bv)) return -1;
      return (av - bv) * sign;
    });
  }, [positions, sort]);

  // 窄屏精简表 + 桌面全列表两套 thead/行（共用排序状态与 localStorage）
  const desktopHead = (
    <thead>
      <tr className="text-left text-xs text-muted border-b border-border">
        <th className="py-1.5 pr-2 font-medium">基金</th>
        <th className="py-2 pr-3 font-medium">平台</th>
        <SortableHeader
          label="当日"
          k="estChangePct"
          sortKey={sort.key}
          sortDir={sort.dir}
          onSort={toggleSort}
        />
        <SortableHeader label="估算收益" k="dayPnlEst" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="估算收益率" k="dayPnlPctEst" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="市值" k="marketValue" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="累计盈亏" k="totalPnl" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="累计盈亏率" k="totalPnlPct" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <th className="py-2 pr-3 font-medium text-right">估值</th>
        <th className="py-2 pr-3 font-medium text-center" title="今日策略信号（在策略信号页启用基金并计算后展示）">
          信号
        </th>
        <th className="py-2 pr-3 font-medium text-right">操作</th>
      </tr>
    </thead>
  );

  const desktopRows = sortedPositions.map((p) => (
    <PositionRowView
      key={`${p.fund.code}:${p.fund.platform}`}
      p={p}
      marketSession={marketSession}
      sig={signals?.[p.fund.code]}
      onDelete={onDelete}
    />
  ));

  const mobileHead = (
    <thead>
      <tr className="text-left text-[12px] text-muted border-b border-border">
        <th className="py-2 pr-2 font-medium">基金</th>
        <SortableHeader label="当日" k="estChangePct" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="估算" k="dayPnlPctEst" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="市值" k="marketValue" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <SortableHeader label="累计盈亏" k="totalPnl" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
        <th className="py-2 pr-2 font-medium" title="购买平台">平台</th>
        <th className="py-2 pr-2 font-medium text-center" title="今日策略信号（在策略信号页启用基金并计算后展示）">
          信号
        </th>
        <th className="py-2 pr-1 font-medium text-right">操作</th>
      </tr>
    </thead>
  );

  const mobileRows = sortedPositions.map((p) => (
    <PositionRowView
      key={`${p.fund.code}:${p.fund.platform}`}
      p={p}
      marketSession={marketSession}
      sig={signals?.[p.fund.code]}
      onDelete={onDelete}
      mobile
    />
  ));

  return (
    <Card title={`持仓明细（${positions.length}）`}>
      {narrow ? (
        <div className="overflow-x-auto">
          {/* 窄屏精简表：整体小一号字；基金名列用「内层定宽容器(120px)」压住列宽(截断文本不再撑宽)，
              信号列容器 114px(≈原 +50)；当日口径标签下沉到百分比下方；估算收益率简写「估算」 */}
          <table className="w-full text-[13px]">
            {mobileHead}
            <tbody>{mobileRows}</tbody>
          </table>
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm min-w-[680px]">
            {desktopHead}
            <tbody>{desktopRows}</tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
