"use client";

import { useEffect, useState } from "react";

import { HScroller } from "@/components/h-scroller";
import { PlayIcon, XIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import { PosterImage } from "@/components/poster-image";
import type { MediaVideo } from "@/lib/api/discover";
import { useTapGuard } from "@/lib/use-tap-guard";

type YoutubeReach = "checking" | "ok" | "blocked";

function useYoutubeReachable(videoKey: string): YoutubeReach {
  const [reach, setReach] = useState<YoutubeReach>("checking");

  useEffect(() => {
    setReach("checking");
    const probe = new Image();
    const timer = window.setTimeout(() => setReach("blocked"), 6000);
    const settle = (result: YoutubeReach) => () => {
      window.clearTimeout(timer);
      setReach(result);
    };
    probe.onload = settle("ok");
    probe.onerror = settle("blocked");
    probe.src = `https://i.ytimg.com/vi/${videoKey}/default.jpg`;
    return () => {
      window.clearTimeout(timer);
      probe.onload = null;
      probe.onerror = null;
    };
  }, [videoKey]);

  return reach;
}

function TrailerCard({
  video,
  title,
  onPlay,
}: {
  video: MediaVideo;
  title: string;
  onPlay: () => void;
}) {
  const tapGuard = useTapGuard(onPlay);
  return (
    <button
      type="button"
      {...tapGuard}
      className="group/trailer w-[264px] shrink-0 text-left max-md:w-[208px]"
    >
      <div className="dark-media-surface relative aspect-video overflow-hidden rounded-xl bg-[#141824] ring-1 ring-white/[0.08] transition-all duration-300 ease-out group-hover/trailer:-translate-y-1 group-hover/trailer:shadow-[0_16px_40px_rgba(0,0,0,0.55)] group-hover/trailer:ring-white/30">
        <PosterImage
          src={video.thumbnailUrl}
          alt={`${title} ${video.kind}`}
          className="size-full object-cover transition-transform duration-500 ease-out group-hover/trailer:scale-[1.05]"
        />
        <span className="pointer-events-none absolute inset-0 flex items-center justify-center bg-black/25 transition-colors group-hover/trailer:bg-black/10">
          <span className="flex size-11 items-center justify-center rounded-full bg-black/55 text-white ring-1 ring-white/25 backdrop-blur-sm transition-transform duration-300 group-hover/trailer:scale-110">
            <PlayIcon className="ml-0.5 size-5" />
          </span>
        </span>
        <span className="pointer-events-none absolute left-2 top-2 rounded-full bg-black/60 px-2 py-0.5 text-caption font-medium text-white/85 backdrop-blur-sm">
          {video.kind}
        </span>
      </div>
      <p className="mt-2 truncate text-sub text-[var(--text-muted)] transition-colors group-hover/trailer:text-[var(--text)]">
        {video.name}
      </p>
    </button>
  );
}

function TrailerPlayer({
  video,
  title,
  onClose,
}: {
  video: MediaVideo;
  title: string;
  onClose: () => void;
}) {
  const reach = useYoutubeReachable(video.key);

  return (
    <Modal open onClose={onClose} label={`${title} ${video.kind}`} width="full" panelClassName="!max-w-4xl">
      <div className="flex items-start gap-3 px-4 py-3">
        <div className="min-w-0 flex-1">
          <p className="truncate text-ui font-medium text-[var(--text)]">{video.name}</p>
          <p className="mt-0.5 text-caption text-[var(--text-muted)]">
            {title} · {video.kind}
          </p>
        </div>
        <button
          type="button"
          aria-label="关闭"
          onClick={onClose}
          className="btn-glass -mr-1 flex size-8 shrink-0 items-center justify-center !rounded-full"
        >
          <XIcon className="size-4" />
        </button>
      </div>

      <div className="aspect-video w-full bg-black">
        {reach === "blocked" ? (
          <div className="flex size-full flex-col items-center justify-center gap-3 px-6 text-center">
            <p className="text-ui font-medium text-[var(--text)]">当前浏览器无法直连 YouTube</p>
            <p className="max-w-md text-sub leading-6 text-[var(--text-muted)]">
              {
                "预告片由 YouTube 提供，播放需要浏览器本机能访问它。服务端在「设置 → 网络」配的代理只作用于服务端自己抓数据，不经过播放器；给浏览器挂上代理后即可正常播放。"
              }
            </p>
            <a
              href={video.watchUrl}
              target="_blank"
              rel="noreferrer"
              className="btn-glass flex h-9 items-center gap-2 px-4 text-sub font-medium text-[var(--text)]"
            >
              在 YouTube 打开 ↗
            </a>
          </div>
        ) : (
          <iframe
            key={video.key}
            src={`${video.embedUrl}?autoplay=1&rel=0`}
            title={`${title} ${video.kind}`}
            allow="accelerometer; autoplay; encrypted-media; gyroscope; picture-in-picture"
            allowFullScreen
            referrerPolicy="strict-origin-when-cross-origin"
            className="size-full border-0"
          />
        )}
      </div>
    </Modal>
  );
}

/** 预告片横滚条 + 内嵌播放弹层。 */
export function TrailerRow({ title, videos }: { title: string; videos: MediaVideo[] }) {
  const [playing, setPlaying] = useState<MediaVideo | null>(null);

  return (
    <section>
      <h2 className="text-on-image mb-3 text-body-lg font-semibold tracking-[-0.01em] text-[var(--text)]">
        预告片
      </h2>

      <HScroller className="-mx-1 gap-3 px-1 pb-1 pt-1">
        {videos.map((video) => (
          <TrailerCard
            key={video.key}
            video={video}
            title={title}
            onPlay={() => setPlaying(video)}
          />
        ))}
      </HScroller>

      {playing && (
        <TrailerPlayer video={playing} title={title} onClose={() => setPlaying(null)} />
      )}
    </section>
  );
}
