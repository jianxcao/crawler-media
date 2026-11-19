import { request, resolveRequestUrl } from "@/lib/http";
import { waitForLibraryOperation, type OperationWaitOptions } from "@/lib/library-operations";
import { waitForLibraryRemoval, type RemovalWaitOptions } from "@/lib/library-removal";
import type { LibraryKind } from "@/lib/media-types";

import {
  type ApiEnvelope,
  arrayPayload,
  emptyLibrary,
  libraryFrom,
  libraryOperationFrom,
  unwrap,
} from "./helpers";
import type {
  LibraryOperation,
  LibraryPayload,
  LibraryRemoval,
  LibraryUpdate,
  LibraryUpdatePayload,
  MediaLibrary,
  RoutingOptions,
} from "./types";

/**
 * 列出全部媒体库（可按类型过滤）。
 * GET /libraries → [{id, name, kind, root_paths, is_default, stats}]
 */
export function listLibraries(
  kind?: LibraryKind,
  options?: RequestInit | { manage?: boolean; scope?: string },
): Promise<MediaLibrary[]> {
  const params = new URLSearchParams();
  if (kind) params.set("kind", kind);
  let init: RequestInit | undefined = undefined;
  if (options) {
    if ("manage" in options && options.manage) {
      params.set("manage", "1");
    } else if ("scope" in options && options.scope) {
      params.set("scope", options.scope);
    } else {
      init = options as RequestInit;
    }
  }
  const suffix = params.size > 0 ? `?${params}` : "";
  return unwrap(request<ApiEnvelope<unknown[]>>(`/libraries${suffix}`, init)).then((raw) =>
    arrayPayload(raw, "媒体库列表").map(libraryFrom),
  );
}

/** 获取单个媒体库详情（无 GET /libraries/{id} 端点，从列表取；找不到返回空壳）。 */
export async function getLibrary(id: string): Promise<MediaLibrary> {
  const libs = await listLibraries();
  return libs.find((lib) => lib.id === id) ?? emptyLibrary();
}

/** 创建媒体库（POST /libraries；后端只取 name/kind/root_paths，其余字段忽略）。 */
export function createLibrary(payload: LibraryPayload): Promise<MediaLibrary> {
  return unwrap(
    request<ApiEnvelope<unknown>>(`/libraries`, { method: "POST", body: JSON.stringify(payload) }),
  ).then((raw) => libraryFrom(raw));
}

/**
 * 更新媒体库（PATCH /libraries/{id}；只发要改的字段）。
 *
 * 换根会返回 202 + `{operation_id}`（不是库详情），调用方必须区分两种结果；
 * 同步字段更新返回 200 + 最新库。
 */
export async function updateLibrary(
  id: string,
  payload: LibraryUpdatePayload,
  signal?: AbortSignal,
): Promise<LibraryUpdate> {
  signal?.throwIfAborted();
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(`/libraries/${id}`, {
      method: "PATCH",
      body: JSON.stringify(payload),
      signal,
    }),
  );
  const operation = libraryOperationFrom(raw);
  return operation
    ? { kind: "operation", operation }
    : { kind: "library", library: libraryFrom(raw) };
}

/** 轮询一个已受理的换根操作（GET /libraries/{id}/operations/{operation_id}）。 */
export function getLibraryOperation(
  libraryId: string,
  operationId: string,
  signal?: AbortSignal,
): Promise<LibraryOperation> {
  return unwrap(
    request<ApiEnvelope<unknown>>(
      `/libraries/${libraryId}/operations/${encodeURIComponent(operationId)}`,
      { signal },
    ),
  ).then((raw) => {
    const operation = libraryOperationFrom(raw);
    if (!operation) throw new Error("媒体库操作状态缺失");
    return operation;
  });
}

/**
 * 更新并等到真正生效：换目录的 202 只表示「已受理并冻结」，必须轮询到 succeeded，
 * 再用列表里的最新事实重新读库（没有 GET /libraries/{id}）。
 */
export async function updateLibraryAndWait(
  id: string,
  payload: LibraryUpdatePayload,
  options: OperationWaitOptions = {},
): Promise<MediaLibrary> {
  const accepted = await updateLibrary(id, payload, options.signal);
  if (accepted.kind === "library") return accepted.library;
  await waitForLibraryOperation(
    accepted.operation,
    (operationId, signal) => getLibraryOperation(id, operationId, signal),
    options,
  );
  return getLibrary(id);
}

/** 设为该类型的默认库（PUT /libraries/{id}/default）。 */
export function setDefaultLibrary(id: string): Promise<MediaLibrary> {
  return unwrap(request<ApiEnvelope<unknown>>(`/libraries/${id}/default`, { method: "PUT" })).then(
    (raw) => libraryFrom(raw),
  );
}

/** 重排媒体库展示顺序（PUT /libraries/order，全量 id 一次提交）。 */
export function reorderLibraries(orderedIds: string[]): Promise<Record<string, never>> {
  return unwrap(
    request<ApiEnvelope<Record<string, never>>>(`/libraries/order`, {
      method: "PUT",
      body: JSON.stringify({ ids: orderedIds }),
    }),
  );
}

export function getLibraryRemoval(
  operationId: string,
  signal?: AbortSignal,
): Promise<LibraryRemoval> {
  return unwrap(request<ApiEnvelope<LibraryRemoval>>(`/library-removals/${operationId}`, { signal }));
}

/** 发起删除；调用者必须等到操作成功，不能把 202 当成已完成。 */
export function deleteLibrary(id: string, signal?: AbortSignal): Promise<LibraryRemoval> {
  signal?.throwIfAborted();
  return unwrap(request<ApiEnvelope<LibraryRemoval>>(`/libraries/${id}`, { method: "DELETE", signal }));
}

export async function removeLibraryAndWait(
  id: string,
  options: RemovalWaitOptions = {},
): Promise<LibraryRemoval> {
  const operation = await deleteLibrary(id, options.signal);
  return waitForLibraryRemoval(operation, getLibraryRemoval, options);
}

/** 收藏范围可选项（GET /libraries/routing-options）。 */
export function listLibraryRoutingOptions(): Promise<RoutingOptions> {
  return unwrap(request<ApiEnvelope<RoutingOptions>>("/libraries/routing-options"));
}

/** 海报图片地址（GET /posters/{ledger_id}，按库可见性鉴权）。 */
export function posterUrl(ledgerId: string): string {
  return resolveRequestUrl(`/posters/${encodeURIComponent(ledgerId)}`);
}

export async function previewRootConsolidation(
  libraryId: string,
): Promise<{ duplicates: { keep: string; merge: string }[] }> {
  return unwrap(
    request<ApiEnvelope<{ duplicates: { keep: string; merge: string }[] }>>(
      `/libraries/${libraryId}/root-consolidation-preview`,
    ),
  );
}

export async function applyRootConsolidations(
  libraryId: string,
  roots: string[],
  options: OperationWaitOptions = {},
): Promise<{ consolidated: number; roots: string[] }> {
  const before = await getLibrary(libraryId);
  const removed = new Set(roots);
  const remaining = before.root_paths.filter((path) => !removed.has(path));
  if (remaining.length === 0) throw new Error("媒体库至少需要一个目录");
  const after = await updateLibraryAndWait(libraryId, { root_paths: remaining }, options);
  return {
    consolidated: before.root_paths.length - remaining.length,
    roots: after.root_paths,
  };
}
