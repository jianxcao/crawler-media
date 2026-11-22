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
  const current = useRef<ReturnType<typeof manageArtworkImage> | null>(null);
  const [hasBitmap, setHasBitmap] = useState(false);
  const eager = loading === "eager";
  const eagerRef = useRef(eager);
  eagerRef.current = eager;
  const attach = useCallback((image: HTMLImageElement | null) => {
    if (!image) return;
    const managed = manageArtworkImage(image, 1, undefined, setHasBitmap);
    managed.setVisible(eagerRef.current, eagerRef.current ? 0 : undefined);
    current.current = managed;
    const stop = eagerRef.current
      ? () => undefined
      : observeArtworkImage(image, (near, priority) => managed.setVisible(near, priority));
    const detachRef = typeof ref === "function" ? ref(image) : undefined;
    if (ref && typeof ref !== "function") ref.current = image;
    return () => {
      stop();
      managed.dispose();
      current.current = null;
      if (typeof detachRef === "function") detachRef();
      else if (typeof ref === "function") ref(null);
      else if (ref) ref.current = null;
    };
  }, [ref]);
  useLayoutEffect(() => {
    if (loading === "eager") current.current?.setVisible(true, 0);
    current.current?.load({ src, srcSet, sizes });
  }, [src, srcSet, sizes, loading, ref]);
  // Hide <img> until a decoded bitmap exists so queued/retrying/broken <img>
  // elements never flash the browser's broken-image icon + alt text.
  const mergedStyle = hasBitmap ? style : { ...style, opacity: 0 };
  return <img {...props} style={mergedStyle} loading="eager" ref={attach} />;
}
