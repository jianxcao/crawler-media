import assert from "node:assert/strict";
import test from "node:test";

import { jobIsActive, jobIsHistorical, jobNeedsAttention, isDismissed, markJobDismissed, markJobUndismissed } from "../lib/job-attention.ts";

 test("忽略只作用于当前运行，下一轮错误重新提醒，撤销可恢复", () => {
  const current = {id: 'run-specific', last_status: 'failed', last_finished_at: '2026-10-05T01:00:00Z'};
  markJobDismissed(current.id, current.last_finished_at);
  assert.equal(isDismissed(current), true);
  assert.equal(jobNeedsAttention(current), false);
  assert.equal(isDismissed({...current,last_finished_at:'2026-10-05T02:00:00Z'}),false);
  markJobUndismissed(current.id, current.last_finished_at);
  assert.equal(isDismissed(current),false);
});

test("没有运行时间的旧形状仍可忽略，但不隐藏后续带时间的运行", () => {
  const legacy = {id:'legacy-run',last_status:'failed'};
  markJobDismissed(legacy.id);
  assert.equal(isDismissed(legacy),true);
  assert.equal(isDismissed({...legacy,last_finished_at:'2026-10-05T01:00:00Z'}),false);
  markJobUndismissed(legacy.id);
});

function job({ status = "failed", dismissedAt = null, nextRunAfter = null } = {}) {
  return { id: "job_1", status, dismissed_at: dismissedAt, dismissed_by: null, next_run_after: nextRunAfter };
}

// issue #221：失败任务此前没有任何出口，永远赖在「需要处理」里，
// 侧栏红角标于是永不熄灭。忽略补上的正是这个出口。

test("失败任务默认要用户处理", () => {
  assert.equal(jobNeedsAttention(job()), true);
  assert.equal(jobIsHistorical(job()), false);
});

test("忽略后失败任务从「需要处理」移到「已结束」", () => {
  const dismissed = job({ dismissedAt: "2026-08-27T10:00:00Z" });
  assert.equal(jobNeedsAttention(dismissed), false);
  assert.equal(jobIsHistorical(dismissed), true);
});

test("忽略不改写状态：它仍然是一条失败记录", () => {
  const dismissed = job({ dismissedAt: "2026-08-27T10:00:00Z" });
  assert.equal(dismissed.status, "failed");
});

test("blocked 任务同样受忽略影响，但它的正常出口是取消", () => {
  assert.equal(jobNeedsAttention(job({ status: "blocked" })), true);
  // blocked 仍占着去重键与资源锁，忽略接口不对它开放（见 services/jobs.dismiss_job）；
  // 真被忽略了也不该继续报警，判定本身保持一致。
  assert.equal(
    jobNeedsAttention(job({ status: "blocked", dismissedAt: "2026-08-27T10:00:00Z" })),
    false,
  );
});

test("成功与取消不受忽略影响，始终算已结束", () => {
  for (const status of ["succeeded", "cancelled"]) {
    assert.equal(jobNeedsAttention(job({ status })), false);
    assert.equal(jobIsHistorical(job({ status })), true);
  }
});

test("进行中的任务既不是待处理也不是历史", () => {
  for (const status of ["queued", "running", "retry_wait", "waiting", "cancelling"]) {
    assert.equal(jobNeedsAttention(job({ status })), false);
    assert.equal(jobIsHistorical(job({ status })), false);
  }
});

test("jobIsActive 区分当期可执行任务与未来计划任务", () => {
  const now = 1750000000;
  // running 等非 queued 状态始终属于活跃任务
  assert.equal(jobIsActive(job({ status: "running" }), now), true);
  // 没有延迟调度或到期排队的 queued 属于活跃任务
  assert.equal(jobIsActive(job({ status: "queued", nextRunAfter: null }), now), true);
  assert.equal(jobIsActive(job({ status: "queued", nextRunAfter: now - 10 }), now), true);
  assert.equal(jobIsActive(job({ status: "queued", nextRunAfter: now }), now), true);
  // 未到时间的未来定时任务不属于当期活跃任务
  assert.equal(jobIsActive(job({ status: "queued", nextRunAfter: now + 3600 }), now), false);
  // 非活跃状态不属于活跃任务
  assert.equal(jobIsActive(job({ status: "succeeded" }), now), false);
});
