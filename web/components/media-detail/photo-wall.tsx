"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { ChevronLeftIcon, ChevronRightIcon, PhotoIcon } from "@/components/icons";
import { ImageLightbox, type LightboxAction } from "@/components/image-lightbox";
import { PosterImage } from "@/components/poster-image";
import type { MediaImage } from "@/lib/api/discover";
import { useBackdrop } from "@/lib/backdrop";
import { useTapGuard } from "@/lib/use-tap-guard";

function PhotoArrow({
  dir,
  visible,
  onClick,
}: {
  dir: -1 | 1;
  visible: boolean;
  onClick: () => void;
}) {
  const Icon = dir === -1 ? ChevronLeftIcon : ChevronRightIcon;
  return (
    <button
      type="button"
      aria-label={dir === -1 ? "向左滚动" : "向右滚动"}
      onClick={onClick}
      className={`surface-raised !absolute top-1/2 z-10 flex size-9 -translate-y-1/2 items-center justify-center !rounded-full text-[var(--text)] transition-all duration-200 hover:scale-110 [@media(hover:none)]:hidden ${
        dir === -1 ? "left-2" : "right-2"
      } ${
        visible
          ? "pointer-events-auto opacity-0 group-hover/photos:opacity-100"
          : "pointer-events-none opacity-0"
      }`}
    >
      <Icon className="size-4" />
    </button>
  );
}

function PhotoCard({
  img,
  index,
  label,
  landscape,
  title,
  onOpen,
}: {
  img: { previewUrl: string };
  index: number;
  label: string;
  landscape: boolean;
  title: string;
  onOpen: () => void;
}) {
  const tapGuard = useTapGuard(onOpen);
  return (
    <button
      type="button"
      {...tapGuard}
      aria-label={`查看${label}第 ${index + 1} 张`}
      className={`shrink-0 overflow-hidden rounded-xl bg-[#141824] ring-1 ring-white/[0.08] transition-all duration-300 ease-out hover:-translate-y-1 hover:shadow-[0_16px_40px_rgba(0,0,0,0.55)] hover:ring-white/30 ${
        landscape
          ? "aspect-video h-[148px] max-md:h-[104px]"
          : "aspect-[2/3] h-[148px] max-md:h-[126px]"
      }`}
    >
      <PosterImage
        src={img.previewUrl}
        alt={`${title} ${label}`}
        className="size-full object-cover transition-transform duration-500 ease-out hover:scale-[1.05]"
      />
    </button>
  );
}

/** 剧照与海报横滚墙：胶囊标签切换类型，点击打开全屏灯箱（剧照支持设为背景）。 */
export function PhotoWall({
  title,
  backdrops,
  posters,
}: {
  title: string;
  backdrops: MediaImage[];
  posters: MediaImage[];
}) {
  const tabs = [
    { id: "backdrops" as const, label: "剧照", images: backdrops },
    { id: "posters" as const, label: "海报", images: posters },
  ].filter((t) => t.images.length > 0);
  const [activeId, setActiveId] = useState(tabs[0].id);
  const [lightboxIndex, setLightboxIndex] = useState<number | null>(null);

  const active = tabs.find((t) => t.id === activeId) ?? tabs[0];
  const { uploadBackdrop } = useBackdrop();

  const setAsBackdrop: LightboxAction | undefined =
    active.id === "backdrops"
      ? {
          label: "设为背景",
          busyLabel: "正在下载并设置…",
          doneLabel: "已设为背景",
          icon: <PhotoIcon className="size-3.5" />,
          run: async (i: number) => {
            const image = active.images[i];
            let blob: Blob;
            try {
              const resp = await fetch(image.fullUrl, {
                cache: "no-store",
                signal: AbortSignal.timeout(15_000),
              });
              if (!resp.ok) throw new Error();
              blob = await resp.blob();
            } catch {
              throw new Error("下载剧照原图失败，请检查网络后重试");
            }
            await uploadBackdrop(
              new File([blob], "backdrop.jpg", { type: blob.type || "image/jpeg" }),
            );
          },
        }
      : undefined;

  const scrollerRef = useRef<HTMLDivElement>(null);
  const [canLeft, setCanLeft] = useState(false);
  const [canRight, setCanRight] = useState(false);
  const edgeFrame = useRef(0);

  const updateEdges = useCallback(() => {
    if (edgeFrame.current) return;
    edgeFrame.current = window.requestAnimationFrame(() => {
      edgeFrame.current = 0;
      const el = scrollerRef.current;
      if (!el) return;
      setCanLeft(el.scrollLeft > 1);
      setCanRight(el.scrollLeft + el.clientWidth < el.scrollWidth - 1);
    });
  }, []);

  useEffect(() => {
    updateEdges();
    window.addEventListener("resize", updateEdges);
    return () => {
      window.removeEventListener("resize", updateEdges);
      window.cancelAnimationFrame(edgeFrame.current);
    };
  }, [updateEdges, activeId]);

  const page = (dir: -1 | 1) => {
    const el = scrollerRef.current;
    el?.scrollBy({ left: dir * el.clientWidth * 0.85, behavior: "smooth" });
  };

  const switchTab = (nextId: typeof activeId) => {
    setActiveId(nextId);
    scrollerRef.current?.scrollTo({ left: 0 });
  };

  return (
    <section className="group/photos">
      <div className="mb-3 flex items-center gap-3">
        <h2 className="text-on-image text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
          剧照与海报
        </h2>
        {tabs.length > 1 && (
          <div className="flex gap-1.5">
            {tabs.map((tab) => (
              <button
                key={tab.id}
                type="button"
                aria-pressed={tab.id === activeId}
                onClick={() => switchTab(tab.id)}
                className={`tnum rounded-full px-3 py-1 text-sub font-medium transition-colors ${
                  tab.id === activeId
                    ? "bg-white/[0.14] text-white"
                    : "text-[var(--text-muted)] hover:bg-white/[0.07] hover:text-[var(--text)]"
                }`}
              >
                {tab.label} {tab.images.length}
              </button>
            ))}
          </div>
        )}
      </div>

      <div className="relative">
        <div
          ref={scrollerRef}
          onScroll={updateEdges}
          className="scroll-none -mx-1 flex gap-3 overflow-x-auto overscroll-x-contain px-1 pb-1 pt-1"
        >
          {active.images.map((img, i) => (
            <PhotoCard
              key={img.previewUrl}
              img={img}
              index={i}
              label={active.label}
              landscape={active.id === "backdrops"}
              title={title}
              onOpen={() => setLightboxIndex(i)}
            />
          ))}
        </div>

        <PhotoArrow dir={-1} visible={canLeft} onClick={() => page(-1)} />
        <PhotoArrow dir={1} visible={canRight} onClick={() => page(1)} />
      </div>

      {lightboxIndex !== null && (
        <ImageLightbox
          images={active.images.map((img) => img.fullUrl)}
          initialIndex={lightboxIndex}
          title={`${title} · ${active.label}`}
          action={setAsBackdrop}
          thumbAspect={active.id === "backdrops" ? "landscape" : "portrait"}
          onClose={() => setLightboxIndex(null)}
        />
      )}
    </section>
  );
}
