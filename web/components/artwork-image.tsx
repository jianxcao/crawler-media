"use client";

import { useCallback, useRef, type ComponentPropsWithRef } from "react";
import { artworkRetry } from "@/lib/artwork-retry";

/** Lazy static artwork with bounded retries while the server warms its cache. */
export function ArtworkImage({ ref, loading = "lazy", onError, onLoad, ...props }: ComponentPropsWithRef<"img">) {
  const retry = useRef<ReturnType<typeof artworkRetry> | null>(null);
  const attach = useCallback((image: HTMLImageElement | null) => {
    if (!image) return;
    const lease = artworkRetry(image);
    retry.current = lease;
    const detachRef = typeof ref === "function" ? ref(image) : undefined;
    if (ref && typeof ref !== "function") ref.current = image;
    return () => {
      lease.dispose();
      retry.current = null;
      if (typeof detachRef === "function") detachRef();
      else if (typeof ref === "function") ref(null);
      else if (ref) ref.current = null;
    };
  }, [ref]);
  return <img {...props} loading={loading} ref={attach}
    onError={(event) => { if (!retry.current?.onError(event.nativeEvent)) onError?.(event); }}
    onLoad={(event) => { retry.current?.onLoad(); onLoad?.(event); }} />;
}
