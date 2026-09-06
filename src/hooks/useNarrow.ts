import { useEffect, useState } from 'react';

/**
 * 窄屏（移动端）检测：`(max-width: 1023px)`，即 <lg（与 Tailwind lg 断点对齐）。
 * 供「窄屏卡片化布局 vs 桌面表格布局」双路径条件渲染使用。
 * - 桌面(≥1024) 与 jsdom（无 matchMedia）回落 false → 桌面路径，保证测试与桌面零回归。
 * - 折叠屏展开/折叠热切换时实时更新（监听 change 事件）。
 */
export function useNarrow(): boolean {
  const [narrow, setNarrow] = useState<boolean>(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
    return window.matchMedia('(max-width: 1023px)').matches;
  });
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return undefined;
    const mq = window.matchMedia('(max-width: 1023px)');
    const onChange = (e: MediaQueryListEvent) => setNarrow(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return narrow;
}
