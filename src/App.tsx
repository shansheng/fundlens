// FundLens 应用外壳 + 路由（HashRouter，适配 Tauri 文件协议）
import { lazy, Suspense } from 'react';
import { NavLink, Route, Routes } from 'react-router-dom';
import { createContext, useContext, useState } from 'react';
import { LayoutDashboard, ScanLine, LineChart, PieChart, Info, CalendarDays, Briefcase, Activity, Receipt, Sun, Moon, Monitor } from 'lucide-react';
import { PLATFORMS } from './lib/mockData';
import { useTheme, type ThemeMode } from './theme';

// 路由级代码分割：各页面改为按需懒加载（React.lazy），首屏只加载外壳 + 持仓总览，
// 其余页面（截图导入/记账/统计/周报/关于）在进入对应路由时才拉取对应 chunk，
// 配合 vite.config 的 manualChunks 进一步将 react / recharts / lucide 拆为独立 vendor 包。
const OverviewPage = lazy(() => import('./pages/OverviewPage'));
const ImportPage = lazy(() => import('./pages/ImportPage'));
const FundDetailPage = lazy(() => import('./pages/FundDetailPage'));
const StatsPage = lazy(() => import('./pages/StatsPage'));
const ReportsPage = lazy(() => import('./pages/ReportsPage'));
const LedgerPage = lazy(() => import('./pages/LedgerPage'));
const AboutPage = lazy(() => import('./pages/AboutPage'));

// 平台上下文：全局共享「当前选中的持仓平台」（null = 全部平台聚合）。
// 单机单账户，按本人持仓的不同平台（支付宝/京东金融/腾讯理财通）分组统计。
interface PlatformCtx {
  platform: string | null;
  setPlatform: (p: string | null) => void;
}
const Ctx = createContext<PlatformCtx>({ platform: null, setPlatform: () => {} });
export const usePlatform = () => useContext(Ctx);

const NAV = [
  { to: '/overview', label: '持仓总览', icon: LayoutDashboard },
  { to: '/import', label: '截图导入', icon: ScanLine },
  { to: '/ledger', label: '记账', icon: Receipt },
  { to: '/stats', label: '收益统计', icon: PieChart },
  { to: '/reports', label: '周报月报', icon: CalendarDays },
  { to: '/about', label: '关于', icon: Info },
];

// 懒加载占位：页面 chunk 未就绪时展示轻量骨架，避免白屏。
function PageLoading() {
  return (
    <div className="flex items-center justify-center h-full min-h-[60vh] text-muted">
      <div className="flex flex-col items-center gap-3">
        <span className="h-6 w-6 rounded-full border-2 border-border border-t-primary animate-spin" aria-hidden />
        <span className="text-sm">页面加载中…</span>
      </div>
    </div>
  );
}

// 外观切换：浅色 / 深色 / 跟随系统。选择写入 localStorage（src/theme.tsx），重启后记忆。
function ThemeToggle() {
  const { mode, setMode } = useTheme();
  const opts: { value: ThemeMode; label: string; icon: typeof Sun }[] = [
    { value: 'light', label: '浅色', icon: Sun },
    { value: 'dark', label: '深色', icon: Moon },
    { value: 'system', label: '跟随', icon: Monitor },
  ];
  return (
    <div
      role="group"
      aria-label="外观主题"
      className="flex items-center gap-0.5 rounded-md border border-border bg-background p-0.5"
    >
      {opts.map(({ value, label, icon: Icon }) => {
        const active = mode === value;
        return (
          <button
            key={value}
            type="button"
            onClick={() => setMode(value)}
            aria-pressed={active}
            title={`${label}模式`}
            className={`flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors duration-150 ${
              active ? 'bg-primary text-on-primary' : 'text-muted hover:bg-surface'
            }`}
          >
            <Icon size={14} aria-hidden />
            <span>{label}</span>
          </button>
        );
      })}
    </div>
  );
}

export default function App() {
  const [platform, setPlatform] = useState<string | null>(null);

  return (
    <Ctx.Provider value={{ platform, setPlatform }}>
      <div className="flex min-h-screen bg-background text-foreground">
        {/* 左侧导航 */}
        <aside className="w-56 shrink-0 border-r border-border bg-surface flex flex-col">
          <div className="flex items-center gap-2 px-4 h-14 border-b border-border">
            <Activity size={20} strokeWidth={2.2} className="text-primary" aria-hidden />
            <span className="font-semibold text-base">FundLens</span>
          </div>

          {/* 平台筛选器（按本人持仓的不同平台分组统计） */}
          <div className="px-3 py-3 border-b border-border">
            <label className="flex items-center gap-1.5 text-xs text-muted mb-1.5">
              <Briefcase size={13} aria-hidden />
              平台
            </label>
            <select
              value={platform === null ? 'all' : platform}
              onChange={(e) => setPlatform(e.target.value === 'all' ? null : e.target.value)}
              className="w-full rounded-md border border-border bg-background px-2 py-1.5 text-sm focus:outline-none focus:ring-1 focus:ring-primary"
            >
              <option value="all">全部平台</option>
              {Object.values(PLATFORMS).map((p) => (
                <option key={p.code} value={p.code}>
                  {p.name}
                </option>
              ))}
            </select>
          </div>

          <nav className="flex-1 p-2 space-y-1">
            {NAV.map(({ to, label, icon: Icon }) => (
              <NavLink
                key={to}
                to={to}
                className={({ isActive }) =>
                  `flex items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors duration-150 ${
                    isActive ? 'bg-primary text-on-primary' : 'text-muted hover:bg-background'
                  }`
                }
              >
                <Icon size={18} strokeWidth={2} aria-hidden />
                {label}
              </NavLink>
            ))}
          </nav>
          <div className="px-4 py-3 border-t border-border space-y-3">
            <div>
              <label className="block text-xs text-muted mb-1.5">外观</label>
              <ThemeToggle />
            </div>
            <div className="flex items-center gap-1.5 text-xs text-muted">
              <LineChart size={14} aria-hidden />
              本地自算 · 红涨绿跌
            </div>
          </div>
        </aside>

        {/* 主内容 */}
        <main className="flex-1 min-w-0 overflow-y-auto">
          <Suspense fallback={<PageLoading />}>
            <Routes>
              <Route path="/" element={<OverviewPage />} />
              <Route path="/overview" element={<OverviewPage />} />
              <Route path="/import" element={<ImportPage />} />
              <Route path="/ledger" element={<LedgerPage />} />
              <Route path="/fund/:code" element={<FundDetailPage />} />
              <Route path="/stats" element={<StatsPage />} />
              <Route path="/reports" element={<ReportsPage />} />
              <Route path="/about" element={<AboutPage />} />
              <Route path="*" element={<OverviewPage />} />
            </Routes>
          </Suspense>
        </main>
      </div>
    </Ctx.Provider>
  );
}
