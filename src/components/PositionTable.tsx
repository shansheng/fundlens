// 持仓明细表 —— 含「点击表头排序」（仅数值列：估算净值 / 当日% / 当日估算收益 /
// 市值 / 累计盈亏 / 持仓占比），单列三态循环（升序→降序→恢复默认），排序状态
// 持久化到 localStorage。排序为纯视图变换，不影响底层数据与自动刷新。
//
// 性能：行组件用 React.memo 包裹，排序仅改变行的先后顺序（props 引用不变），
// 不会重渲染每一行的 GainLossBadge/SVG，避免快速连点时主线程被打满卡死。
import { memo, useCallback, useEffect, useMemo, useState, type ReactNode } from 'react';
import { Link } from 'react-router-dom';
import { ChevronUp, ChevronDown, ChevronsUpDown, Trash2 } from 'lucide-react';
import { type PositionRow } from '../api';
import { GainLossBadge } from './GainLossBadge';
import { Card, PlatformBadge } from './ui';

type SortKey = 'estNav' | 'estChangePct' | 'dayPnlEst' | 'marketValue' | 'totalPnl' | 'holdPct';
const ALLOWED: SortKey[] = ['estNav', 'estChangePct', 'dayPnlEst', 'marketValue', 'totalPnl', 'holdPct'];
const LS_KEY = 'fundlens.overview.sort';

type SortState = { key: SortKey | null; dir: 'asc' | 'desc' | null };

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

function sortValue(p: PositionRow, key: SortKey, totalMv: number): number {
  if (key === 'holdPct') return totalMv > 0 ? p.marketValue / totalMv : 0;
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
  totalMarketValue,
  marketSession,
  onDelete,
}: {
  p: PositionRow;
  totalMarketValue: number;
  marketSession: 'intraday' | 'post_close' | 'closed';
  onDelete: (code: string, name: string) => void;
}) {
  const hideDay = marketSession === 'closed' || p.delayNote === 'T+1·海外交易中';
  return (
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
        {hideDay ? (
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
      <td className="py-1.5 pr-2 text-right">
        {hideDay ? (
          <span className="inline-flex items-center gap-1 text-muted">
            <span>—</span>
            {p.delayNote && <DelayTag note={p.delayNote} />}
          </span>
        ) : (
          <span className="inline-flex flex-col items-end gap-0.5">
            <span className="inline-flex items-center gap-1.5">
              <GainLossBadge value={p.dayPnlEst} format="amount" />
              {p.delayNote === 'T+1·海外净值' && <DelayTag note={p.delayNote} />}
            </span>
            <span className="text-xs text-muted tnum"><GainLossBadge value={p.dayPnlPctEst} format="pct" /></span>
          </span>
        )}
      </td>
      <td className="py-1.5 pr-2 text-right tnum">¥{p.marketValue.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}</td>
      <td className="py-1.5 pr-2 text-right">
        <GainLossBadge value={p.totalPnl} format="amount" />
        <div className="text-xs text-muted tnum"><GainLossBadge value={p.totalPnlPct} format="pct" /></div>
      </td>
      <td className="py-1.5 pr-2 text-right tnum">
        {totalMarketValue > 0
          ? `${((p.marketValue / totalMarketValue) * 100).toFixed(2)}%`
          : '—'}
      </td>
      <td className="py-1.5 pr-2 text-right">
        <div className="text-xs text-muted">
          {p.valuationMethod === 'index'
            ? '指数实时'
            : p.estimated
              ? `${(p.disclosedWeightSum * 100).toFixed(0)}%`
              : '—'}
        </div>
        {p.valuationSource === 'local' && (
          <div className="mt-0.5 text-[11px] tnum text-muted">本地自算</div>
        )}
      </td>
      <td className="py-1.5 pr-2 text-right">
        <button
          onClick={() => void onDelete(p.fund.code, p.fund.name)}
          title="删除持仓"
          className="inline-flex items-center justify-center rounded p-1.5 text-muted hover:bg-border/60 hover:text-danger"
        >
          <Trash2 size={16} aria-hidden />
        </button>
      </td>
    </tr>
  );
});

export default function PositionTable({
  positions,
  totalMarketValue,
  marketSession,
  daySession,
  onDelete,
}: {
  positions: PositionRow[];
  totalMarketValue: number;
  marketSession: 'intraday' | 'post_close' | 'closed';
  daySession: { label: string; cls: string };
  onDelete: (code: string, name: string) => void;
}) {
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
      const av = sortValue(a, key, totalMarketValue);
      const bv = sortValue(b, key, totalMarketValue);
      if (Number.isNaN(av) && Number.isNaN(bv)) return 0;
      if (Number.isNaN(av)) return 1;
      if (Number.isNaN(bv)) return -1;
      return (av - bv) * sign;
    });
  }, [positions, sort, totalMarketValue]);

  return (
    <Card title={`持仓明细（${positions.length}）`}>
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-left text-xs text-muted border-b border-border">
              <th className="py-1.5 pr-2 font-medium">基金</th>
              <th className="py-2 pr-3 font-medium">平台</th>
              <SortableHeader label="估算净值" k="estNav" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
              <SortableHeader
                label="当日"
                k="estChangePct"
                sortKey={sort.key}
                sortDir={sort.dir}
                onSort={toggleSort}
                badge={
                  <span className={`ml-1 inline-block rounded border px-1 py-0.5 text-xs font-normal ${daySession.cls}`}>
                    {daySession.label}
                  </span>
                }
              />
              <SortableHeader label="当日估算收益" k="dayPnlEst" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
              <SortableHeader label="市值" k="marketValue" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
              <SortableHeader label="累计盈亏" k="totalPnl" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
              <SortableHeader label="持仓占比" k="holdPct" sortKey={sort.key} sortDir={sort.dir} onSort={toggleSort} />
              <th className="py-2 pr-3 font-medium text-right">估值</th>
              <th className="py-2 pr-3 font-medium text-right">操作</th>
            </tr>
          </thead>
          <tbody>
            {sortedPositions.map((p) => (
              <PositionRowView
                key={p.fund.code}
                p={p}
                totalMarketValue={totalMarketValue}
                marketSession={marketSession}
                onDelete={onDelete}
              />
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}
