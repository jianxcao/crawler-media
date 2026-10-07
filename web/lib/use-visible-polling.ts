"use client";

import { useEffect, useRef } from "react";

/**
 * 页面可见时才执行的周期轮询（system-logs-section 的写法抽成通用 hook）。
 *
 * 全站各处的状态轮询（下载器/LLM/站点验证、扫描进度、媒体库刷新等）此前
 * 都是裸 setInterval：标签页切到后台仍按原频率发请求 + 重渲染组件树，
 * 既费电费流量，也让后端白白挨打。这里统一为：
 *   - 页面隐藏时跳过本 tick（定时器保留，省去重建逻辑）；
 *   - 重新可见时立即补一次，不让用户干等下个周期。
 *
 * @param callback 轮询体；引用变化不重建定时器（内部走 ref）
 * @param intervalMs 轮询间隔；null/0 表示停止轮询
 * @param leading 挂载（或 intervalMs 变化）时是否立即先执行一次，默认 false
 */
export function useVisiblePolling(
  callback: () => void,
  intervalMs: number | null,
  { leading = false }: { leading?: boolean } = {},
) {
  const callbackRef = useRef(callback);
  callbackRef.current = callback;

  useEffect(() => {
    if (!intervalMs) return;
    const tick = () => {
      if (!document.hidden) callbackRef.current();
    };
    if (leading) tick();
    const timer = setInterval(tick, intervalMs);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [intervalMs, leading]);
}
