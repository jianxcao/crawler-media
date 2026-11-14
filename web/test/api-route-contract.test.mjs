import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

/**
 * 请求路径契约：前端只能交出「API 相对路径」。
 *
 * `request()`（lib/http.ts → lib/env.ts 的 apiBaseUrl，默认 `/api/v1`）负责
 * 拼前缀。手写 `/api/v1/...` 会被再拼一次，变成 `/api/v1/api/v1/...` 打空。
 */

const WEB_ROOT = path.resolve(fileURLToPath(new URL("..", import.meta.url)));

/** 扫描范围见 Task 1：组件与 lib 里的源码。 */
const SCAN_DIRS = ["components", "lib"];
/** 唯一允许出现默认前缀的地方：它就是这个前缀的定义处。 */
const PREFIX_OWNER = path.join("lib", "env.ts");
const SOURCE_EXTENSIONS = new Set([".ts", ".tsx", ".mjs"]);

/**
 * 去掉注释，保留字符串字面量。
 *
 * 注释里写路径是文档（例如 lib/http.ts 解释前缀怎么拼），不是请求；只有
 * 真正的字符串才可能被打进 `request()`。扫描前把注释抹掉，避免把文档当违规。
 */
function stripComments(source) {
  let out = "";
  let i = 0;
  while (i < source.length) {
    const ch = source[i];
    const next = source[i + 1];
    if (ch === "/" && next === "/") {
      while (i < source.length && source[i] !== "\n") i += 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      i += 2;
      while (i < source.length && !(source[i] === "*" && source[i + 1] === "/")) i += 1;
      i += 2;
      continue;
    }
    if (ch === '"' || ch === "'" || ch === "`") {
      const quote = ch;
      out += ch;
      i += 1;
      while (i < source.length) {
        const inner = source[i];
        if (inner === "\\") {
          out += inner + (source[i + 1] ?? "");
          i += 2;
          continue;
        }
        out += inner;
        i += 1;
        if (inner === quote) break;
      }
      continue;
    }
    out += ch;
    i += 1;
  }
  return out;
}

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

test("web 源码不再手写 /api/v1 前缀", async () => {
  const offenders = [];
  for (const dir of SCAN_DIRS) {
    for (const file of await sourceFiles(path.join(WEB_ROOT, dir))) {
      const relative = path.relative(WEB_ROOT, file);
      if (relative === PREFIX_OWNER) continue;
      const code = stripComments(await readFile(file, "utf8"));
      if (code.includes("/api/v1")) offenders.push(relative);
    }
  }
  assert.deepEqual(
    offenders,
    [],
    "请求路径必须是相对路径（如 /settings/browser），前缀由 lib/env.ts 的 apiBaseUrl 提供"
  );
});

test("playback 客户端不调用 ping/DELETE session", async () => {
  const source = await readFile(
    path.join(WEB_ROOT, "lib", "api", "playback.ts"),
    "utf8"
  );
  const code = stripComments(source);
  for (const name of [
    "pingPlaybackSession",
    "stopPlaybackSession",
    "stopPlaybackSessionOnUnload",
    "sharePlaybackScope",
  ]) {
    assert.equal(
      new RegExp(`\\b${name}\\b`).test(code),
      false,
      `playback.ts 不应再出现 ${name}：后端没有对应路由`
    );
  }
});
