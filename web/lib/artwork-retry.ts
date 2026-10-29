const MAX_RETRIES = 20;
const MAX_RETRY_MS = 5_000;
const NATIVE_RETRIES = 4;

function retryLimit(source: string): number {
  let url: URL;
  try { url = new URL(source); } catch { return 0; }
  if (url.protocol !== "http:" && url.protocol !== "https:") return 0;
  if (/\/images\/proxy$/.test(url.pathname)
      || /\/media\/[^/]+\/poster$/.test(url.pathname)) return MAX_RETRIES;
  if (/\/(?:posters|fanart|stills|chapters)\//.test(url.pathname)
      || /\/libraries\/[^/]+\/cover$/.test(url.pathname)) return NATIVE_RETRIES;
  return 0;
}

/** Retry cache warmup and transient native artwork deadlines without holding a connection. */
export function artworkRetry(image: HTMLImageElement) {
  let attempts = 0;
  let previousSource = "";
  let timer: ReturnType<typeof setTimeout> | undefined;
  const onError = (event: Event) => {
    const source = image.src;
    if (source !== previousSource) attempts = 0;
    if (attempts >= retryLimit(source)) return false;
    event.stopImmediatePropagation();
    clearTimeout(timer);
    attempts += 1;
    timer = setTimeout(() => {
      timer = undefined;
      // A component may switch images while this cache miss is pending.
      if (image.src !== source) return;
      const next = new URL(source);
      next.searchParams.set("_retry", String(attempts));
      previousSource = next.href;
      image.src = previousSource;
    }, Math.min(MAX_RETRY_MS, 1_000 * 2 ** (attempts - 1)));
    return true;
  };
  const onLoad = () => {
    clearTimeout(timer);
    attempts = 0;
    previousSource = "";
  };
  return { onError, onLoad, dispose: () => clearTimeout(timer) };
}

export function installArtworkRetry(image: HTMLImageElement): () => void {
  const retry = artworkRetry(image);
  image.addEventListener("error", retry.onError);
  image.addEventListener("load", retry.onLoad);
  return () => {
    retry.dispose();
    image.removeEventListener("error", retry.onError);
    image.removeEventListener("load", retry.onLoad);
  };
}

/** Also used by slideshow/hero preloads, before their existing onerror handler. */
export function createArtworkImage(signal: AbortSignal): HTMLImageElement {
  const image = new Image();
  const detachRetry = installArtworkRetry(image);
  const cancel = () => {
    detachRetry();
    image.removeEventListener("load", loaded);
    image.onload = null;
    image.onerror = null;
    image.removeAttribute("src");
  };
  const loaded = () => {
    detachRetry();
    signal.removeEventListener("abort", cancel);
  };
  signal.addEventListener("abort", cancel, { once: true });
  image.addEventListener("load", loaded, { once: true });
  if (signal.aborted) cancel();
  return image;
}
