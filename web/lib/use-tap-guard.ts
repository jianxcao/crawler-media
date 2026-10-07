"use client";

import { useCallback, useRef, type PointerEvent, type MouseEvent } from "react";

/**
 * 触摸端「误触保护」：把一次原生 click 重新判定为「是不是真的点了一下」。
 *
 * 为什么需要它：浏览器只在「滚动容器真的滚起来了」时才抑制随后的 click。
 * 发现页是海报墙 + 横滚行的组合，滑动远多于点击，下面三种手势浏览器都会
 * 老老实实派发 click，于是用户一滑就进详情页：
 *   1) 手指有位移但没滚动 —— 已经到边缘，或横滑时的纵向分量被容器吃掉；
 *   2) 点一下停住惯性 —— 惯性滑行中按屏幕刹车，这是一次完整的 tap；
 *   3) 按住看海报后抬手 —— 手指微动，仍然是 click。
 *
 * 判定规则（三条全过才算点击）：
 *   - 位移 < TAP_SLOP_PX：10px 对齐 iOS 系统的 touch slop，低于它按不准，
 *     高于它正常人已经在划了；
 *   - 时长 < TAP_MAX_MS：排掉长按（想看清海报而不是想进详情）；
 *   - 按下瞬间不在滚动中：见 SCROLL_QUIET_MS。
 *
 * 只拦截 pointerType === "touch"，鼠标和键盘一律放行 —— 桌面端的点击行为
 * 完全不变（鼠标没有误触问题，键盘 Enter 更不该被位移阈值挡住）。
 */

/** 允许的最大位移（px）：超过即判为滑动，不是点击 */
const TAP_SLOP_PX = 10;
/** 允许的最大按压时长（ms）：超过即判为长按 */
const TAP_MAX_MS = 500;
/**
 * 「刚刚还在滚」的静默窗口（ms）。惯性滚动每帧都在派发 scroll 事件，
 * 因此在按下的瞬间只要距上一次 scroll 不足这个时长，就认为用户是在刹车。
 * 必须在 pointerdown 时判定：手指一按惯性立刻停，等到 pointerup 时
 * scroll 早就不发了，那会儿再看就晚了。
 */
const SCROLL_QUIET_MS = 120;

/** 全站最近一次滚动的时间戳（任意滚动容器）；懒装一个捕获阶段监听共享给所有卡片 */
let lastScrollAt = 0;
let scrollListenerAttached = false;

/**
 * scroll 事件不冒泡，但会走捕获阶段，因此在 document 上以 capture 监听
 * 就能收到页面内任意滚动容器（纵向内容区、横滚行）的滚动，无需逐个接线。
 */
function ensureScrollListener() {
  if (scrollListenerAttached || typeof document === "undefined") return;
  scrollListenerAttached = true;
  document.addEventListener(
    "scroll",
    () => {
      lastScrollAt = performance.now();
    },
    { capture: true, passive: true },
  );
}

/** 一次按压的起始状态；null 表示当前没有需要判定的触摸序列（鼠标/键盘） */
interface PressStart {
  x: number;
  y: number;
  t: number;
  /** 按下时页面正在滚动（含惯性），这一下多半是刹车而非点击 */
  duringScroll: boolean;
}

export interface TapGuardProps {
  onPointerDown: (e: PointerEvent) => void;
  onPointerUp: (e: PointerEvent) => void;
  onPointerCancel: () => void;
  onClick: (e: MouseEvent) => void;
}

/**
 * @param onTap 判定通过后要执行的动作；省略时只做拦截，
 *              交由元素的默认行为处理（如 `<Link>` 的跳转）。
 */
export function useTapGuard(onTap?: () => void): TapGuardProps {
  const start = useRef<PressStart | null>(null);
  // 本次 click 是否放行。默认 true，这样鼠标点击、键盘回车这些
  // 不经过 pointerdown 判定的路径天然放行。
  const allow = useRef(true);

  const onPointerDown = useCallback((e: PointerEvent) => {
    ensureScrollListener();
    if (e.pointerType !== "touch") {
      start.current = null;
      allow.current = true;
      return;
    }
    start.current = {
      x: e.clientX,
      y: e.clientY,
      t: performance.now(),
      duringScroll: performance.now() - lastScrollAt < SCROLL_QUIET_MS,
    };
    // 触摸序列先默认拦截，走完 pointerup 的三项判定才可能翻成放行；
    // 这样即便 pointerup 丢失（被系统手势打断），也不会漏放一次误触。
    allow.current = false;
  }, []);

  const onPointerUp = useCallback((e: PointerEvent) => {
    const s = start.current;
    start.current = null;
    if (!s) return;
    const moved = Math.hypot(e.clientX - s.x, e.clientY - s.y);
    allow.current =
      !s.duringScroll && moved < TAP_SLOP_PX && performance.now() - s.t < TAP_MAX_MS;
  }, []);

  const onPointerCancel = useCallback(() => {
    start.current = null;
    allow.current = false;
  }, []);

  const onClick = useCallback(
    (e: MouseEvent) => {
      const allowed = allow.current;
      // 判定只对本次 click 有效，用完立刻复位，避免状态泄漏到下一次交互
      allow.current = true;
      if (!allowed) {
        // preventDefault 拦掉 <Link> 的跳转，stopPropagation 拦掉外层可能的点击处理
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      onTap?.();
    },
    [onTap],
  );

  return { onPointerDown, onPointerUp, onPointerCancel, onClick };
}
