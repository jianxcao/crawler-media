import { artworkQueue } from "./artwork-queue.ts";
import { artworkRetry } from "./artwork-retry.ts";

export interface ArtworkSource {
  src?: string;
  srcSet?: string;
  sizes?: string;
}

/** All source assignments, including retries, share the same admission queue. */
export function manageArtworkImage(
  image: HTMLImageElement,
  initialPriority = 1,
  onSettled?: () => void,
  onBitmapState?: (hasBitmap: boolean) => void,
) {
  let source: ArtworkSource = {};
  let visible = true;
  let disposed = false;
  let loaded = false;
  let active = false;
  let settledSync = false;
  let requestSeq = 0;
  let priority = initialPriority;
  let lease: ReturnType<typeof artworkQueue.enqueue> | undefined;
  let retry = artworkRetry(image, (src) => request({ ...source, src }));

  function clearSource() {
    image.removeAttribute("srcset");
    image.removeAttribute("src");
    if (!disposed) onBitmapState?.(false);
  }
  function release() {
    active = false;
    settledSync = true;
    requestSeq += 1;
    const previous = lease;
    lease = undefined;
    previous?.finish();
  }
  function request(next: ArtworkSource) {
    if (disposed || !visible || (!next.src && !next.srcSet)) return;
    release();
    const token = ++requestSeq;
    active = true;
    settledSync = false;
    const created = artworkQueue.enqueue(() => {
      if (requestSeq !== token || disposed || !visible) return;
      active = true;
      settledSync = false;
      image.removeAttribute("srcset");
      if (next.sizes) image.setAttribute("sizes", next.sizes);
      else image.removeAttribute("sizes");
      if (next.srcSet) image.setAttribute("srcset", next.srcSet);
      if (next.src) image.src = next.src;
      if (requestSeq === token && !disposed && image.complete && image.naturalWidth > 0) {
        onBitmapState?.(true);
      }
    }, priority);
    if (requestSeq !== token || settledSync || !active) {
      created.finish();
    } else {
      lease = created;
    }
  }
  const onLoad = (event: Event) => {
    if (!active || disposed || !visible) { event.stopImmediatePropagation(); return; }
    loaded = true;
    retry.onLoad();
    release();
    onBitmapState?.(true);
    onSettled?.();
  };
  const onError = (event: Event) => {
    if (!active || disposed || !visible) { event.stopImmediatePropagation(); return; }
    loaded = false;
    onBitmapState?.(false);
    release();
    if (!retry.onError(event)) onSettled?.();
  };
  image.addEventListener("load", onLoad, true);
  image.addEventListener("error", onError, true);

  return {
    load(next: ArtworkSource) {
      if (
        (loaded || active) &&
        (next.src ?? "") === (source.src ?? "") &&
        (next.srcSet ?? "") === (source.srcSet ?? "") &&
        (next.sizes ?? "") === (source.sizes ?? "")
      ) {
        return;
      }
      retry.dispose();
      // Keep a decoded bitmap only while waiting for a real replacement.
      if (!loaded || (!next.src && !next.srcSet)) clearSource();
      release();
      source = next;
      loaded = false;
      retry = artworkRetry(image, (src) => request({ ...source, src }));
      request(source);
    },
    setVisible(near: boolean, nextPriority = priority) {
      priority = nextPriority;
      lease?.prioritize(priority);
      if (near === visible) return;
      visible = near;
      if (loaded) return;
      if (!visible) {
        retry.dispose();
        clearSource();
        release();
      } else {
        request(source);
      }
    },
    dispose() {
      disposed = true;
      retry.dispose();
      if (!loaded) clearSource();
      release();
      image.removeEventListener("load", onLoad, true);
      image.removeEventListener("error", onError, true);
    },
  };
}

/** Explicit loading keeps handler setup before admission and supports navigation abort. */
export function createArtworkImage(signal: AbortSignal, priority = 1) {
  const image = new Image();
  const stopListening = () => signal.removeEventListener("abort", cancel);
  const managed = manageArtworkImage(image, priority, stopListening);
  const cancel = () => {
    managed.dispose();
    image.onload = null;
    image.onerror = null;
  };
  return {
    image,
    load(src: string) {
      if (signal.aborted) return;
      if (src) signal.addEventListener("abort", cancel, { once: true });
      else stopListening();
      managed.load({ src });
    },
  };
}

export { observeArtworkImage } from "./artwork-viewport.ts";
