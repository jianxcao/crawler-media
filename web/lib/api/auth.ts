import { request } from "@/lib/http";

/** 后端统一响应信封（自有 API：{ok, data} / {ok, error}）。 */
interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  const envelope = await promise;
  if (envelope.ok !== true) throw new Error("响应不是 {ok:true, data} 信封");
  return envelope.data;
}

/** 能力开关快照：前端据此裁剪入口；安全边界仍在后端 403。 */
export interface SessionCapabilities {
  allow_subscribe: boolean;
  allow_search: boolean;
  allow_direct_download: boolean;
}

/** 当前登录会话。 */
export interface SessionView {
  username: string;
  nickname: string;
  avatar_url: string | null;
  role: "admin" | "member";
  capabilities: SessionCapabilities;
}

/** 自有 API 用户视图。 */
export interface UserView {
  id: string;
  login: string;
  role: "admin" | "member";
}

const TOKEN_KEY = "mc_token";

function persistToken(token: string | null): void {
  if (typeof window === "undefined") return;
  if (token) {
    window.localStorage.setItem(TOKEN_KEY, token);
  } else {
    window.localStorage.removeItem(TOKEN_KEY);
  }
}

/** 把自有 API 的 /auth/login 响应（{token, user}）组装成 SessionView。 */
function sessionFromLogin(data: { token: string; user: UserView }): SessionView {
  persistToken(data.token);
  return {
    username: data.user.login,
    nickname: data.user.login,
    avatar_url: null,
    role: data.user.role,
    capabilities: {
      allow_subscribe: true,
      allow_search: true,
      allow_direct_download: true,
    },
  };
}

function sessionFromUser(user: UserView): SessionView {
  return {
    username: user.login,
    nickname: user.login,
    avatar_url: null,
    role: user.role,
    capabilities: {
      allow_subscribe: true,
      allow_search: true,
      allow_direct_download: true,
    },
  };
}

/** 管理员登录：token 即密码（ADR-0003 明文）。remember 忽略（token 常驻本地）。 */
export function login(
  username: string,
  password: string,
  _remember = false,
): Promise<SessionView> {
  return unwrap(
    request<ApiEnvelope<{ token: string; user: UserView }>>("/auth/login", {
      method: "POST",
      body: JSON.stringify({ username, password }),
    }),
  ).then(sessionFromLogin);
}

/** 退出登录：携带当前 token 通知服务端吊销会话，随后清除本地 token。 */
export async function logout(): Promise<null> {
  try {
    await unwrap(request<ApiEnvelope<null>>("/auth/logout", { method: "POST" }));
  } finally {
    persistToken(null);
  }
  return null;
}

/** 查询当前登录状态；未登录时抛 401（由 http.ts 统一跳转登录页）。 */
export async function getSession(): Promise<SessionView> {
  const user = await unwrap(request<ApiEnvelope<UserView>>("/auth/me"));
  return sessionFromUser(user);
}

