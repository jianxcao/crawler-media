type Visibility = (near: boolean, priority: number) => void;
const callbacks = new WeakMap<Element, { near: boolean; visible: boolean; change: Visibility }>();
let nearObserver: IntersectionObserver | undefined;
let visibleObserver: IntersectionObserver | undefined;

/** Continuous observation: loaded bitmaps stay; pending offscreen traffic is released. */
export function observeArtworkImage(image: Element, change: Visibility) {
  if (typeof IntersectionObserver === "undefined") {
    change(true, 0);
    return () => {};
  }
  const notify = (records: IntersectionObserverEntry[], visible: boolean) => {
    for (const record of records) {
      const entry = callbacks.get(record.target);
      if (!entry) continue;
      if (visible) entry.visible = record.isIntersecting;
      else entry.near = record.isIntersecting;
      entry.change(entry.near || entry.visible, entry.visible ? 0 : 1);
    }
  };
  nearObserver ??= new IntersectionObserver((records) => notify(records, false), { rootMargin: "400px" });
  visibleObserver ??= new IntersectionObserver((records) => notify(records, true), { rootMargin: "0px" });
  callbacks.set(image, { near: false, visible: false, change });
  nearObserver.observe(image);
  visibleObserver.observe(image);
  return () => {
    nearObserver?.unobserve(image);
    visibleObserver?.unobserve(image);
    callbacks.delete(image);
  };
}
