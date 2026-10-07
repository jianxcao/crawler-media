#!/usr/bin/env node
/**
 * Next.js → Vite/react-router 机械迁移脚本(一次性):
 * 1. next/link → react-router-dom Link,href → to
 * 2. next/navigation 钩子 → react-router-dom 等价物
 *    (useRouter→useNavigate、usePathname→useLocation,useParams/useSearchParams 同源更名)
 * 3. 去掉 `as Route` 强转(Next typed routes,类型已不存在)
 * 4. router.push/replace/back/prefetch → navigate 等价写法
 * 5. next/image → 原生 <img>(去掉 priority)
 *
 * 特例文件(含 metadata/redirect/notFound/useSearchParams 元组差异)手工处理,不在这里。
 */
import fs from "node:fs";
import path from "node:path";

const root = process.cwd();

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name === "dist" || entry.name === ".next") continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (/\.(ts|tsx)$/.test(entry.name)) out.push(full);
  }
  return out;
}

/** 把 router.push(x)/router.replace(x[, opts]) 重写为 navigate(...) */
function rewriteRouterCalls(src) {
  const re = /router\.(push|replace)\(/g;
  let out = "";
  let last = 0;
  let m;
  while ((m = re.exec(src))) {
    out += src.slice(last, m.index);
    const kind = m[1];
    let depth = 1;
    let j = m.index + m[0].length;
    while (depth > 0 && j < src.length) {
      const c = src[j];
      if (c === "(") depth++;
      else if (c === ")") depth--;
      j++;
    }
    let args = src.slice(m.index + m[0].length, j - 1).trim();
    // Next 的 { scroll: false } 选项 react-router 不认(类型上也没有),剥掉
    args = args.replace(/,\s*\{\s*scroll:\s*false\s*\}\s*$/, "");
    out += kind === "replace" ? `navigate(${args}, { replace: true })` : `navigate(${args})`;
    last = j;
  }
  return out + src.slice(last);
}

const files = walk(root)
  .filter((f) => (f.includes(`${path.sep}app${path.sep}`) || f.includes(`${path.sep}components${path.sep}`) || f.includes(`${path.sep}lib${path.sep}`) || f.includes(`${path.sep}src${path.sep}`)))
  .filter((f) => !f.includes("not-found.tsx") && !f.includes("main.tsx") && !f.includes("layout.tsx"));

let changed = 0;
for (const file of files) {
  let src = fs.readFileSync(file, "utf8");
  const orig = src;

  if (/import Link from "next\/link"/.test(src)) {
    src = src.replace('import Link from "next/link"', 'import { Link } from "react-router-dom"');
    src = src.replace(/<Link\s+href=/g, "<Link to=");
  }

  const navImport = src.match(/import \{(.+?)\} from "next\/navigation"/);
  if (navImport) {
    const names = navImport[1]
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const mapped = names.map((n) => (n === "useRouter" ? "useNavigate" : n === "usePathname" ? "useLocation" : n));
    src = src.replace(navImport[0], `import { ${mapped.join(", ")} } from "react-router-dom"`);
  }

  if (/from "next\/image"/.test(src)) {
    src = src.replace(/import Image from "next\/image";?\n?/, "");
    src = src.replace(/<Image\b/g, "<img");
    src = src.replace(/\s*priority\s*\n/g, "\n");
  }

  src = src.replace(/const router = useRouter\(\)/g, "const navigate = useNavigate()");
  src = src.replace(/usePathname\(\)/g, "useLocation().pathname");
  src = src.replace(/router\.back\(\)/g, "navigate(-1)");
  src = src.replace(/^\s*router\.prefetch\([^;]*\);\s*$/gm, "");
  src = src.replace(/\s+as Route/g, "");
  src = rewriteRouterCalls(src);

  if (src !== orig) {
    fs.writeFileSync(file, src);
    changed++;
  }
}
console.log(`processed ${files.length} files, changed ${changed}`);
