import { artworkQueue } from "./artwork-queue.ts";
import { artworkRetry } from "./artwork-retry.ts";

export interface ArtworkSource {
  src?: string;
  srcSet?: string;
  sizes?: string;
}

/** All source assignments, including retries, share the same admission queue. */
export function manageArtworkImage(image: HTMLImageElement, initialPriority = 1) {
  let source: ArtworkSource = {};
  let visible = true;
  let disposed = false;
  let loaded = false;
  let priority = initialPriority;
  let lease: ReturnType<typeof artworkQueue.enqueue> | undefined;
  let retry = artworkRetry(image, (src) => request({ ...source, src }));

  function clearSource() {
    image.removeAttribute("srcset");
    image.removeAttribute("src");
  }
  function release() {
    const previous = lease;
    lease = undefined;
    previous?.finish();
  }
  function request(next: ArtworkSource) {
    if (disposed || !visible || (!next.src && !next.srcSet)) return;
    lease = artworkQueue.enqueue(() => {
      image.removeAttribute("srcset");
      if (next.sizes) image.setAttribute("sizes", next.sizes);
      else image.removeAttribute("sizes");
      if (next.srcSet) image.setAttribute("srcset", next.srcSet);
      if (next.src) image.src = next.src;
    }, priority);
  }
  const onLoad = (event: Event) => {
    if (!lease || disposed || !visible) { event.stopImmediatePropagation(); return; }
    loaded = true;
    retry.onLoad();
    release();
  };
  const onError = (event: Event) => {
    if (!lease || disposed || !visible) { event.stopImmediatePropagation(); return; }
    release();
    retry.onError(event);
  };
  image.addEventListener("load", onLoad, true);
  image.addEventListener("error", onError, true);

  return {
    load(next: ArtworkSource) {
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
  const managed = manageArtworkImage(image, priority);
  const cancel = () => {
    managed.dispose();
    image.onload = null;
    image.onerror = null;
  };
  signal.addEventListener("abort", cancel, { once: true });
  if (signal.aborted) cancel();
  return { image, load: (src: string) => managed.load({ src }) };
}

export { observeArtworkImage } from "./artwork-viewport.ts";
