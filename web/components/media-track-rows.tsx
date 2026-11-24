"use client";

/**
 * 条目页的音轨 / 字幕摘要区（电影与剧集共用）。
 */

import { useEffect, useMemo, useRef, useState } from "react";

import {
  FloatingFocusManager,
  FloatingPortal,
  autoUpdate,
  flip,
  offset,
  shift,
  size,
  useDismiss,
  useFloating,
  useInteractions,
  useRole,
} from "@floating-ui/react";

import { Modal } from "@/components/modal";
import {
  type AudioStream,
  type LibraryItemFile,
  type SubtitleStream,
} from "@/lib/api/libraries";
import { formatVideoResolution } from "@/lib/format";
import {
  TRACK_TONE_CLASS,
  audioEntries,
  fileNameOf,
  groupByLanguage,
  subtitleEntries,
  topAudioSpec,
  videoTrackEntries,
  type LanguageGroup,
  type TrackEntry,
} from "@/lib/media-track-format";
import { useIsMobile } from "@/lib/use-media-query";

export function MediaTrackRows({
  files,
  selectedFileId,
  onSelectedFileIdChange,
}: {
  files: LibraryItemFile[];
  selectedFileId: string | null;
  onSelectedFileIdChange: (fileId: string) => void;
  onChanged?: () => void;
}) {
  const availableFiles = files;

  const selectedFile =
    availableFiles.find((file) => file.file_id === selectedFileId) ?? availableFiles[0];

  const audioGroups = useMemo(
    () => groupByLanguage(audioEntries(selectedFile?.audio_tracks ?? [])),
    [selectedFile?.audio_tracks],
  );
  const videoStem = selectedFile ? fileNameOf(selectedFile).replace(/\.[^.]+$/, "") : "";
  const subtitleGroups = useMemo(
    () => groupByLanguage(subtitleEntries(selectedFile?.subtitle_tracks ?? [], videoStem)),
    [selectedFile?.subtitle_tracks, videoStem],
  );

  if (availableFiles.length === 0 || !selectedFile) return null;
  const multipleVersions = availableFiles.length > 1;
  const fileOptionLabel = (file: LibraryItemFile) => {
    const resolution = file.resolution ? formatVideoResolution(file.resolution) : null;
    const name = fileNameOf(file);
    return resolution ? `${resolution} — ${name}` : name;
  };
  const audioSpec = topAudioSpec(selectedFile?.audio_tracks ?? []);

  return (
    <div className="mt-4 max-w-4xl space-y-2.5">
      {multipleVersions && (
        <select
          aria-label="选择视频文件"
          value={selectedFile.file_id}
          onChange={(event) => onSelectedFileIdChange(event.target.value)}
          className="w-full rounded-xl border border-white/[0.08] bg-white/[0.055] px-3.5 py-2 text-sub text-white/90 outline-none transition focus:border-white/25 focus:bg-white/[0.08]"
        >
          {availableFiles.map((file) => (
            <option key={file.file_id} value={file.file_id}>
              {fileOptionLabel(file)}
            </option>
          ))}
        </select>
      )}

      <TrackRow
        key={`${selectedFile.file_id}:video`}
        label="视频流"
        kind="视频流"
        groups={groupByLanguage(videoTrackEntries(selectedFile))}
        empty="尚未探测"
      />

      <TrackRow
        key={`${selectedFile.file_id}:audio`}
        label="音轨"
        kind="音轨"
        groups={audioGroups}
        trailing={
          audioSpec ? (
            <span className="tnum shrink-0 rounded-[7px] border border-white/[0.14] bg-white/[0.04] px-2.5 py-1 text-caption font-semibold text-white/80 md:ml-auto">
              {audioSpec}
            </span>
          ) : null
        }
        empty="尚未探测"
      />

      <TrackRow
        key={`${selectedFile.file_id}:subtitle`}
        label="字幕"
        kind="字幕"
        groups={subtitleGroups}
        footer={undefined}
        empty="无内封或外挂字幕"
      />
    </div>
  );
}

/** 只读的音轨 / 字幕两行（裸路由无上下文可用时渲染）。 */
export function ReadOnlyTrackRows({
  audioStreams,
  subtitleStreams,
}: {
  audioStreams: AudioStream[] | null;
  subtitleStreams: SubtitleStream[];
}) {
  const audioGroups = useMemo(() => groupByLanguage(audioEntries(audioStreams ?? [])), [audioStreams]);
  const subtitleGroups = useMemo(
    () => groupByLanguage(subtitleEntries(subtitleStreams, "")),
    [subtitleStreams],
  );
  const audioSpec = topAudioSpec(audioStreams ?? []);
  return (
    <div className="mt-4 max-w-4xl space-y-2.5">
      <TrackRow
        label="音轨"
        kind="音轨"
        groups={audioGroups}
        trailing={
          audioSpec ? (
            <span className="tnum shrink-0 rounded-[7px] border border-white/[0.14] bg-white/[0.04] px-2.5 py-1 text-caption font-semibold text-white/80 md:ml-auto">
              {audioSpec}
            </span>
          ) : null
        }
        empty={audioStreams === null ? "尚未探测" : "文件内没有音轨"}
      />
      <TrackRow label="字幕" kind="字幕" groups={subtitleGroups} empty="无内封或外挂字幕" />
    </div>
  );
}

