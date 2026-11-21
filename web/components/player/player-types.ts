import type { PlaybackApiScope, PlaybackSession, PlaybackUnit } from "@/lib/api/playback";
import type { PlayerState } from "@/lib/player/machine";
import type { AdjustKind } from "@/lib/player/touch-adjust";

export interface PageRestoreState {
  unitKey: string;
  playSessionId: string | null;
  renewedFromPlaySessionId?: string | null;
  deviceId?: string;
  audioTrack?: string;
  subtitleTrack?: string;
}

export interface InFlightRenewalState {
  unitKey: string;
  deviceId: string;
  predecessorSid: string;
  promise: Promise<PlaybackSession>;
}

export interface CurrentPageRefValue {
  unitKey: string;
  state: PlayerState;
}

export interface VideoPlayerProps {
  unit: PlaybackUnit;
  /** 片名。条目信息与起播并行加载（§6.10），晚到时先空着 */
  title: string | null;
  /** 副标题：剧集显示「S01E05 · 集名」，电影为 null */
  episodeLabel: string | null;
  posterUrl: string | null;
  /**
   * 分享链接的 `?t=` 起播覆盖（毫秒）。只对进入播放页的第一个单元生效。
   */
  startMsOverride?: number;
  /** 下一集；没有（电影 / 本季最后一集）为 null */
  next: { unit: PlaybackUnit; label: string } | null;
  /** 上一集；没有（电影 / 本季第一集）为 null */
  prev: { unit: PlaybackUnit; label: string } | null;
  onPlayNext: () => void;
  onPlayPrev: () => void;
  onExit: () => void;
  api?: PlaybackApiScope;
}

export interface WebkitPresentationVideo {
  webkitSupportsPresentationMode?: (mode: string) => boolean;
  webkitSetPresentationMode?: (mode: string) => void;
  webkitPresentationMode?: string;
}

export interface AdjustFeedback {
  kind: AdjustKind;
  value: number;
  unsupported?: boolean;
  muted?: boolean;
  leaving?: boolean;
}

export interface SeekFlashFeedback {
  seconds: number;
  leaving: boolean;
}

export interface SeekPreviewFeedback {
  targetMs: number;
  deltaMs: number;
  leaving: boolean;
}

export interface NoticeFeedback {
  text: string;
  leaving: boolean;
}

export const PROGRESS_INTERVAL_MS = 10_000;
export const PENDING_COMMIT_MAX_WAIT_MS = 5_000;
export const IDLE_HIDE_MS = 4000;
