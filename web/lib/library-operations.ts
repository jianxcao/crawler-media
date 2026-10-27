import type { LibraryOperation } from "./api/libraries";

export interface OperationWaitOptions {
  signal?: AbortSignal;
  onProgress?: (operation: LibraryOperation) => void;
  intervalMs?: number;
}

function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal?.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal?.reason ?? new DOMException("Aborted", "AbortError"));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", abort, { once: true });
  });
}

/**
 * 等待一个已受理的媒体库操作走到终态。
 *
 * PATCH 换根返回 202 只代表「操作已落库并冻结了库」，真正改完文件归属可能要几十秒；
 * 这里按 operation_id 轮询 `GET /libraries/{id}/operations/{operation_id}`，
 * 只有 `succeeded` 才算改完，`failed` 抛出后端 error（可重试）。
 */
export async function waitForLibraryOperation(
  operation: LibraryOperation,
  getStatus: (operationId: string, signal?: AbortSignal) => Promise<LibraryOperation>,
  options: OperationWaitOptions = {},
): Promise<LibraryOperation> {
  let isFirst = true;
  for (;;) {
    options.signal?.throwIfAborted();
    options.onProgress?.(operation);
    if (operation.state === "succeeded") return operation;
    if (operation.state === "failed" && !isFirst) {
      throw new Error(operation.error ?? "媒体库操作失败，可重新尝试");
    }
    isFirst = false;
    await pause(options.intervalMs ?? 1000, options.signal);
    operation = await getStatus(operation.operation_id, options.signal);
  }
}

/** 操作阶段文案（唯一出处：表单弹窗等调用方直接展示）。 */
export function operationLabel(operation: LibraryOperation): string {
  if (operation.state === "failed") return `操作失败：${operation.error ?? "请重试"}`;
  if (operation.state === "succeeded") return "操作已完成";
  const phase: Record<string, string> = {
    planned: "正在准备",
    frozen: "正在冻结媒体库",
    applying: "正在迁移文件归属",
    checking: "正在校验结果",
  };
  return `正在等待操作完成 · ${phase[operation.state] ?? operation.state}`;
}
