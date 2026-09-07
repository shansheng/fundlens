// 移动端文件链路助手（M2-P0「内容传参」）
// ----------------------------------------------------------------
// 背景：Tauri 2 的 tauri-plugin-dialog 在 Android 上 open/save 只返回 content:// URI，
// Rust std::fs 无法读写；而 wry 的 RustWebChromeClient 已实现 onShowFileChooser，
// 故 HTML <input type=file> 在移动端 WebView 可靠可用（走系统文件选择器）。
// 统一策略：移动端用 <input type=file> 读字节 → base64 内容传参给后端命令；
// 图片预览直接用前端 data URL（不再调用后端 read_image_data_url）。
// 桌面端维持既有 dialog 路径版，零行为变化。
// ----------------------------------------------------------------

/** 把 File 读为原始 base64（无 data: 前缀）。分块 btoa，避免大图 RangeError/栈溢出。 */
export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error('读取文件失败'));
    reader.onload = () => {
      const buf = reader.result as ArrayBuffer;
      const bytes = new Uint8Array(buf);
      const CHUNK = 0x8000; // 32KB/块，兼容旧 WebView 的 apply 参数上限
      let bin = '';
      for (let i = 0; i < bytes.length; i += CHUNK) {
        bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
      }
      // btoa 对二进制字符串安全（每字符 ≤ 0xFF）
      resolve(btoa(bin));
    };
    reader.readAsArrayBuffer(file);
  });
}

/** 把 File 读为完整 data URL（含 data:<mime>;base64, 前缀），可直接作 <img src>。 */
export function fileToDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error('读取文件失败'));
    reader.onload = () => resolve(reader.result as string);
    reader.readAsDataURL(file);
  });
}

export interface PickedImage {
  /** 原始 base64（无前缀），供后端 *_b64 命令 decode */
  b64: string;
  /** 完整 data URL，供前端 <img> 预览 */
  dataUrl: string;
  name: string;
  size: number;
}

function pickFilesViaInput(accept: string, multiple: boolean): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = accept;
    input.multiple = multiple;
    input.style.display = 'none';
    document.body.appendChild(input);
    const cleanup = () => {
      input.removeEventListener('change', onChange);
      input.removeEventListener('cancel', onCancel);
      input.remove();
    };
    const onChange = () => {
      cleanup();
      resolve(input.files ? Array.from(input.files) : []);
    };
    const onCancel = () => {
      cleanup();
      resolve([]);
    };
    input.addEventListener('change', onChange);
    input.addEventListener('cancel', onCancel);
    input.click();
  });
}

/** 移动端多选图片（截图导入用）：返回 base64 + dataUrl 双份。用户取消时返回空数组。 */
export async function pickImagesMobile(accept = 'image/*'): Promise<PickedImage[]> {
  const files = await pickFilesViaInput(accept, true);
  const picks: PickedImage[] = [];
  for (const f of files) {
    try {
      const [b64, dataUrl] = await Promise.all([fileToBase64(f), fileToDataUrl(f)]);
      picks.push({ b64, dataUrl, name: f.name, size: f.size });
    } catch {
      // 单个文件读取失败跳过，不阻塞其余
    }
  }
  return picks;
}

/** 移动端单选任意类型文件（备份恢复 .db 用）。用户取消返回 null。 */
export async function pickSingleFileMobile(accept: string): Promise<{ b64: string; dataUrl: string; name: string; size: number } | null> {
  const files = await pickFilesViaInput(accept, false);
  if (files.length === 0) return null;
  const f = files[0];
  const [b64, dataUrl] = await Promise.all([fileToBase64(f), fileToDataUrl(f)]);
  return { b64, dataUrl, name: f.name, size: f.size };
}

/** b64 → Blob 构建（移动端系统分享 / 下载用） */
export function base64ToBlob(b64: string, mime: string): Blob {
  const bin = atob(b64);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i);
  return new Blob([bytes], { type: mime });
}

/** 是否支持 Web Share API（移动端分享落地检测）。 */
function canWebShare(): boolean {
  return typeof navigator !== 'undefined' && typeof navigator.share === 'function';
}

/** 供 UI 在按钮旁如实提示分享能力（如 AboutPage 导出消息文案）。 */
export function webShareSupported(): boolean {
  return canWebShare();
}

/**
 * 移动端保存文本（报表 .md）：优先 Web Share 系统分享，退化为复制引导。
 * 返回 true 表示已走系统分享成功触发。
 */
export async function shareTextMobile(title: string, text: string): Promise<boolean> {
  if (canWebShare()) {
    try {
      await navigator.share({ title, text });
      return true;
    } catch (e) {
      // 用户取消分享（AbortError）不算失败；其余原因落入提示
      if ((e as Error).name === 'AbortError') return true;
    }
  }
  return false;
}

/** 文件分享结果：shared=系统分享已弹出 / aborted=用户取消 / downloadAttempted=已尝试下载兜底 / unsupported=两者皆不可用 */
export type ShareFileResult = 'shared' | 'aborted' | 'downloadAttempted' | 'unsupported';

/**
 * 移动端保存文件（备份导出 .db）：
 * 1) 优先 Web Share 携带文件（系统分享到文件管理/网盘/微信）；
 * 2) 分享不可用/失败 → 触发 `<a download>`（Android WebView 若无下载监听可能静默失败，
 *    调用方须按返回结果如实提示，不得谎称已保存）。
 */
export async function shareFileMobile(fileName: string, mime: string, b64: string): Promise<ShareFileResult> {
  if (canWebShare() && typeof navigator.canShare === 'function') {
    try {
      const file = new File([base64ToBlob(b64, mime)], fileName, { type: mime });
      if (navigator.canShare({ files: [file] })) {
        await navigator.share({ files: [file], title: fileName });
        return 'shared';
      }
    } catch (e) {
      if ((e as Error).name === 'AbortError') return 'aborted';
      // 其余异常（如安全策略拒绝）落入下载兜底
    }
  }
  // 兜底：触发 WebView 下载（是否真正落盘取决于平台 DownloadListener，如实标注为「已尝试」）
  try {
    const blob = base64ToBlob(b64, mime);
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = fileName;
    a.style.display = 'none';
    document.body.appendChild(a);
    a.click();
    setTimeout(() => {
      URL.revokeObjectURL(url);
      a.remove();
    }, 4000);
    return 'downloadAttempted';
  } catch {
    return 'unsupported';
  }
}
