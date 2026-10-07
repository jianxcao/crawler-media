export interface VerificationResult {
  ok: boolean;
  error?: string | null;
}

export interface VerificationState {
  status: "active" | "failed" | "pending" | "unverified" | "disabled";
  last_error: string | null;
}

export function verificationState(res?: VerificationResult | null): VerificationState {
  if (!res) {
    return {
      status: "pending",
      last_error: null,
    };
  }
  if (res.ok) {
    return {
      status: "active",
      last_error: null,
    };
  }
  return {
    status: "failed",
    last_error: res.error?.trim() || "连接验证失败",
  };
}
