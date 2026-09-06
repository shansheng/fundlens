import { useEffect, useState } from 'react';

/**
 * 触屏设备检测（`(pointer: coarse)`）。供图表 Tooltip trigger（click vs hover）、
 * 触控目标尺寸等使用。比 api.isMobile（仅 Tauri WebView）更宽——覆盖触屏浏览器等场景。
 */
export function useIsTouch(): boolean {
  const [isTouch, setIsTouch] = useState<boolean>(() => {
    if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return false;
    return window.matchMedia('(pointer: coarse)').matches;
  });
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return undefined;
    const mq = window.matchMedia('(pointer: coarse)');
    const onChange = (e: MediaQueryListEvent) => setIsTouch(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return isTouch;
}
