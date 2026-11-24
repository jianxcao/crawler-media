"use client";

import { useContext, useState } from "react";

import {
  DownloadTargetConfirmBar,
  DownloadTargetDialog,
  submitRememberedTarget,
  type DownloadTargetRequest,
} from "@/components/download-target-dialog";
import { useToast } from "@/components/feedback";
import { PhotoIcon, XIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import { DownloadTargetPrefContext, GrabContext } from "@/components/search/torrent-contexts";
import type { DownloadSubmitResult, DownloadTargetPref } from "@/lib/api/downloaders";
import type { TorrentHit } from "@/lib/api/search";
import { downloadSelectedTorrentForSubscription } from "@/lib/api/subscriptions";
import { CATEGORY_LABEL, type TorrentCategory } from "@/lib/categories";
import { formatRelativeTime } from "@/lib/time";
import {
  mediaTypeOf,
  parsedTitleOf,
  torrentIdOf,
  torrentSizeLabel,
} from "@/lib/torrent-search-view";
import { usePermissions } from "@/lib/permissions";

/** 下载按钮的提交状态：终态（done/exists）不可再点，error 可点重试。 */
type DownloadState = "idle" | "submitting" | "done" | "exists" | "error";

const DOWNLOAD_LABEL: Record<DownloadState, string> = {
  idle: "下载",
  submitting: "提交中…",
  done: "已提交",
  exists: "已在下载器",
  error: "失败·重试",
};

/**
 * 下载按钮：点击先弹「选择保存位置」（download-target-dialog），用户确认
 * 文件落点后由弹窗提交到默认下载器（后端带站点登录态取回 .torrent）。
 */
export function DownloadButton({
  hit,
  className,
  dialogTopmost = false,
}: {
  hit: TorrentHit;
  className: string;
  /** 按钮长在灯箱（z-70）里时置位：保存位置弹窗与确认条都要抬到灯箱之上才看得见 */
  dialogTopmost?: boolean;
}) {
  const { canDirectDownload } = usePermissions();
  const toast = useToast();
  const [state, setState] = useState<DownloadState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [request, setRequest] = useState<DownloadTargetRequest | null>(null);
  const [confirming, setConfirming] = useState<{
    request: DownloadTargetRequest;
    target: DownloadTargetPref;
  } | null>(null);
  const [fallbackReason, setFallbackReason] = useState<string | null>(null);
  const prefs = useContext(DownloadTargetPrefContext);
  if (!canDirectDownload || !hit.enclosure) return null;

  const settled = state === "done" || state === "exists";

  function settle(result: DownloadSubmitResult) {
    setState(result.already_exists ? "exists" : "done");
    const submittedText = result.downloader_name
      ? `已提交到「${result.downloader_name}」`
      : "已提交到下载器";
    toast.success(
      result.already_exists
        ? "该种子已在下载器中，未重复添加"
        : `${submittedText}${result.save_path ? ` · ${result.save_path}` : ""}`,
    );
  }

  function confirmRemembered(req: DownloadTargetRequest, target: DownloadTargetPref) {
    setState("submitting");
    setConfirming(null);
    submitRememberedTarget(req, target)
      .then((result) => {
        if (result === null) {
          setState("idle");
          setFallbackReason("这条种子没匹配到唯一条目，请手动选择保存位置。");
          setRequest(req);
          return;
        }
        settle(result);
      })
      .catch((err) => {
        setState("error");
        setError(err instanceof Error ? err.message : "提交失败");
        toast.error(err instanceof Error ? err.message : "提交失败，请重试");
      });
  }

  function openDialog(e: React.MouseEvent) {
    e.stopPropagation();
    if (state === "submitting" || settled || !hit.enclosure) return;
    setError(null);
    setFallbackReason(null);
    const title = parsedTitleOf(hit);
    const mediaType = mediaTypeOf(hit.release);
    const req: DownloadTargetRequest = {
      site_id: hit.site_id,
      download_url: hit.enclosure,
      torrent_id: torrentIdOf(hit),
      identity:
        title && hit.release.year != null
          ? { kind: mediaType, title, year: hit.release.year }
          : null,
      subtitle: null,
      category: hit.category ?? "other",
    };
    const remembered = prefs.byCategory.get(req.category) ?? null;
    if (!remembered) {
      setRequest(req);
      return;
    }
    if (remembered.kind === "smart" && !req.identity) {
      setFallbackReason("这条种子没解析出条目身份，用不了记住的「智能入库」，请手动选择。");
      setRequest(req);
      return;
    }
    if (remembered.downloader_id != null && remembered.downloader_name === null) {
      setFallbackReason("上次使用的下载器已不可用，请重新选择保存位置。");
      setRequest(req);
      return;
    }
    if (prefs.isStaleDir(remembered)) {
      setFallbackReason(`上次的保存位置 ${remembered.save_path} 已不存在，请重新选择。`);
      setRequest(req);
      return;
    }
    setConfirming({ request: req, target: remembered });
  }

  return (
    <>
      <button
        type="button"
        onClick={openDialog}
        disabled={state === "submitting"}
        title={error ?? undefined}
        className={`${className}${settled ? " cursor-default opacity-75" : ""}${
          state === "submitting" ? " cursor-wait opacity-75" : ""
        }`}
      >
        {DOWNLOAD_LABEL[state]}
      </button>
      {confirming && (
        <DownloadTargetConfirmBar
          request={confirming.request}
          target={confirming.target}
          topmost={dialogTopmost}
          onConfirm={() => confirmRemembered(confirming.request, confirming.target)}
          onChange={() => {
            setRequest(confirming.request);
            setConfirming(null);
          }}
          onForget={() => {
            const { request: req, target } = confirming;
            setConfirming(null);
            void prefs.forget(target.category);
            setFallbackReason(
              `已清除「${CATEGORY_LABEL[target.category as TorrentCategory] ?? target.category}」的默认位置。想重新设一个，在下面勾上「记住本次选择」。`,
            );
            setRequest(req);
          }}
          onClose={() => setConfirming(null)}
        />
      )}
      <DownloadTargetDialog
        request={request}
        remembered={request ? (prefs.byCategory.get(request.category) ?? null) : null}
        reason={fallbackReason}
        topmost={dialogTopmost}
        onClose={() => {
          setRequest(null);
          setFallbackReason(null);
        }}
        onSubmitted={(result) => {
          setState(result.already_exists ? "exists" : "done");
          void prefs.refresh();
        }}
      />
    </>
  );
}

type GrabState = "idle" | "submitting" | "done" | "error";

const GRAB_LABEL: Record<GrabState, string> = {
  idle: "投给订阅",
  submitting: "投递中…",
  done: "已投递",
  error: "失败·重试",
};

/**
 * 手动选种按钮：仅在选种模式（GrabContext 非空）出现在下载按钮旁。
 */
export function GrabButton({ hit, className }: { hit: TorrentHit; className: string }) {
  const grabFor = useContext(GrabContext);
  const toast = useToast();
  const [state, setState] = useState<GrabState>("idle");
  if (!grabFor || !hit.enclosure) return null;

  const grab = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (state === "submitting" || state === "done") return;
    setState("submitting");
    try {
      const { units } = await downloadSelectedTorrentForSubscription(grabFor.id, {
        site_id: hit.site_id,
        torrent_id: torrentIdOf(hit),
        title: hit.title,
        category: hit.category,
        download_url: hit.enclosure,
        size_bytes: hit.size_bytes ?? 0,
        seeders: hit.seeders ?? 0,
        is_free: hit.free,
        hit_and_run: hit.hr,
        publish_time: hit.upload_time,
      });
      setState("done");
      toast.success(`已投给《${grabFor.title}》，覆盖 ${units.length} 个追踪单元`);
    } catch (err) {
      setState("error");
      toast.error(err instanceof Error ? err.message : "投递失败，请稍后重试");
    }
  };

  return (
    <button
      type="button"
      onClick={(e) => void grab(e)}
      disabled={state === "submitting"}
      title={`直接投给《${grabFor.title}》（跳过规则组限制）`}
      className={`${className}${state === "done" ? " cursor-default opacity-75" : ""}${
        state === "submitting" ? " cursor-wait opacity-75" : ""
      }`}
    >
      {GRAB_LABEL[state]}
    </button>
  );
}

