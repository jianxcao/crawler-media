import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { RefreshIcon } from "@/components/icons";

interface VirtualTransferListProps<T> {
  items: T[];
  getKey: (item: T, index: number) => string;
  defaultItemHeight: number;
  scrollElement?: HTMLElement | null;
  overscan?: number;
  gap?: number;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
  renderItem: (item: T, index: number) => ReactNode;
}

const useIsomorphicLayoutEffect =
  typeof window !== "undefined" ? useLayoutEffect : useEffect;

export function VirtualTransferList<T>({
  items,
  getKey,
  defaultItemHeight,
  scrollElement,
  overscan = 8,
  gap = 12,
  hasMore,
  loadingMore,
  onLoadMore,
  renderItem,
}: VirtualTransferListProps<T>) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [fallbackScrollEl, setFallbackScrollEl] = useState<HTMLElement | null>(null);
  const [scrollMargin, setScrollMargin] = useState(0);

  const activeScrollEl = scrollElement ?? fallbackScrollEl;

  useIsomorphicLayoutEffect(() => {
    if (scrollElement) return;
    const container = containerRef.current;
    if (!container || typeof container.closest !== "function") return;
    const detected = container.closest<HTMLElement>("[data-scroll-root]");
    if (detected && detected !== fallbackScrollEl) {
      setFallbackScrollEl(detected);
    }
  }, [scrollElement, fallbackScrollEl]);

  useIsomorphicLayoutEffect(() => {
    const container = containerRef.current;
    const scroller = activeScrollEl;
    if (!container || !scroller) return;

    const updateMargin = () => {
      if (
        typeof container.getBoundingClientRect !== "function" ||
        typeof scroller.getBoundingClientRect !== "function"
      ) {
        return;
      }
      const listRect = container.getBoundingClientRect();
      const rootRect = scroller.getBoundingClientRect();
      const next = Math.max(0, Math.round(listRect.top - rootRect.top + (scroller.scrollTop || 0)));
      setScrollMargin((prev) => (Math.abs(prev - next) >= 2 ? next : prev));
    };

    updateMargin();
    if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
      window.addEventListener("resize", updateMargin, { passive: true });
      return () => window.removeEventListener("resize", updateMargin);
    }
    return undefined;
  }, [activeScrollEl, items.length]);

  const rowVirtualizer = useVirtualizer({
    count: items.length,
    getScrollElement: () => activeScrollEl,
    estimateSize: () => defaultItemHeight,
    getItemKey: (index) => {
      const item = items[index];
      return item !== undefined ? getKey(item, index) : String(index);
    },
    overscan,
    gap,
    scrollMargin,
    initialRect: { width: 1024, height: 840 },
  });

  const virtualItems = rowVirtualizer.getVirtualItems();
  const lastVirtualIndex = virtualItems.length > 0 ? virtualItems[virtualItems.length - 1].index : -1;

  useEffect(() => {
    if (!hasMore || loadingMore || items.length === 0) return;
    if (lastVirtualIndex >= items.length - 10) {
      onLoadMore();
    }
  }, [hasMore, loadingMore, items.length, lastVirtualIndex, onLoadMore]);

  const totalSize = rowVirtualizer.getTotalSize();

  return (
    <div ref={containerRef} className="mt-4">
      <div
        style={{
          height: `${totalSize}px`,
          width: "100%",
          position: "relative",
        }}
      >
        {virtualItems.map((virtualRow) => {
          const item = items[virtualRow.index];
          if (item === undefined) return null;
          const offsetY = virtualRow.start - rowVirtualizer.options.scrollMargin;
          return (
            <div
              key={virtualRow.key}
              data-index={virtualRow.index}
              data-transfer-row={virtualRow.index}
              ref={(el) => {
                if (el && typeof el.getAttribute === "function") {
                  rowVirtualizer.measureElement(el);
                }
              }}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${offsetY}px)`,
              }}
            >
              {renderItem(item, virtualRow.index)}
            </div>
          );
        })}
      </div>

      {loadingMore && (
        <div className="flex items-center justify-center gap-2 py-5 text-xs text-[var(--text-muted)]">
          <RefreshIcon className="size-3.5 animate-spin text-sky-400" />
          <span>正在加载更多记录...</span>
        </div>
      )}

      {!loadingMore && hasMore && (
        <div className="flex justify-center py-4">
          <button
            type="button"
            onClick={onLoadMore}
            className="rounded-xl border border-white/[0.08] bg-white/[0.04] px-4 py-2 text-xs font-medium text-white/75 transition hover:border-white/[0.16] hover:bg-white/[0.08] hover:text-white"
          >
            加载更多
          </button>
        </div>
      )}
    </div>
  );
}