/** 折叠态芯片：统一中性色，颜色留给展开态的格式色块。 */
const CHIP_CLASS =
  "tnum inline-flex h-7 items-center gap-1.5 rounded-[7px] bg-white/[0.075] px-2.5 " +
  "text-caption font-medium leading-none text-white/85 backdrop-blur-md " +
  "shadow-[inset_0_1px_0_rgba(255,255,255,0.06)] " +
  "transition-[transform,background-color] hover:-translate-y-px hover:bg-white/[0.13] " +
  "aria-expanded:bg-white/[0.16] max-md:h-8 " +
  "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent-2)]";

function TrackRow({
  label,
  kind,
  groups,
  empty,
  footer,
  trailing,
  onSelect,
}: {
  label: string;
  kind: string;
  groups: LanguageGroup[];
  empty: string;
  footer?: string;
  trailing?: React.ReactNode;
  onSelect?: (entry: TrackEntry) => void;
}) {
  const isMobile = useIsMobile();
  const [open, setOpen] = useState(false);
  const [focusLanguage, setFocusLanguage] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const focusRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  const changeOpen = (next: boolean) => {
    setOpen(next);
    if (!next) triggerRef.current?.focus();
  };

  const { refs, floatingStyles, context } = useFloating({
    open: open && !isMobile,
    onOpenChange: changeOpen,
    placement: "bottom-start",
    middleware: [
      offset(8),
      flip({ padding: 12 }),
      shift({ padding: 12 }),
      size({
        padding: 12,
        apply({ availableHeight, elements }) {
          elements.floating.style.maxHeight = `${Math.min(336, Math.max(200, availableHeight))}px`;
        },
      }),
    ],
    whileElementsMounted: autoUpdate,
  });
  const { getFloatingProps } = useInteractions([
    useDismiss(context, {
      outsidePress: (event) =>
        !(event.target as Element | null)?.closest?.('[role="dialog"][aria-modal="true"]'),
    }),
    useRole(context, { role: "dialog" }),
  ]);

  const maxChips = isMobile ? 2 : 3;
  const shown = groups.slice(0, maxChips);
  const hidden = groups.slice(maxChips);
  const total = groups.reduce((sum, group) => sum + group.entries.length, 0);

  const toggleAt = (language: string, trigger: HTMLButtonElement) => {
    if (open && focusLanguage === language) {
      changeOpen(false);
      return;
    }
    triggerRef.current = trigger;
    setFocusLanguage(language);
    setOpen(true);
  };

  const list = (
    <TrackList
      scrollRef={scrollRef}
      focusRef={focusRef}
      kind={kind}
      groups={groups}
      total={total}
      focusLanguage={focusLanguage}
      footer={footer}
      onSelect={
        onSelect
          ? (entry) => {
              changeOpen(false);
              onSelect(entry);
            }
          : undefined
      }
    />
  );

  return (
    <div className="flex items-baseline gap-4">
      <span className="w-10 shrink-0 text-sub text-[var(--text-faint)]">{label}</span>
      <div ref={refs.setReference} className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
        {groups.length === 0 ? (
          <span className="text-sub text-[var(--text-muted)]">{empty}</span>
        ) : (
          <>
            {shown.map((group) => (
              <button
                key={group.language}
                type="button"
                className={CHIP_CLASS}
                aria-haspopup="dialog"
                aria-expanded={open && focusLanguage === group.language}
                onClick={(event) => toggleAt(group.language, event.currentTarget)}
              >
                <span>{group.language}</span>
                {group.entries.length === 1 ? (
                  <span className="text-micro font-semibold text-white/50">
                    {group.entries[0]!.format}
                  </span>
                ) : (
                  <span className="font-semibold text-white/50">×{group.entries.length}</span>
                )}
              </button>
            ))}
            {hidden.length > 0 && (
              <button
                type="button"
                className={`${CHIP_CLASS} !bg-white/[0.045] text-[var(--text-muted)] hover:!bg-white/[0.1]`}
                aria-haspopup="dialog"
                aria-expanded={open && focusLanguage === hidden[0]!.language}
                onClick={(event) => toggleAt(hidden[0]!.language, event.currentTarget)}
              >
                +{hidden.length} 种
              </button>
            )}
          </>
        )}
        {trailing}
      </div>

      {open && !isMobile && (
        <FloatingPortal>
          <FloatingFocusManager
            context={context}
            modal={false}
            initialFocus={focusRef}
            returnFocus={false}
          >
            <div
              ref={refs.setFloating}
              style={floatingStyles}
              aria-label={`${kind}列表`}
              className="menu-surface z-50 flex w-[27rem] max-w-[calc(100vw-1.5rem)] flex-col overflow-hidden"
              {...getFloatingProps()}
            >
              {list}
            </div>
          </FloatingFocusManager>
        </FloatingPortal>
      )}

      {isMobile && (
        <Modal
          open={open}
          onClose={() => changeOpen(false)}
          label={`${kind}列表`}
          width="lg"
          panelClassName="flex max-h-[78dvh] flex-col"
        >
          {list}
        </Modal>
      )}
    </div>
  );
}

