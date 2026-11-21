"use client";

import { useEffect, useRef, useState } from "react";

import { ChevronRightIcon } from "@/components/icons";

/**
 * 电影、分集与在线详情共用的四行简介。只有真实发生溢出时才出现展开入口；
 * 文案切换时先恢复折叠，再按新文案重新测量。
 */
export function ExpandablePlot({
  text,
  selectable = false,
}: {
  text: string;
  selectable?: boolean;
}) {
  const paragraphRef = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [hasOverflow, setHasOverflow] = useState(false);

  useEffect(() => {
    setExpanded(false);
    setHasOverflow(false);
  }, [text]);

  useEffect(() => {
    if (expanded) return;
    const paragraph = paragraphRef.current;
    if (!paragraph) return;

    const measure = () => {
      setHasOverflow(paragraph.scrollHeight > paragraph.clientHeight + 1);
    };
    const frame = window.requestAnimationFrame(measure);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(paragraph);
    return () => {
      window.cancelAnimationFrame(frame);
      observer?.disconnect();
    };
  }, [expanded, text]);

  return (
    <div className="max-w-3xl">
      <p
        ref={paragraphRef}
        className={`${selectable ? "selectable " : ""}text-on-image text-body-lg leading-7 text-white/78 ${
          expanded ? "" : "line-clamp-4"
        }`}
      >
        {text}
      </p>
      {(hasOverflow || expanded) && (
        <button
          type="button"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
          className="mt-1.5 inline-flex items-center gap-1 text-sub font-medium text-white/55 transition hover:text-white/85 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-2)]"
        >
          {expanded ? "收起" : "展开全文"}
          <ChevronRightIcon
            className={`size-3.5 transition-transform ${expanded ? "-rotate-90" : "rotate-90"}`}
          />
        </button>
      )}
    </div>
  );
}

/** 外部信息源链接：固定在正文底部，新窗口打开站点词条。 */
export function SourceLink({ href, label }: { href: string; label: string }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="text-caption text-[var(--text-muted)] underline decoration-white/20 underline-offset-2 transition hover:text-white/80"
    >
      {label} ↗
    </a>
  );
}
