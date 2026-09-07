// 图表取色共享工具 — 从设计令牌（CSS 变量）读取颜色，避免源码硬编码 hex（P0 合规）。
// recharts / 内联 SVG 的 stroke/fill 等属性对 var() 支持不稳定，统一在渲染期解析为
// rgb() 字符串传入。主题切换时，ThemeProvider 会在渲染期同步更新 <html data-theme>，
// 因此「每次渲染都调用本函数」即可天然拿到当前主题的正确颜色（配合 useTheme 订阅触发重渲染）。
export function readColorVar(name: string): string {
  if (typeof window === 'undefined') return 'currentColor';
  const raw = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return raw ? (raw.startsWith('#') || raw.startsWith('rgb') ? raw : `rgb(${raw})`) : 'currentColor';
}

// 给 readColorVar 取回的 rgb()/hex 附上 alpha → rgba()，用于日历热力图等「按强度着色」的叠加层。
export function withAlpha(color: string, alpha: number): string {
  const a = Math.max(0, Math.min(1, alpha));
  const m = color.match(/rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/);
  if (m) return `rgba(${m[1]}, ${m[2]}, ${m[3]}, ${a})`;
  if (color.startsWith('#') && color.length === 7) {
    const r = parseInt(color.slice(1, 3), 16);
    const g = parseInt(color.slice(3, 5), 16);
    const b = parseInt(color.slice(5, 7), 16);
    return `rgba(${r}, ${g}, ${b}, ${a})`;
  }
  return color;
}
