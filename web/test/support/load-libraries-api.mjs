import { existsSync, readFileSync, statSync } from "node:fs";
import ts from "typescript";

export function loadLibrariesApi(
  deps = {},
  { cryptoApi = globalThis.crypto, fallbackEmpty = false } = {},
) {
  const entryUrl = new URL("../../lib/api/libraries.ts", import.meta.url);
  const cache = new Map();

  function resolveRelative(specifier, parentUrl) {
    const base = new URL(specifier, parentUrl);
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = new URL(`${base.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve relative module ${specifier} from ${parentUrl.href}`);
  }

  function loadModule(fileUrl) {
    if (cache.has(fileUrl.href)) return cache.get(fileUrl.href).exports;
    const mod = { exports: {} };
    cache.set(fileUrl.href, mod);
    const source = readFileSync(fileUrl, "utf8");
    const js = ts.transpileModule(source, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const localRequire = (name) => {
      if (name.startsWith("./") || name.startsWith("../")) {
        return loadModule(resolveRelative(name, fileUrl));
      }
      if (name in deps) return deps[name];
      if (fallbackEmpty) return {};
      throw new Error(`Unexpected import ${name}`);
    };
    new Function("require", "module", "exports", "crypto", js)(
      localRequire,
      mod,
      mod.exports,
      cryptoApi,
    );
    return mod.exports;
  }

  return loadModule(entryUrl);
}
