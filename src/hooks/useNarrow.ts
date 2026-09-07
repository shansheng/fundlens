import { useEffect, useState } from 'react';

/**
 * 窄屏（手机竖屏）检测：`(max-width: 767px)`，即 <md（与 Tailwind md 断点对齐）。
 * 供「手机精简表+汇总条 vs 平板/桌面原布局」双路径条件渲染使用：
 * - 折叠屏展开态(~950px)与桌面(≥768)均回落 false → 走「原来的布局」全列表+四格。
 * - jsdom（无 matchMedia）回落 false → 桌面路径，保证测试与桌面零回归。
 * - 折叠屏展开/折叠热切换时实时更新（监听 change 事件）。
 */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState<boolean>(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
    return window.matchMedia('(max-width: 767px)').matches;
  });
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return undefined;
    const mq = window.matchMedia('(max-width: 767px)');
    const onChange = (e: MediaQueryListEvent) => setNarrow(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return narrow;
}
