"use client";

import { useCallback, useEffect, useState, type RefObject } from "react";

import {
  asNativeVideo,
  enterLandscape,
  enterNativeVideoFullscreen,
  exitLandscape,
  requestFullscreen,
  screenOrientation,
} from "@/lib/player/orientation";
import { pipSupported } from "@/lib/player/pip";

import type { WebkitPresentationVideo } from "./player-types";

export function usePlayerViewport({
  video,
  containerRef,
  flashNotice,
  onExit,
}: {
  video: HTMLVideoElement | null;
  containerRef: RefObject<HTMLDivElement | null>;
  flashNotice: (text: string) => void;
  onExit: () => void;
}) {
  const [landscape, setLandscape] = useState(false);
  const [deviceLandscape, setDeviceLandscape] = useState(false);
  const [fakeLandscape, setFakeLandscape] = useState(false);
  const [fullscreen, setFullscreen] = useState(false);
  const [canRotate, setCanRotate] = useState(false);
  const [canPip, setCanPip] = useState(false);
  const [pipActive, setPipActive] = useState(false);

  useEffect(() => {
    setCanRotate(window.matchMedia("(pointer: coarse)").matches);
    const landscapeQuery = window.matchMedia("(orientation: landscape)");
    const sync = () => setDeviceLandscape(landscapeQuery.matches);
    sync();
    landscapeQuery.addEventListener("change", sync);
    return () => landscapeQuery.removeEventListener("change", sync);
  }, []);

  useEffect(() => {
    if (!video) {
      setCanPip(false);
      setPipActive(false);
      return;
    }
    const webkit = video as WebkitPresentationVideo;
    const webkitProbe = webkit.webkitSupportsPresentationMode;
    setCanPip(
      pipSupported({
        disabled: video.disablePictureInPicture === true,
        webkitSupports:
          typeof webkitProbe === "function"
            ? webkitProbe.call(video, "picture-in-picture") === true
            : null,
        standardEnabled: document.pictureInPictureEnabled === true,
      }),
    );
    const sync = () =>
      setPipActive(
        document.pictureInPictureElement === video ||
          webkit.webkitPresentationMode === "picture-in-picture",
      );
    sync();
    video.addEventListener("enterpictureinpicture", sync);
    video.addEventListener("leavepictureinpicture", sync);
    video.addEventListener("webkitpresentationmodechanged", sync);
    return () => {
      video.removeEventListener("enterpictureinpicture", sync);
      video.removeEventListener("leavepictureinpicture", sync);
      video.removeEventListener("webkitpresentationmodechanged", sync);
    };
  }, [video]);

  const togglePip = useCallback(() => {
    if (!video) return;
    const webkit = video as WebkitPresentationVideo;
    if (typeof webkit.webkitSetPresentationMode === "function") {
      const target =
        webkit.webkitPresentationMode === "picture-in-picture" ? "inline" : "picture-in-picture";
      webkit.webkitSetPresentationMode(target);
      if (target === "picture-in-picture") {
        window.setTimeout(() => {
          if (webkit.webkitPresentationMode !== "picture-in-picture") {
            const standalone = (navigator as { standalone?: boolean }).standalone === true;
            flashNotice(
              standalone
                ? "iOS 桌面应用暂不支持画中画，请在 Safari 中打开本站使用"
                : "系统未开启画中画（低电量模式或当前环境可能不支持）",
            );
          }
        }, 500);
      }
      return;
    }
    if (document.pictureInPictureElement === video) {
      void document.exitPictureInPicture().catch(() => undefined);
      return;
    }
    void video.requestPictureInPicture?.().catch((error: unknown) => {
      const name =
        typeof error === "object" && error && "name" in error
          ? String((error as { name: unknown }).name)
          : "";
      flashNotice(
        name === "NotAllowedError"
          ? "浏览器拒绝了画中画（需要页面处于可交互状态）"
          : "画中画启动失败（此视频源或环境不支持）",
      );
      console.warn("画中画请求被拒绝：", error);
    });
  }, [video, flashNotice]);

  const toggleFullscreen = useCallback(() => {
    if (document.fullscreenElement) {
      void document.exitFullscreen();
      return;
    }
    const target = containerRef.current;
    if (!target) return;
    void requestFullscreen(target).then((ok) => {
      if (!ok) enterNativeVideoFullscreen(asNativeVideo(video));
    });
  }, [containerRef, video]);

  const toggleLandscape = useCallback(() => {
    if (landscape || document.fullscreenElement) {
      setLandscape(false);
      setFakeLandscape(false);
      void exitLandscape(
        screenOrientation(),
        document.fullscreenElement ? () => document.exitFullscreen() : null,
      );
      return;
    }
    const target = containerRef.current;
    if (!target) return;
    void enterLandscape(target, screenOrientation()).then((outcome) => {
      if (outcome === "unsupported") setFakeLandscape(true);
      setLandscape(true);
    });
  }, [containerRef, landscape]);

  useEffect(() => {
    const onChange = () => {
      const active = Boolean(document.fullscreenElement);
      setFullscreen(active);
      if (active) return;
      setLandscape(false);
      void exitLandscape(screenOrientation(), null);
    };
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  useEffect(() => {
    return () => {
      void exitLandscape(
        screenOrientation(),
        typeof document !== "undefined" && document.fullscreenElement
          ? () => document.exitFullscreen()
          : null,
      );
    };
  }, []);

  const goBack = useCallback(() => {
    if (landscape) toggleLandscape();
    else if (document.fullscreenElement) void document.exitFullscreen();
    else onExit();
  }, [landscape, toggleLandscape, onExit]);

  return {
    landscape,
    deviceLandscape,
    fakeLandscape,
    fullscreen,
    canRotate,
    canPip,
    pipActive,
    togglePip,
    toggleFullscreen,
    toggleLandscape,
    goBack,
  };
}
