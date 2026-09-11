import { useCallback, useEffect, useRef, useState } from 'react';
import {
  disclosureFetchCancel,
  disclosureFetchProgress,
  disclosureFetchStart,
  type DisclosureFetchProgress,
} from '../api';

/**
 * 批量披露抓取后台任务状态机：
 * start 启动（后端立即返回，抓取在 Rust 后台线程执行，不阻塞 UI）
 * → 0.8s 轮询进度 → running=false 时停止轮询并回调 onFinished。
 * unmount 时自动停止轮询（后台任务继续跑，进度不丢）。
 */
export function useDisclosureFetch(onFinished?: (p: DisclosureFetchProgress) => void | Promise<void>) {
  const [progress, setProgress] = useState<DisclosureFetchProgress | null>(null);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const finishedCbRef = useRef(onFinished);
  finishedCbRef.current = onFinished;

  const stopPolling = useCallback(() => {
    if (timerRef.current !== null) {
      clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const tick = useCallback(async () => {
    try {
      const p = await disclosureFetchProgress();
      setProgress(p);
      if (!p.running) {
        stopPolling();
        void finishedCbRef.current?.(p);
      }
    } catch {
      // 单次轮询失败（瞬态）：保留上次进度，下一轮重试
    }
  }, [stopPolling]);

  /** 启动抓取（幂等：后端已在跑则直接接续轮询）。错误抛给调用方展示。 */
  const start = useCallback(async () => {
    if (timerRef.current !== null) return; // 已在轮询中
    const p = await disclosureFetchStart();
    setProgress(p);
    if (p.running) {
      timerRef.current = setInterval(() => { void tick(); }, 800);
    } else {
      // 秒完成（如本地无基金）或接续了一个刚结束的任务：直接走完成回调
      void finishedCbRef.current?.(p);
    }
  }, [tick]);

  /** 请求取消（协作式：当前这只跑完即停）。 */
  const cancel = useCallback(async () => {
    try {
      await disclosureFetchCancel();
    } catch {
      // 取消失败不影响轮询，任务正常结束路径仍会回调
    }
  }, []);

  useEffect(() => () => stopPolling(), [stopPolling]);

  return { progress, running: progress?.running ?? false, start, cancel };
}
