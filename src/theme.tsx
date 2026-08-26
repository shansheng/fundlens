// FundLens 主题管理（浅色 / 深色 / 跟随系统）
// 设计令牌已在 src/index.css 中以 [data-theme='dark'] 完整定义；
// 本文件负责：解析模式 → 将 data-theme 应用到 <html> → 持久化用户选择。
import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from 'react';

export type ThemeMode = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';

// 与 index.html 内联引导脚本保持一致，否则首屏会因 key 不一致而重置。
export const THEME_STORAGE_KEY = 'fundlens-theme';

interface ThemeContextValue {
  /** 用户设定的模式（含「跟随系统」）。 */
  mode: ThemeMode;
  /** 实际生效的主题（system 模式下由系统偏好解析而来）。 */
  theme: ResolvedTheme;
  setMode: (m: ThemeMode) => void;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

function getSystemTheme(): ResolvedTheme {
  if (typeof window === 'undefined' || !window.matchMedia) return 'light';
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

function readStoredMode(): ThemeMode {
  try {
    const v = localStorage.getItem(THEME_STORAGE_KEY);
    if (v === 'light' || v === 'dark' || v === 'system') return v;
  } catch {
    /* localStorage 不可用时回退默认 */
  }
  return 'light';
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>(() => readStoredMode());
  const [systemTheme, setSystemTheme] = useState<ResolvedTheme>(() => getSystemTheme());

  // 监听系统主题变化时同步（仅「跟随系统」模式下影响实际渲染）。
  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return;
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = (e: MediaQueryListEvent) => setSystemTheme(e.matches ? 'dark' : 'light');
    if (mq.addEventListener) mq.addEventListener('change', handler);
    else mq.addListener(handler);
    return () => {
      if (mq.removeEventListener) mq.removeEventListener('change', handler);
      else mq.removeListener(handler);
    };
  }, []);

  const theme: ResolvedTheme = mode === 'system' ? systemTheme : mode;

  // 将解析后的主题应用到 <html data-theme>，驱动 CSS 变量切换。
  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
  }, [theme]);

  const setMode = (m: ThemeMode) => {
    setModeState(m);
    try {
      localStorage.setItem(THEME_STORAGE_KEY, m);
    } catch {
      /* 忽略持久化失败 */
    }
  };

  const value = useMemo<ThemeContextValue>(() => ({ mode, theme, setMode }), [mode, theme]);
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error('useTheme 必须在 ThemeProvider 内使用');
  return ctx;
}
