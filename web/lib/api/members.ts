import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/**
 * 成员 = 系统用户。列表/读写走 `/api/v1/users`。
 */
export interface MemberView {
  id: string;
  login: string;
  role: "admin" | "member";
  enabled: boolean;
  created_at: string;
}

/** 编辑成员的可选字段；未提供的字段不改动。 */
export interface MemberUpdatePayload {
  login?: string;
  password?: string;
  enabled?: boolean;
}

export function listMembers(): Promise<MemberView[]> {
  return unwrap(request<ApiEnvelope<MemberView[]>>("/users"));
}

export function createMember(
  username: string,
  password: string,
  nickname: string,
): Promise<MemberView> {
  // 新契约 User 只有 login/password；nickname 无对应字段，忽略。
  return unwrap(
    request<ApiEnvelope<MemberView>>("/users", {
      method: "POST",
      body: JSON.stringify({ login: username, password }),
    }),
  );
}

export function updateMember(id: string, payload: MemberUpdatePayload): Promise<MemberView> {
  return unwrap(
    request<ApiEnvelope<MemberView>>(`/users/${id}`, {
      method: "PATCH",
      body: JSON.stringify(payload),
    }),
  );
}

/** 禁用/启用成员；响应与后续列表均以服务端 enabled 字段为准。 */
export function setMemberStatus(id: string, enabled: boolean): Promise<MemberView> {
  return unwrap(
    request<ApiEnvelope<MemberView>>(`/users/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),
  );
}

/**
 * 重置密码：前端生成一次性密码，走已有的 PATCH /users/{id}。
 * 改密会撤销该成员全部会话。新密码只应显示一次。
 */
export async function resetMemberPassword(
  id: string,
): Promise<{ id: string; username: string; password: string }> {
  const alphabet = "abcdefghjkmnpqrstuvwxyzACDEFGHJKLMNPQRSTUVWXYZ2345679";
  const bytes = crypto.getRandomValues(new Uint8Array(14));
  const password = Array.from(bytes, (byte) => alphabet[byte % alphabet.length]).join("");
  const user = await updateMember(id, { password });
  return { id: user.id, username: user.login, password };
}

/** 用户删除操作状态：`DELETE /users/{id}` 受理（202）与 `GET /users/removals/{op}` 共用。 */
export interface UserRemovalStatus {
  operation_id: string;
  user_id: string;
  phase: "disabling" | "cleaning" | "finalizing" | "succeeded" | string;
  state: "pending" | "failed" | "succeeded";
  error: string | null;
}

/** 受理删除：返回操作状态，而不是“已删除”。成员行只有 state=succeeded 才可移除。 */
export function deleteMember(id: string): Promise<UserRemovalStatus> {
  return unwrap(request<ApiEnvelope<UserRemovalStatus>>(`/users/${id}`, { method: "DELETE" }));
}

export function getUserRemoval(operationId: string): Promise<UserRemovalStatus> {
  return unwrap(
    request<ApiEnvelope<UserRemovalStatus>>(
      `/users/removals/${encodeURIComponent(operationId)}`,
    ),
  );
}

/**
 * 轮询直到删除离开 pending（成功或失败可见），或超过 `attempts` 次仍 pending 时返回最后状态。
 */
export async function waitForUserRemoval(
  accepted: UserRemovalStatus,
  options: { attempts?: number; intervalMs?: number; sleep?: (ms: number) => Promise<void> } = {},
): Promise<UserRemovalStatus> {
  const attempts = options.attempts ?? 30;
  const intervalMs = options.intervalMs ?? 500;
  const sleep = options.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  let status = accepted;
  for (let i = 0; i < attempts && status.state === "pending"; i += 1) {
    await sleep(intervalMs);
    status = await getUserRemoval(accepted.operation_id);
  }
  return status;
}