/**
 * 移动端单条种子的操作抽屉。
 */
export function TorrentActionsSheet({
  hit,
  open,
  onClose,
  onViewImages,
}: {
  hit: TorrentHit;
  open: boolean;
  onClose: () => void;
  /** 海报模式可从抽屉继续进入图片灯箱；列表模式不提供。 */
  onViewImages?: () => void;
}) {
  const parsedTitle = parsedTitleOf(hit);
  const displayTitle = parsedTitle ?? hit.title;
  const secondaryTitle = parsedTitle ? hit.title : null;
  const size = torrentSizeLabel(hit.size_bytes);

  return (
    <Modal open={open} onClose={onClose} label={`${displayTitle}的资源操作`}>
      <div className="px-4 pb-5 pt-2">
        <div className="mx-auto mb-3 h-1 w-10 rounded-full bg-white/20" aria-hidden="true" />
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <p className="truncate text-ui font-semibold text-[var(--text)]">{displayTitle}</p>
            {secondaryTitle && (
              <p className="mt-1 line-clamp-2 text-caption leading-5 text-[var(--text-muted)]">
                {secondaryTitle}
              </p>
            )}
            <p className="tnum mt-1 text-caption text-[var(--text-faint)]">
              {[
                hit.site_name,
                size,
                `${hit.seeders ?? 0} 做种`,
                hit.upload_time ? formatRelativeTime(hit.upload_time) : null,
              ]
                .filter(Boolean)
                .join(" · ")}
            </p>
          </div>
          <button
            type="button"
            aria-label="关闭资源操作"
            onClick={onClose}
            className="touch-target flex size-8 shrink-0 items-center justify-center rounded-full bg-white/[0.06] text-[var(--text-muted)]"
          >
            <XIcon className="size-4" />
          </button>
        </div>

        <div className="mt-5 grid gap-2">
          {onViewImages && (
            <button
              type="button"
              onClick={onViewImages}
              className="btn-glass flex h-11 w-full items-center justify-center gap-2 text-ui font-medium text-[var(--text)]"
            >
              <PhotoIcon className="size-4" />
              浏览图片
            </button>
          )}
          {hit.detail_url && (
            <a
              href={hit.detail_url}
              target="_blank"
              rel="noreferrer"
              onClick={onClose}
              className="btn-glass flex h-11 w-full items-center justify-center text-ui font-medium text-[var(--text)]"
            >
              查看详情
            </a>
          )}
          <GrabButton
            hit={hit}
            className="flex h-11 w-full items-center justify-center rounded-full border border-[var(--info)]/45 bg-[var(--info)]/15 text-ui font-medium text-[var(--info)] transition-colors active:bg-[var(--info)]/25"
          />
          <DownloadButton
            hit={hit}
            className="btn-accent flex h-11 w-full items-center justify-center rounded-full text-ui font-medium"
          />
        </div>
      </div>
    </Modal>
  );
}
