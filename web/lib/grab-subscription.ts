/** A manual-search target is the API Subscribe UUID, kept unchanged in requests. */
export function parseGrabSubscriptionId(raw: string | null): string | null {
  return raw && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(raw)
    ? raw
    : null;
}
