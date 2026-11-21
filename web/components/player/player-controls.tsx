"use client";

import { useEffect, useState } from "react";

import { ActivityIcon, ExpandIcon, MoreIcon, ShrinkIcon } from "@/components/icons";
import type { PlaybackChapterMark } from "@/lib/api/playback";
import type { AudioOption } from "@/lib/player/audio-tracks";
import { QUALITY_OPTIONS } from "@/lib/player/quality";
import type { SubtitleStyle, SubtitleTracks } from "@/lib/player/subtitles";
import type { TrickplayIndex } from "@/lib/player/trickplay";

import {
  AudioGlyph,
  PLAYER_ICON_CLASS,
  RotateGlyph,
  SubtitleGlyph,
} from "./player-icons";
import { IconButton, MenuItem, MenuPanel } from "./player-menu-primitives";
import { PlayerScrubBar } from "./player-scrub-bar";
import { SubtitleMenu } from "./subtitle-menu";

export { PlayerCenterControls } from "./player-center-controls";

export interface PlayerControlsProps {
  positionMs: number;
  video: HTMLVideoElement | null;
  startMs: number | null;
  overrideMs: number | null;
  durationMs: number | null;
  bufferedEndMs: number | null;
  chromeVisible: boolean;
  onSeek: (fileMs: number) => void;
  onScrub: (fileMs: number) => void;
  onScrubCancel: () => void;
  onScrubbingChange: (active: boolean) => void;
  subtitles: SubtitleTracks;
  selectedSubtitle: string | null;
  onSelectSubtitle: (ref: string | null) => void;
  audioOptions: AudioOption[];
  selectedAudio: string | null;
  onSelectAudio: (ref: string) => void;
  subtitleStyle: SubtitleStyle;
  onSubtitleStyleChange: (style: SubtitleStyle) => void;
  diagnosticsOpen: boolean;
  onToggleDiagnostics: () => void;
  landscape: boolean;
  canRotate: boolean;
  onToggleLandscape: () => void;
  fullscreen: boolean;
  onToggleFullscreen: () => void;
  systemSubtitles: boolean;
  quality: number | null;
  onSelectQuality: (maxHeight: number | null) => void;
  onMenuOpenChange: (open: boolean) => void;
  trickplay: TrickplayIndex | null;
  chapters: PlaybackChapterMark[];
  fakeLandscape: boolean;
}

