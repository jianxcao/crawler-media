"use client";

import { Link } from "react-router-dom";

import { TaskActionsMenu } from "@/components/job-center";
import { OverflowText } from "@/components/overflow-text";
import { PosterImage } from "@/components/poster-image";
import type {
  ActiveFileDownload,
  ActivePlaybackSession,
  MediaActivityTarget,
} from "@/lib/api/playback";
import { formatBytes } from "@/lib/format";
import { imageUrl } from "@/lib/image-proxy";
import {
  detailHref,
  deviceLabel,
  formatPlayClock,
  formatRate,
  unitLabel,
} from "@/lib/media-activity-helpers";

function MetaLine({
  parts,
  className = "",
}: {
  parts: (string | null | undefined)[];
  className?: string;
}) {
  const kept = parts.filter((part): part is string => Boolean(part && part.trim()));
  if (kept.length === 0) return null;
  return (
    <p className={`truncate text-caption leading-5 text-white/40 ${className}`}>
      {kept.map((part, index) => (
        <span key={index}>
          {index > 0 && <span className="mx-1.5 text-white/20">·</span>}
          {part}
        </span>
      ))}
    </p>
  );
}

function ActivityPoster({
  media,
  className,
}: {
  media: MediaActivityTarget | null;
  className: string;
}) {
  const image = (
    <PosterImage
      src={media?.poster_url ? imageUrl(media.poster_url) : null}
      alt={media?.title ?? "未知内容"}
      className={`${className} rounded-lg object-cover ring-1 ring-white/10`}
    />
  );
  const href = media ? detailHref(media) : null;
  if (!href) return <span className="shrink-0">{image}</span>;
  return (
    <Link to={href} className="shrink-0 transition hover:opacity-80">
      {image}
    </Link>
  );
}

function ActivityTitle({ media }: { media: MediaActivityTarget }) {
  const unit = unitLabel(media);
  const href = detailHref(media);
  const text = (
    <>
      {media.title}
      {media.year != null && (
        <span className="ml-1.5 font-normal text-white/45">{media.year}</span>
      )}
      {unit && <span className="tnum ml-1.5 font-normal text-white/60">{unit}</span>}
      {media.episode_title && (
        <span className="ml-1.5 font-normal text-white/45">{media.episode_title}</span>
      )}
      {!media.browsable && (
        <span className="ml-1.5 rounded-md border border-white/15 px-1.5 py-px text-[11px] font-medium text-white/50">
          仅管理
        </span>
      )}
    </>
  );
  if (!href) {
    return (
      <OverflowText lines={1} className="text-ui font-semibold text-white/90">
        {text}
      </OverflowText>
    );
  }
  return (
    <OverflowText lines={1} className="text-ui font-semibold text-white/90">
      <Link to={href} className="transition hover:text-white">
        {text}
      </Link>
    </OverflowText>
  );
}

function DeviceActionsMenu({
  deviceId,
  deviceLabel: label,
  onEnd,
  onRevoke,
  busy,
}: {
  deviceId: string;
  deviceLabel: string;
  onEnd?: (deviceId: string, label: string) => void;
  onRevoke?: (deviceId: string, label: string) => void;
  busy: boolean;
}) {
  const items = [
    ...(onEnd ? [{ id: "end", label: "结束播放", onSelect: () => onEnd(deviceId, label) }] : []),
    ...(onRevoke
      ? [
          {
            id: "revoke",
            label: "注销此设备",
            tone: "danger" as const,
            onSelect: () => onRevoke(deviceId, label),
          },
        ]
      : []),
  ];
  if (items.length === 0) return null;
  return <TaskActionsMenu ariaLabel={`「${label}」的设备操作`} disabled={busy} items={items} />;
}

function StatusBadge({ paused }: { paused: boolean }) {
  const tone = paused
    ? { pill: "bg-white/[0.08] text-white/55", dot: "bg-white/40" }
    : { pill: "bg-[var(--ok)]/12 text-[var(--ok)]", dot: "bg-[var(--ok)]" };
  return (
    <span
      title={paused ? "已暂停" : "播放中"}
      className={`inline-flex shrink-0 items-center gap-1.5 rounded-full px-2 py-0.5 text-caption font-semibold max-md:bg-transparent max-md:p-0 ${tone.pill}`}
    >
      <span className="relative flex size-1.5 max-md:size-2" aria-hidden="true">
        {!paused && (
          <span
            className={`absolute inline-flex size-1.5 motion-safe:animate-ping rounded-full max-md:size-2 ${tone.dot} opacity-60`}
          />
        )}
        <span className={`relative inline-flex size-1.5 rounded-full max-md:size-2 ${tone.dot}`} />
      </span>
      <span className="max-md:sr-only">{paused ? "已暂停" : "播放中"}</span>
    </span>
  );
}

function ActivityCard({
  children,
  percent,
  muted = false,
}: {
  children: React.ReactNode;
  percent?: number | null;
  muted?: boolean;
}) {
  return (
    <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.03]">
      <div className="flex gap-3.5 p-3.5 pb-4 max-md:gap-3 max-md:p-3 max-md:pb-3.5">
        {children}
      </div>
      {percent != null && (
        <div className="absolute inset-x-0 bottom-0 h-[3px] bg-white/[0.06]">
          <div
            className="h-full transition-[width] duration-700"
            style={{
              width: `${Math.min(100, Math.max(1, percent))}%`,
              backgroundColor: muted ? "rgba(255,255,255,0.3)" : "var(--info)",
            }}
          />
        </div>
      )}
    </div>
  );
}

