"use client";

import type { CSSProperties } from "react";
import { MediaController } from "media-chrome/react";

import { ConsentDialog } from "@/components/player/consent-dialog";
import { DiagnosticsPanel } from "@/components/player/diagnostics-panel";
import { PlayerCenterControls, PlayerControls } from "@/components/player/player-controls";
import { SubtitleLayer } from "@/components/player/subtitle-layer";

import { PlayerHudOverlays } from "./player-hud-overlays";
import { PlayerTopBar } from "./player-top-bar";
import type { VideoPlayerProps } from "./player-types";
import { useVideoPlayerController } from "./use-video-player-controller";

export type { VideoPlayerProps };

/**
 * 网页播放器编排组件。
 */
export function VideoPlayer(props: VideoPlayerProps) {
  const { title, episodeLabel, posterUrl, next, onPlayNext, onExit } = props;
  const c = useVideoPlayerController(props);

  return (
    <div
      ref={c.containerRef}
      className={`player-root relative size-full bg-black ${
        c.viewport.fakeLandscape ? "player-fake-landscape" : ""
      }`}
      style={
        {
          "--cue-lift": `${c.cueLiftPx}px`,
          "--cue-font-size": c.cueFontPx ? `${c.cueFontPx}px` : undefined,
        } as CSSProperties
      }
      data-chrome={c.chromeVisible ? "visible" : "hidden"}
      onPointerMove={c.gestures.onContainerPointerMove}
      onPointerDown={c.gestures.onContainerPointerDown}
      onPointerLeave={c.gestures.onContainerPointerLeave}
    >
      <MediaController
        noHotkeys
        {...{ gesturesdisabled: "", autohide: "-1" }}
        className="size-full"
        style={{ width: "100%", height: "100%", backgroundColor: "#000" }}
      >
        <video
          ref={c.setVideo}
          slot="media"
          playsInline
          poster={posterUrl ?? undefined}
          onClick={c.gestures.onSurfaceClick}
          onDoubleClick={c.gestures.onSurfaceDoubleClick}
          className="size-full object-contain"
        >
          {c.session.pipSubtitleUrl ? (
            <track key={c.session.pipSubtitleUrl} kind="subtitles" src={c.session.pipSubtitleUrl} />
          ) : null}
        </video>

        <canvas
          ref={c.freeze.freezeCanvasRef}
          {...{ noautohide: "" }}
          aria-hidden
          className={`pointer-events-none absolute inset-0 size-full object-contain ${
            c.freeze.frozen ? "" : "hidden"
          }`}
        />

        {c.gestures.brightness < 1 ? (
          <div
            {...{ noautohide: "" }}
            className="player-brightness-mask pointer-events-none absolute inset-0 bg-black"
            style={{ opacity: 1 - c.gestures.brightness }}
          />
        ) : null}

        <SubtitleLayer
          video={c.video}
          track={
            c.session.activeSubtitle?.ref === c.session.burnedSubtitle
              ? null
              : c.session.systemSubtitles && c.session.activeSubtitle?.kind !== "pgs"
                ? null
                : c.session.activeSubtitle
          }
          style={c.session.subtitleStyle}
          baseOffsetSeconds={(c.session.mode?.originMs ?? 0) / 1000}
          avoidBottomPx={c.chromeVisible ? 172 : 8}
        />

        <PlayerCenterControls
          paused={c.paused}
          visible={
            c.chromeVisible &&
            !c.busy &&
            c.state.phase !== "error" &&
            c.state.phase !== "consent"
          }
          onTogglePlay={c.session.togglePlay}
          onSeekBy={c.seek.seekBy}
        />

        <PlayerTopBar
          chromeVisible={c.chromeVisible}
          landscape={c.viewport.landscape}
          fakeLandscape={c.viewport.fakeLandscape}
          deviceLandscape={c.viewport.deviceLandscape}
          canRotate={c.viewport.canRotate}
          canPip={c.viewport.canPip}
          pipActive={c.viewport.pipActive}
          title={title}
          episodeLabel={episodeLabel}
          speedLabel={c.telemetry.speedLabel}
          onBack={c.viewport.goBack}
          onLock={() => {
            c.gestures.setLocked(true);
            c.gestures.revealLock();
          }}
          onTogglePip={c.viewport.togglePip}
        />

        <PlayerHudOverlays
          locked={c.gestures.locked}
          lockHint={c.gestures.lockHint}
          onUnlock={() => {
            c.gestures.setLocked(false);
            c.setChromeVisible(true);
          }}
          holdSpeed={c.gestures.holdSpeed}
          adjust={c.feedback.adjust}
          seekPreview={c.feedback.seekPreview}
          durationMs={c.durationMs}
          notice={c.feedback.notice}
          seekFlash={c.feedback.seekFlash}
          showPauseOverlay={c.showPauseOverlay}
          busy={c.busy}
          phase={c.state.phase}
          speedLabel={c.telemetry.speedLabel}
          error={c.state.error}
          onRetry={c.retryFromCurrent}
          onExit={onExit}
          showNextCard={c.showNextCard}
          nextLabel={next?.label ?? null}
          onDismissNext={() => c.setNextDismissed(true)}
          onPlayNext={onPlayNext}
        />

        {c.state.phase === "consent" && c.state.decision ? (
          <ConsentDialog
            decision={c.state.decision}
            onGranted={() => {
              c.session.consentGrantedRef.current = true;
              c.dispatch({ type: "consent-granted" });
            }}
            onCancel={onExit}
          />
        ) : null}

        {c.diagnosticsOpen && c.state.session ? (
          <DiagnosticsPanel
            session={c.state.session}
            engine={c.engineRef.current}
            qoe={c.liveQoeStats}
            landscape={c.viewport.landscape}
            onClose={() => c.setDiagnosticsOpen(false)}
          />
        ) : null}

        <div className="pointer-events-none absolute inset-x-0 bottom-0 z-20">
          {c.showPauseOverlay && title ? (
            <div className="player-inset-x min-w-0 [@media(max-height:480px)]:hidden">
              <p className="text-[12px] uppercase tracking-[0.3em] text-white/55">已暂停</p>
              <h2 className="mt-2 truncate text-[32px] font-bold leading-tight text-white drop-shadow-lg max-md:text-[22px]">
                {title}
              </h2>
              {episodeLabel ? (
                <p className="mt-1 truncate text-[16px] text-white/70 max-md:text-[13px]">
                  {episodeLabel}
                </p>
              ) : null}
            </div>
          ) : null}
          <PlayerControls
            positionMs={c.positionMs}
            video={c.video}
            startMs={c.session.mode?.originMs ?? null}
            overrideMs={c.overrideMs}
            durationMs={c.durationMs}
            bufferedEndMs={c.engine.bufferedEndMs}
            chromeVisible={c.chromeVisible}
            onSeek={c.seek.commitSeek}
            onScrub={c.seek.scrubTo}
            onScrubCancel={c.seek.cancelScrubFollow}
            onScrubbingChange={c.setScrubbing}
            subtitles={c.session.subtitles}
            selectedSubtitle={c.session.selectedSubtitle}
            onSelectSubtitle={c.session.selectSubtitle}
            audioOptions={c.session.audioOptions}
            selectedAudio={c.state.session?.decision.audio?.track_ref ?? null}
            onSelectAudio={c.session.selectAudio}
            subtitleStyle={c.session.subtitleStyle}
            onSubtitleStyleChange={c.handleSubtitleStyleChange}
            diagnosticsOpen={c.diagnosticsOpen}
            onToggleDiagnostics={c.toggleDiagnostics}
            landscape={c.viewport.landscape}
            canRotate={c.viewport.canRotate}
            onToggleLandscape={c.viewport.toggleLandscape}
            systemSubtitles={c.session.systemSubtitles}
            quality={c.session.quality}
            onSelectQuality={c.session.selectQuality}
            fullscreen={c.viewport.fullscreen}
            onToggleFullscreen={c.viewport.toggleFullscreen}
            onMenuOpenChange={c.setMenuOpen}
            trickplay={c.session.trickplay}
            chapters={c.session.chapters}
            fakeLandscape={c.viewport.fakeLandscape}
          />
        </div>
      </MediaController>
    </div>
  );
}
