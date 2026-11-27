"use client";

import { createContext, useContext } from "react";

import {
  AUTH_EPOCH_STORAGE_KEY,
  AUTH_SESSION_CHANNEL,
  type SessionBoundaryEvent,
  type SessionView,
} from "@/lib/api/auth";

export { AUTH_EPOCH_STORAGE_KEY, AUTH_SESSION_CHANNEL };

/**
 * 登录会话的全局上下文：由 AuthGate 在确认登录后注入，
 * 供用户菜单、设置页个人信息等处读取真实账号数据。
 * setSession 用于"改昵称后立即同步所有展示处"，不必整页刷新。
 */
interface SessionContextValue {
  session: SessionView;
  setSession: (session: SessionView) => void;
  loginEpoch?: string;
}

const SessionContext = createContext<SessionContextValue | null>(null);

export interface BroadcastChannelLike {
  addEventListener(type: "message", listener: (ev: { data: unknown }) => void): void;
  removeEventListener(type: "message", listener: (ev: { data: unknown }) => void): void;
  close(): void;
}

export interface WindowLike {
  addEventListener(type: string, listener: (ev: unknown) => void): void;
  removeEventListener(type: string, listener: (ev: unknown) => void): void;
}

export interface DocumentLike {
  visibilityState?: string;
  addEventListener(type: string, listener: () => void): void;
  removeEventListener(type: string, listener: () => void): void;
}

export type SessionMismatchReason =
  | "broadcast"
  | "storage"
  | "focus-mismatch"
  | "session-invalid";

export interface WatchSessionIdentityOptions {
  currentSession: Pick<SessionView, "id" | "role">;
  checkSession: () => Promise<Pick<SessionView, "id" | "role">>;
  onIdentityMismatch: (reason: SessionMismatchReason) => void;
  channelFactory?: (name: string) => BroadcastChannelLike | null;
  targetWindow?: WindowLike | null;
  targetDocument?: DocumentLike | null;
}

function isMismatchBoundaryEvent(
  raw: unknown,
  currentUserId: string,
): boolean {
  if (!raw || typeof raw !== "object") return false;
  const ev = raw as Partial<SessionBoundaryEvent>;
  if (ev.kind === "logout") return true;
  if (ev.kind === "login" && typeof ev.userId === "string" && ev.userId !== currentUserId) {
    return true;
  }
  return false;
}

export function watchSessionIdentity(options: WatchSessionIdentityOptions): () => void {
  let disposed = false;
  let checking = false;

  const channel: BroadcastChannelLike | null = options.channelFactory
    ? options.channelFactory(AUTH_SESSION_CHANNEL)
    : typeof BroadcastChannel !== "undefined"
      ? new BroadcastChannel(AUTH_SESSION_CHANNEL)
      : null;

  const win: WindowLike | null =
    options.targetWindow !== undefined
      ? options.targetWindow
      : typeof window !== "undefined"
        ? (window as unknown as WindowLike)
        : null;

  const doc: DocumentLike | null =
    options.targetDocument !== undefined
      ? options.targetDocument
      : typeof document !== "undefined"
        ? (document as unknown as DocumentLike)
        : null;

  const handleBroadcast = (ev: { data: unknown }) => {
    if (disposed) return;
    if (isMismatchBoundaryEvent(ev.data, options.currentSession.id)) {
      options.onIdentityMismatch("broadcast");
    }
  };

  const handleStorage = (ev: unknown) => {
    if (disposed || !ev || typeof ev !== "object") return;
    const storageEv = ev as { key?: string | null; oldValue?: string | null; newValue?: string | null };
    if (storageEv.key === "mc_token" && storageEv.newValue !== storageEv.oldValue) {
      options.onIdentityMismatch("storage");
      return;
    }
    if (storageEv.key === AUTH_EPOCH_STORAGE_KEY && storageEv.newValue) {
      try {
        const parsed = JSON.parse(storageEv.newValue) as unknown;
        if (isMismatchBoundaryEvent(parsed, options.currentSession.id)) {
          options.onIdentityMismatch("storage");
        }
      } catch {
        /* ignore malformed epoch record */
      }
    }
  };

  const recheckIdentity = () => {
    if (disposed || checking) return;
    checking = true;
    void options
      .checkSession()
      .then((next) => {
        if (disposed) return;
        if (next.id !== options.currentSession.id || next.role !== options.currentSession.role) {
          options.onIdentityMismatch("focus-mismatch");
        }
      })
      .catch((err: unknown) => {
        if (disposed) return;
        const status =
          typeof err === "object" && err !== null && "status" in err
            ? Number((err as { status?: unknown }).status)
            : undefined;
        if (status === 401 || status === 403) {
          options.onIdentityMismatch("session-invalid");
        }
      })
      .finally(() => {
        checking = false;
      });
  };

  const handleVisibility = () => {
    if (!doc || doc.visibilityState === "visible") {
      recheckIdentity();
    }
  };

  channel?.addEventListener("message", handleBroadcast);
  win?.addEventListener("storage", handleStorage);
  win?.addEventListener("focus", recheckIdentity);
  doc?.addEventListener("visibilitychange", handleVisibility);

  return () => {
    disposed = true;
    channel?.removeEventListener("message", handleBroadcast);
    channel?.close();
    win?.removeEventListener("storage", handleStorage);
    win?.removeEventListener("focus", recheckIdentity);
    doc?.removeEventListener("visibilitychange", handleVisibility);
  };
}

export function SessionProvider({
  value,
  children,
}: {
  value: SessionContextValue;
  children: React.ReactNode;
}) {
  return <SessionContext.Provider value={value}>{children}</SessionContext.Provider>;
}

/** 读取当前登录会话。必须在 AuthGate（SessionProvider）内使用。 */
export function useSession(): SessionContextValue {
  const ctx = useContext(SessionContext);
  if (!ctx) throw new Error("useSession 必须在 <SessionProvider> 内使用");
  return ctx;
}

export function useOptionalSession(): SessionContextValue | null {
  return useContext(SessionContext);
}

/** 由昵称生成头像徽标字：中文取首字，拉丁字母取前两位大写。 */
export function initialsOf(name: string): string {
  const trimmed = name.trim();
  if (!trimmed) return "?";
  if (/[一-鿿]/.test(trimmed[0])) return trimmed[0];
  return trimmed.slice(0, 2).toUpperCase();
}
