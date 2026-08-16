// FundLens 应用外壳 + 路由（HashRouter，适配 Tauri 文件协议）
import { NavLink, Route, Routes } from 'react-router-dom';
import { createContext, useContext, useState } from 'react';
import { LayoutDashboard, ScanLine, LineChart, PieChart, Info, CalendarDays, Briefcase, Activity, Receipt } from 'lucide-react';
import OverviewPage from './pages/OverviewPage';
import ImportPage from './pages/ImportPage';
import FundDetailPage from './pages/FundDetailPage';
import StatsPage from './pages/StatsPage';
import ReportsPage from './pages/ReportsPage';
import LedgerPage from './pages/LedgerPage';
import AboutPage from './pages/AboutPage';
import { PLATFORMS } from './lib/mockData';

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
          <div className="px-4 py-3 border-t border-border text-xs text-muted">
            <div className="flex items-center gap-1.5">
              <LineChart size={14} aria-hidden />
              本地自算 · 红涨绿跌
            </div>
          </div>
        </aside>

        {/* 主内容 */}
        <main className="flex-1 min-w-0 overflow-y-auto">
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
        </main>
      </div>
    </Ctx.Provider>
  );
}
