import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

/**
 * 新契约下前端只认新形状：不读旧字段、不保留兼容分支。
 *
 * 这一层是「删干净」的守门人——旧字段一旦重新被写回组件，说明有人在按记忆里
 * 的旧 DTO 做事，而不是按后端实际下发的字段。
 */

const WEB_ROOT = path.resolve(fileURLToPath(new URL("..", import.meta.url)));
const SCAN_DIRS = ["lib", "components"];
const SOURCE_EXTENSIONS = new Set([".ts", ".tsx", ".mjs"]);

/**
 * 旧 DTO 字段与兼容层标记：新契约里没有任何正当含义。
 *
 * `examined` 刻意不在名单里——它是 `data_retention` 的现行契约字段
 * （聚合与每类都有，见 docs/api-contracts/self.md），旧的是 `examined ?? scanned`
 * 那个兜底，不是字段本身。
 */
const LEGACY_TOKENS = [
  "Legacy",
  "can_replace",
  "landing_error",
  "retry_wait",
  "probe_stages",
];

/**
 * 只判定 Job 状态的模块。`"blocked"` / `"waiting"` 在别处有各自的正当含义
 * （浏览器自动播放被拦截、YouTube 嵌入不可达、定时任务等待态），因此旧 Job
 * 状态字面量只在读 `JobView.last_status` 的这几个文件里查。
 */
const JOB_STATUS_MODULES = [
  path.join("lib", "job-attention.ts"),
  path.join("lib", "api", "jobs.ts"),
  path.join("components", "job-center.tsx"),
];
const LEGACY_JOB_STATUS_LITERALS = ['"blocked"', '"retry_wait"', '"cancelling"', '"waiting"'];

async function sourceFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await sourceFiles(full)));
    } else if (SOURCE_EXTENSIONS.has(path.extname(entry.name))) {
      files.push(full);
    }
  }
  return files;
}

test("web/lib 与 web/components 不出现旧 DTO 字段与兼容层标记", async () => {
  const offenders = [];
  for (const dir of SCAN_DIRS) {
    for (const file of await sourceFiles(path.join(WEB_ROOT, dir))) {
      const relative = path.relative(WEB_ROOT, file);
      const source = await readFile(file, "utf8");
      for (const token of LEGACY_TOKENS) {
        if (source.includes(token)) offenders.push(`${relative}: ${token}`);
      }
    }
  }
  assert.deepEqual(offenders, [], "旧字段必须从 DTO 与组件里删干净，不做兼容");
});

test("Job 状态判定只认后端 JobStatus 枚举", async () => {
  const offenders = [];
  for (const relative of JOB_STATUS_MODULES) {
    const source = await readFile(path.join(WEB_ROOT, relative), "utf8");
    for (const literal of LEGACY_JOB_STATUS_LITERALS) {
      if (source.includes(literal)) offenders.push(`${relative}: ${literal}`);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "后端 JobStatus 只有 queued/running/succeeded/failed/cancelled 五档"
  );
});
