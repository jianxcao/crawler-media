"use client";

import { useCallback, useLayoutEffect, useRef, type ComponentPropsWithRef } from "react";
import { manageArtworkImage, observeArtworkImage } from "@/lib/artwork-loading";

/** Static artwork shares four network slots with preloads and retries. */
export function ArtworkImage({ ref, src, srcSet, sizes, loading = "lazy", ...props }: ComponentPropsWithRef<"img">) {
  const current = useRef<ReturnType<typeof manageArtworkImage> | null>(null);
  const attach = useCallback((image: HTMLImageElement | null) => {
    if (!image) return;
    const managed = manageArtworkImage(image);
    managed.setVisible(false);
    current.current = managed;
    const stop = observeArtworkImage(image, (near, priority) => managed.setVisible(near, priority));
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
  // Native lazy loading could hold a slot without starting traffic inside
  // content-visibility subtrees; viewport admission is managed above.
  return <img {...props} loading="eager" ref={attach} />;
}
