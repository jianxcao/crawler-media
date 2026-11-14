/**
 * 进度心跳的准入判定。
 *
 * 心跳是**唯一**的会话保活信号：用户关页面、暂停、切后台都不会发任何信号，
 * 服务端只能按 `last_report_at` 的空闲窗口回收会话（`Store::sweep_stale_sessions`，
 * 窗口 `SESSION_TIMEOUT_SECS = 300`）。心跳一停，会话被回收后分片也就没了，
 * 用户回来点播放只会一直转圈。
 *
 * 暂停是这条链上最容易断的一环：视频暂停后播放阶段**不会**变，但暂停期间拖动
 * 进度条会让 `seeking` 落到阶段上（`seeked` 只记 QoE，不把阶段推回 `playing`），
 * 网络卡住时同理落到 `buffering`。这两种「暂停在非 playing 阶段」都必须继续喂。
 *
 * 暂停心跳不会污染进度：后端对 `paused: true` 的样本不计观看时长
 * （`crates/store/src/playback/event.rs` 的 `watch_increment` 直接返回 0），
 * 位置不变也就不会挪续播点。因此这里只判断「该不该喂」，不判断「喂什么」——
 * 上报体永远是当前位置 + 当前暂停态。
 */

import type { PlayerPhase } from "./machine.ts";

/** 会话还活着、可以喂心跳的阶段：播放中，以及暂停后可能停留的拖动 / 缓冲。 */
const HEARTBEAT_PHASES = new Set<PlayerPhase>(["playing", "buffering", "seeking"]);

export interface HeartbeatConditions {
  /** 播放器当前阶段（见 lib/player/machine.ts）。 */
  phase: PlayerPhase;
  /** 本次会话是否已经报过 `start`：没报过的心跳会在服务端凭空建出一条播放会话。 */
  announced: boolean;
  /** 视频元素当前是否暂停。 */
  paused: boolean;
}

/**
 * 这一拍该不该发进度心跳。
 *
 * - 未暂停：只在 `playing` 阶段发（与既有行为一致，拖动/缓冲不额外上报）；
 * - 暂停：已报过 `start`、且阶段还停在播放/拖动/缓冲，就一直发 `paused: true`。
 */
export function shouldHeartbeat({ phase, announced, paused }: HeartbeatConditions): boolean {
  if (paused) return announced && HEARTBEAT_PHASES.has(phase);
  return phase === "playing";
}
