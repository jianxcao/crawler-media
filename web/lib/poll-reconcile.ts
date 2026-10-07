/**
 * 轮询快照的引用复用工具。
 *
 * 背景：媒体库页面靠周期轮询保持新鲜（扫描时 3 秒一轮），每轮响应都是全新
 * 反序列化的对象。即使内容一字未变，直接 setState 也会换掉引用，导致整面
 * 海报墙（可能几百个格子）重新渲染一遍——这是轮询期间周期性卡顿的主因之一。
 *
 * 这里在写入 state 前与上一份快照做**内容比对**：
 *   - 整体没变 → 返回旧引用，React 跳过本次更新；
 *   - 部分变了 → 逐条复用没变的条目对象，配合列表项的 React.memo，
 *     只有真正变化的格子会重新渲染。
 *
 * 比对用 JSON.stringify：轮询数据本就来自 JSON 反序列化（无函数/undefined/
 * 循环引用），序列化比对是准确的；几百条媒体数据的开销在毫秒级，远小于
 * 一次整墙 React 重渲染。
 */

/** 内容相等时返回旧引用（配合 setState 使用可跳过重渲染），否则返回新值。 */
export function keepIfEqual<T>(prev: T, next: T): T {
  if (prev === next) return next;
  return JSON.stringify(prev) === JSON.stringify(next) ? prev : next;
}

/**
 * 按主键逐条复用列表：内容没变的条目沿用旧对象引用；全部没变（含顺序）时
 * 直接返回旧数组。用于大列表（海报墙），让 memo 化的格子逐条跳过渲染。
 */
export function reconcileList<T>(
  prev: readonly T[],
  next: T[],
  keyOf: (item: T) => string | number,
): T[] {
  if ((prev as unknown) === (next as unknown)) return next;
  const prevByKey = new Map<string | number, T>();
  for (const item of prev) prevByKey.set(keyOf(item), item);
  let unchanged = prev.length === next.length;
  const merged = next.map((item, index) => {
    const old = prevByKey.get(keyOf(item));
    if (old !== undefined && JSON.stringify(old) === JSON.stringify(item)) {
      if (prev[index] !== old) unchanged = false; // 内容同但位置变了（重新排序）
      return old;
    }
    unchanged = false;
    return item;
  });
  return unchanged ? (prev as T[]) : merged;
}