function ClockLine({
  percent,
  positionMs,
  durationMs,
}: {
  percent: number | null;
  positionMs: number | null;
  durationMs: number | null;
}) {
  if (positionMs == null && percent == null) return null;
  return (
    <p className="tnum mt-auto pt-1 text-caption text-white/45">
      {positionMs != null && (
        <>
          {formatPlayClock(positionMs)}
          {durationMs != null && (
            <span className="text-white/25"> / {formatPlayClock(durationMs)}</span>
          )}
        </>
      )}
      {percent != null && (
        <>
          {positionMs != null && <span className="mx-1.5 text-white/20">·</span>}
          <span className="font-medium text-white/60">{percent}%</span>
        </>
      )}
    </p>
  );
}

export function SessionCard({
  session,
  onEnd,
  onRevoke,
  busy,
}: {
  session: ActivePlaybackSession;
  onEnd: (userId: string, deviceId: string, label: string) => void;
  onRevoke: (userId: string, deviceId: string, label: string) => void;
  busy: boolean;
}) {
  const media = session.media;
  const percent = session.progress_percent;
  const specParts = [
    session.file?.resolution,
    session.file?.video_codec?.toUpperCase(),
    session.file?.hdr,
    session.file?.bit_rate ? `${(session.file.bit_rate / 1_000_000).toFixed(1)} Mbps` : null,
    session.file?.size_bytes ? formatBytes(session.file.size_bytes) : null,
  ].filter(Boolean) as string[];
  return (
    <ActivityCard percent={percent} muted={session.paused}>
      <ActivityPoster media={media} className="h-24 w-16 max-md:h-[76px] max-md:w-[52px]" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-start justify-between gap-2.5">
          <ActivityTitle media={media} />
          <div className="flex shrink-0 items-center gap-1.5">
            <StatusBadge paused={session.paused} />
            <DeviceActionsMenu
              deviceId={session.device_id}
              deviceLabel={deviceLabel(session.client, session.device_name)}
              onEnd={(deviceId, label) => onEnd(session.user_id, deviceId, label)}
              onRevoke={
                session.revocable
                  ? (deviceId, label) => onRevoke(session.user_id, deviceId, label)
                  : undefined
              }
              busy={busy}
            />
          </div>
        </div>
        <MetaLine
          parts={[
            session.member_name,
            deviceLabel(session.client, session.device_name),
            session.client_version,
          ]}
        />
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 text-caption text-white/40">
          {session.play_method === "local" ? (
            <>
              {session.rate_bytes_per_second != null && session.rate_bytes_per_second > 0 ? (
                <span className="tnum font-medium text-[var(--info)]">
                  {formatRate(session.rate_bytes_per_second)}
                </span>
              ) : (
                <span className="text-white/55">本地直连</span>
              )}
              {session.bytes_sent != null && session.bytes_sent > 0 && (
                <span className="tnum">已传输 {formatBytes(session.bytes_sent)}</span>
              )}
              {session.connections > 1 && <span>{session.connections} 条连接</span>}
            </>
          ) : (
            <span>网盘直链 · 流量不经过服务器</span>
          )}
          {specParts.length > 0 && (
            <span className="text-white/35 max-md:hidden">{specParts.join(" · ")}</span>
          )}
        </div>
        <ClockLine
          percent={percent}
          positionMs={session.position_ms}
          durationMs={session.duration_ms}
        />
      </div>
    </ActivityCard>
  );
}

export function DownloadCard({
  download,
  onRevoke,
  busy,
}: {
  download: ActiveFileDownload;
  onRevoke: (userId: string, deviceId: string, label: string) => void;
  busy: boolean;
}) {
  const media = download.media;
  return (
    <ActivityCard percent={download.progress_percent}>
      <ActivityPoster media={media} className="h-24 w-16 max-md:h-[76px] max-md:w-[52px]" />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex items-start justify-between gap-2.5">
          {media ? (
            <ActivityTitle media={media} />
          ) : (
            <OverflowText lines={1} className="text-ui font-semibold text-white/90">
              {download.file_name}
            </OverflowText>
          )}
          <DeviceActionsMenu
            deviceId={download.device_id}
            deviceLabel={deviceLabel(download.client, download.device_name)}
            onRevoke={
              download.revocable
                ? (deviceId, label) => onRevoke(download.user_id, deviceId, label)
                : undefined
            }
            busy={busy}
          />
        </div>
        <MetaLine
          parts={[download.member_name, deviceLabel(download.client, download.device_name)]}
        />
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1 text-caption text-white/40">
          {download.rate_bytes_per_second > 0 && (
            <span className="tnum font-medium text-[var(--info)]">
              {formatRate(download.rate_bytes_per_second)}
            </span>
          )}
          <span className="tnum">
            {formatBytes(download.position_bytes)}
            {download.size_bytes > 0 && (
              <span className="text-white/25"> / {formatBytes(download.size_bytes)}</span>
            )}
            {download.progress_percent != null && (
              <span className="ml-1.5 font-medium text-white/60">
                {download.progress_percent}%
              </span>
            )}
          </span>
          {download.connections > 1 && <span>{download.connections} 条连接</span>}
        </div>
        {media && (
          <OverflowText lines={1} className="mt-auto pt-1 text-caption text-white/30">
            {download.file_name}
          </OverflowText>
        )}
      </div>
    </ActivityCard>
  );
}

export function SectionHeading({
  icon,
  title,
  count,
  trailing,
}: {
  icon: React.ReactNode;
  title: string;
  count: number;
  trailing?: React.ReactNode;
}) {
  return (
    <div className="mb-3 flex items-center gap-2.5">
      {icon}
      <h2 className="text-ui font-semibold text-white/65">{title}</h2>
      <span className="tnum text-caption text-white/30">{count}</span>
      <span aria-hidden="true" className="h-px min-w-8 flex-1 bg-white/[0.09]" />
      {trailing}
    </div>
  );
}
