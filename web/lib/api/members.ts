import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/**
 * 成员 = 系统用户。列表/读写走 `/api/v1/users`（相对 `VITE_API_BASE_URL`）。
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

export function deleteMember(id: string): Promise<void> {
  return unwrap(request<ApiEnvelope<void>>(`/users/${id}`, { method: "DELETE" }));
}
