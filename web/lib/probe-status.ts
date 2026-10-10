export type ProbeStageView = {
  status:
    | "pending"
    | "queued"
    | "running"
    | "succeeded"
    | "failed"
    | "not_applicable"
    | "cancelled";
  failure_count: number;
  next_retry_at_ms: number | null;
  error_kind: string | null;
};

export function probeStatusText(stage: ProbeStageView, nowMs: number): string {
  if (stage.status === "failed" && stage.failure_count >= 5) {
    return "重试失败阶段";
  }
  if (stage.status === "failed" && stage.next_retry_at_ms != null) {
    const waitMs = Math.max(0, stage.next_retry_at_ms - nowMs);
    const seconds = Math.ceil(waitMs / 1000);
    if (stage.error_kind === "http_403") {
      return `读取被拒绝，${seconds} 秒后重试`;
    }
    return `${seconds} 秒后重试`;
  }
  if (stage.status === "queued" || stage.status === "running") return "正在读取";
  if (stage.status === "succeeded") return "已完成";
  return "";
}

export function shouldPollProbeDetails(stages: ProbeStageView[]): boolean {
  return stages.some((stage) => stage.status === "queued" || stage.status === "running");
}