export function PlayerControls(props: PlayerControlsProps) {
  const {
    positionMs,
    video,
    startMs,
    overrideMs,
    durationMs,
    bufferedEndMs,
    chromeVisible,
    onSeek,
    onScrub,
    onScrubCancel,
    onScrubbingChange,
    subtitles,
    selectedSubtitle,
    onSelectSubtitle,
    audioOptions,
    selectedAudio,
    onSelectAudio,
    subtitleStyle,
    onSubtitleStyleChange,
    diagnosticsOpen,
    onToggleDiagnostics,
    landscape,
    canRotate,
    onToggleLandscape,
    fullscreen,
    onToggleFullscreen,
    systemSubtitles,
    quality,
    onSelectQuality,
    onMenuOpenChange,
    trickplay,
    chapters,
    fakeLandscape,
  } = props;

  const [menu, setMenu] = useState<"none" | "audio" | "subtitles" | "settings">("none");
  const openMenu = (next: "none" | "audio" | "subtitles" | "settings") => {
    setMenu(next);
    onMenuOpenChange(next !== "none");
  };

  const [expanded, setExpanded] = useState(false);
  useEffect(() => {
    if (!chromeVisible) {
      setExpanded(false);
      return;
    }
    const timer = window.setTimeout(() => setExpanded(true), 320);
    return () => window.clearTimeout(timer);
  }, [chromeVisible]);

  return (
    <div className="pointer-events-none relative pb-[var(--safe-bottom)]">
      <div
        className={`absolute inset-0 bg-gradient-to-t from-black/80 via-black/40 to-transparent transition-opacity duration-300 ${
          chromeVisible ? "opacity-100" : "opacity-0"
        }`}
      />

      <PlayerScrubBar
        positionMs={positionMs}
        video={video}
        startMs={startMs}
        overrideMs={overrideMs}
        durationMs={durationMs}
        bufferedEndMs={bufferedEndMs}
        chromeVisible={chromeVisible}
        onSeek={onSeek}
        onScrub={onScrub}
        onScrubCancel={onScrubCancel}
        onScrubbingChange={onScrubbingChange}
        trickplay={trickplay}
        chapters={chapters}
        fakeLandscape={fakeLandscape}
      />

      <div
        className={`grid transition-[grid-template-rows] duration-300 ${
          chromeVisible ? "grid-rows-[1fr]" : "grid-rows-[0fr]"
        }`}
      >
        <div className={menu === "none" && !expanded ? "overflow-hidden" : ""}>
          <div
            className={`player-inset-x relative flex items-center pb-4 pt-3 transition-opacity duration-300 max-md:pb-3 ${
              chromeVisible ? "pointer-events-auto opacity-100" : "pointer-events-none opacity-0"
            }`}
          >
            <div className="player-glass flex items-center gap-1 rounded-full px-1.5 py-1">
              {audioOptions.length > 0 ? (
                <div className="relative">
                  <IconButton
                    tip="音轨"
                    open={menu === "audio"}
                    onClick={() => openMenu(menu === "audio" ? "none" : "audio")}
                  >
                    <AudioGlyph />
                  </IconButton>
                  {menu === "audio" ? (
                    <MenuPanel title="音轨" onClose={() => openMenu("none")}>
                      {audioOptions.map((option) => (
                        <MenuItem
                          key={option.ref}
                          active={
                            option.ref === selectedAudio ||
                            (selectedAudio === null && option.isDefault)
                          }
                          onClick={() => {
                            onSelectAudio(option.ref);
                            openMenu("none");
                          }}
                        >
                          {option.label}
                        </MenuItem>
                      ))}
                      <p className="mt-1 border-t border-white/[0.08] px-3 pb-1 pt-2 text-[12px] leading-relaxed text-white/40">
                        换音轨需要重新起流，会从当前位置续上，中间大约停顿一秒。
                      </p>
                    </MenuPanel>
                  ) : null}
                </div>
              ) : null}

              <div className="relative">
                <IconButton
                  tip="字幕"
                  active={Boolean(selectedSubtitle)}
                  open={menu === "subtitles"}
                  onClick={() => openMenu(menu === "subtitles" ? "none" : "subtitles")}
                >
                  <SubtitleGlyph />
                </IconButton>
                {menu === "subtitles" ? (
                  <SubtitleMenu
                    tracks={subtitles}
                    selected={selectedSubtitle}
                    onSelect={(ref) => onSelectSubtitle(ref)}
                    style={subtitleStyle}
                    onStyleChange={onSubtitleStyleChange}
                    systemRendered={systemSubtitles}
                    onClose={() => openMenu("none")}
                  />
                ) : null}
              </div>

              <div className="relative">
                <IconButton
                  tip="设置"
                  open={menu === "settings"}
                  onClick={() => openMenu(menu === "settings" ? "none" : "settings")}
                >
                  <MoreIcon className={PLAYER_ICON_CLASS} />
                </IconButton>
                {menu === "settings" ? (
                  <MenuPanel title="设置" onClose={() => openMenu("none")}>
                    <div className="px-4 pb-1 pt-0.5 text-[12px] font-medium text-white/45">
                      画质
                    </div>
                    {QUALITY_OPTIONS.map((option) => (
                      <MenuItem
                        key={option.label}
                        active={quality === option.maxHeight}
                        onClick={() => {
                          onSelectQuality(option.maxHeight);
                          openMenu("none");
                        }}
                      >
                        {option.label}
                        {option.hint ? (
                          <span className="ml-2 text-[12px] text-white/40">{option.hint}</span>
                        ) : null}
                      </MenuItem>
                    ))}
                    <div className="my-1.5 h-px bg-white/10" />
                    <MenuItem
                      active={diagnosticsOpen}
                      icon={<ActivityIcon className="size-4 shrink-0" />}
                      onClick={() => {
                        onToggleDiagnostics();
                        openMenu("none");
                      }}
                    >
                      播放诊断
                    </MenuItem>
                  </MenuPanel>
                ) : null}
              </div>
            </div>

            <div className="flex-1" />

            <div className="player-glass flex items-center gap-1 rounded-full px-1.5 py-1">
              {canRotate ? (
                <IconButton tip={landscape ? "退出横屏" : "横屏"} onClick={onToggleLandscape}>
                  <RotateGlyph active={landscape} />
                </IconButton>
              ) : null}
              <IconButton tip={fullscreen ? "退出全屏" : "全屏"} onClick={onToggleFullscreen}>
                {fullscreen ? (
                  <ShrinkIcon className={PLAYER_ICON_CLASS} />
                ) : (
                  <ExpandIcon className={PLAYER_ICON_CLASS} />
                )}
              </IconButton>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