function TrackList({
  scrollRef,
  focusRef,
  kind,
  groups,
  total,
  focusLanguage,
  footer,
  onSelect,
}: {
  scrollRef: React.RefObject<HTMLDivElement | null>;
  focusRef: React.RefObject<HTMLDivElement | null>;
  kind: string;
  groups: LanguageGroup[];
  total: number;
  focusLanguage: string | null;
  footer?: string;
  onSelect?: (entry: TrackEntry) => void;
}) {
  const entries = groups.flatMap((group) => group.entries);
  const hasSource = entries.some((entry) => entry.external !== null);
  const external = entries.filter((entry) => entry.external).length;

  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      const scroller = scrollRef.current;
      const group = focusRef.current;
      if (!scroller || !group) return;
      scroller.scrollTop +=
        group.getBoundingClientRect().top - scroller.getBoundingClientRect().top - 6;
    });
    return () => cancelAnimationFrame(frame);
  }, [focusLanguage, focusRef, scrollRef]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-baseline justify-between gap-4 border-b border-white/[0.08] px-4 py-2.5">
        <strong className="text-ui font-semibold text-[var(--text)]">
          {kind} · 共 {total} 条
        </strong>
        {hasSource && (
          <span className="tnum text-caption text-[var(--text-faint)]">
            内封 {total - external} · 外挂 {external}
          </span>
        )}
      </div>

      <div
        ref={scrollRef}
        tabIndex={-1}
        className="scroll-thin min-h-0 flex-1 overflow-y-auto p-1.5 outline-none"
      >
        {groups.map((group) => (
          <div
            key={group.language}
            className={`rounded-[10px] p-0.5 ${
              group.language === focusLanguage ? "bg-[#cdd6e6]/[0.07]" : ""
            }`}
          >
            <div
              ref={group.language === focusLanguage ? focusRef : undefined}
              tabIndex={group.language === focusLanguage ? -1 : undefined}
              className="flex items-baseline gap-2 px-2.5 pb-1 pt-2 text-micro font-semibold uppercase tracking-[0.1em] text-[var(--text-faint)] outline-none"
            >
              <span>{group.language}</span>
              <span className="tnum tracking-normal">· {group.entries.length}</span>
            </div>
            {group.entries.map((entry) => (
              <TrackLine
                key={entry.key}
                entry={entry}
                onSelect={entry.preview && onSelect ? () => onSelect(entry) : undefined}
              />
            ))}
          </div>
        ))}
      </div>

      {footer && (
        <div className="border-t border-white/[0.08] px-4 py-2.5 text-caption text-[var(--text-faint)]">
          {footer}
        </div>
      )}
    </div>
  );
}

function TrackLine({
  entry,
  onSelect,
}: {
  entry: TrackEntry;
  onSelect?: () => void;
}) {
  const content = (
    <>
      <span
        className={`inline-flex h-5 items-center justify-center rounded-[5px] px-1.5 text-micro font-bold leading-none ${TRACK_TONE_CLASS[entry.tone]}`}
      >
        {entry.format}
      </span>
      <span className="truncate text-sub text-white/88">{entry.primary}</span>
      <span className="flex items-center gap-1.5">
        <span className="text-caption text-[var(--text-faint)]">{entry.secondary}</span>
        {entry.flags.map((flag) => (
          <span
            key={flag}
            className="rounded-[5px] bg-white/[0.07] px-1.5 py-0.5 text-micro font-semibold text-[var(--text-muted)]"
          >
            {flag}
          </span>
        ))}
        {onSelect && (
          <span
            aria-hidden="true"
            className="flex size-6 shrink-0 items-center justify-center text-sub text-[var(--accent-2)] max-md:size-7"
          >
            <span className="opacity-0 transition-opacity group-hover/line:opacity-100 group-focus-visible/line:opacity-100 max-md:opacity-100">
              ›
            </span>
          </span>
        )}
      </span>
    </>
  );

  const className =
    "grid min-w-0 flex-1 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2.5 " +
    "rounded-[9px] px-2.5 py-1.5 text-left max-md:py-2.5";

  return (
    <div className="group/row relative flex items-center rounded-[9px] transition-colors hover:bg-white/[0.075] has-[:focus-visible]:bg-white/[0.075]">
      {onSelect ? (
        <button
          type="button"
          onClick={onSelect}
          aria-label={`预览字幕：${entry.language} · ${entry.format} · ${entry.primary}`}
          className={`group/line ${className} focus-visible:outline-none`}
        >
          {content}
        </button>
      ) : (
        <div className={`group/line ${className}`}>{content}</div>
      )}
    </div>
  );
}
