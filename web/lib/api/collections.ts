import { request } from "@/lib/http";

export interface CollectionSummary {
  id: string;
  name: string;
  item_count: number;
  cover_url: string | null;
}

export interface CollectionItem {
  media_item_id: string;
  library_id: string;
  kind: "movie" | "tv" | "video";
  title: string;
  year: number | null;
  poster_url: string | null;
}

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
  error?: { message?: string };
}

async function unwrap<T>(response: Promise<ApiEnvelope<T>>): Promise<T> {
  const envelope = await response;
  if (!envelope.ok || envelope.data == null) {
    throw new Error(envelope.error?.message ?? "请求失败");
  }
  return envelope.data;
}

export function listCollections(): Promise<CollectionSummary[]> {
  return unwrap(request<ApiEnvelope<CollectionSummary[]>>("/collections"));
}

export function createCollection(name: string): Promise<{ id: string }> {
  return unwrap(
    request<ApiEnvelope<{ id: string }>>("/collections", {
      method: "POST",
      body: JSON.stringify({ name }),
    }),
  );
}

export function renameCollection(id: string, name: string): Promise<{ renamed: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ renamed: boolean }>>(`/collections/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ name }),
    }),
  );
}

export function deleteCollection(id: string): Promise<{ deleted: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/collections/${id}`, { method: "DELETE" }),
  );
}

export function listCollectionItems(id: string): Promise<CollectionItem[]> {
  return unwrap(request<ApiEnvelope<CollectionItem[]>>(`/collections/${id}/items`));
}

export function addCollectionItem(id: string, mediaItemId: string): Promise<{ added: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ added: boolean }>>(`/collections/${id}/items`, {
      method: "POST",
      body: JSON.stringify({ media_item_id: mediaItemId }),
    }),
  );
}

export function removeCollectionItem(id: string, mediaItemId: string): Promise<{ removed: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ removed: boolean }>>(`/collections/${id}/items/${mediaItemId}`, {
      method: "DELETE",
    }),
  );
}
