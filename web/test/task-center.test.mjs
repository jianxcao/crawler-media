import assert from "node:assert/strict";
import test from "node:test";

import {
  activityScopeFromQuery,
  taskCenterViewFromQuery,
} from "../lib/task-center.ts";

test("任务中心下载中入口深链选择进行中 tab", () => {
  assert.equal(taskCenterViewFromQuery("active"), "active");
});

test("任务中心非法视图安全回退全部", () => {
  assert.equal(taskCenterViewFromQuery("downloads"), "all");
  assert.equal(taskCenterViewFromQuery("unknown"), "all");
  assert.equal(taskCenterViewFromQuery(undefined), "all");
});

test("活动页默认落在观看视角", () => {
  assert.equal(activityScopeFromQuery(undefined), "media");
  assert.equal(activityScopeFromQuery(""), "media");
  // 旧的 media 视图值不再是任务状态，同样落回观看
  assert.equal(activityScopeFromQuery("media"), "media");
});

test("带任务状态的深链进入任务视角", () => {
  assert.equal(activityScopeFromQuery("active"), "tasks");
  assert.equal(activityScopeFromQuery("attention"), "tasks");
  assert.equal(activityScopeFromQuery("all"), "tasks");
  assert.equal(activityScopeFromQuery("history"), "tasks");
});

test("重复查询参数取第一个值，不被数组绕过", () => {
  assert.equal(activityScopeFromQuery(["attention", "media"]), "tasks");
  assert.equal(taskCenterViewFromQuery(["attention", "active"]), "attention");
});
