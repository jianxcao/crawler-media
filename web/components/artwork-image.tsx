"use client";

import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type ComponentPropsWithRef,
} from "react";
import { manageArtworkImage, observeArtworkImage } from "@/lib/artwork-loading";

/** Static artwork shares four network slots with preloads and retries. */
export function ArtworkImage({
  ref,
  src,
  srcSet,
  sizes,
  loading = "lazy",
  style,
  ...props
}: ComponentPropsWithRef<"img">) {
  const imageRef = useRef<HTMLImageElement | null>(null);
  const current = useRef<ReturnType<typeof manageArtworkImage> | null>(null);
  const stopObserverRef = useRef<(() => void) | null>(null);
  const forwardedRef = useRef(ref);
  forwardedRef.current = ref;
  const [hasBitmap, setHasBitmap] = useState(false);
  const eager = loading === "eager";
  const eagerRef = useRef(eager);
  eagerRef.current = eager;

  const attach = useCallback((image: HTMLImageElement | null) => {
    if (!image) return;
    imageRef.current = image;
    const managed = manageArtworkImage(image, 1, undefined, setHasBitmap);
    managed.setVisible(eagerRef.current, eagerRef.current ? 0 : 1);
    current.current = managed;
    stopObserverRef.current = eagerRef.current
      ? null
      : observeArtworkImage(image, (near, priority) => managed.setVisible(near, priority));
    const targetRef = forwardedRef.current;
    const detachRef = typeof targetRef === "function" ? targetRef(image) : undefined;
    if (targetRef && typeof targetRef !== "function") targetRef.current = image;
    return () => {
      stopObserverRef.current?.();
      stopObserverRef.current = null;
      managed.dispose();
      current.current = null;
      imageRef.current = null;
      const cleanupTarget = forwardedRef.current;
      if (typeof detachRef === "function") detachRef();
      else if (typeof cleanupTarget === "function") cleanupTarget(null);
      else if (cleanupTarget) cleanupTarget.current = null;
    };
  }, []);

  useLayoutEffect(() => {
    const image = imageRef.current;
    const managed = current.current;
    if (!image || !managed) return;
    if (eager) {
      stopObserverRef.current?.();
      stopObserverRef.current = null;
      managed.setVisible(true, 0);
    } else if (!stopObserverRef.current) {
      managed.setVisible(false, 1);
      stopObserverRef.current = observeArtworkImage(image, (near, priority) =>
        managed.setVisible(near, priority),
      );
    }
    managed.load({ src, srcSet, sizes });
  }, [src, srcSet, sizes, eager]);
  // Hide <img> until a decoded bitmap exists so queued/retrying/broken <img>
  // elements never flash the browser's broken-image icon + alt text.
  const mergedStyle = hasBitmap ? style : { ...style, opacity: 0 };
  return <img {...props} style={mergedStyle} loading="eager" ref={attach} />;
}
