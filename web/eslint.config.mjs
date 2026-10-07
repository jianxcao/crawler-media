import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

const nodeGlobals = {
  process: "readonly",
  console: "readonly",
  setTimeout: "readonly",
  clearTimeout: "readonly",
  setInterval: "readonly",
  clearInterval: "readonly",
  URLSearchParams: "readonly",
  URL: "readonly",
  fetch: "readonly",
  Request: "readonly",
  Response: "readonly",
  Headers: "readonly",
  AbortController: "readonly",
  performance: "readonly",
  TextDecoder: "readonly",
  TextEncoder: "readonly",
  Buffer: "readonly",
  queueMicrotask: "readonly",
};

export default tseslint.config(
  {
    // vendor/ 是从 liquidglass-oss 内联的第三方源码，按原样保留，不参与本项目 lint。
    ignores: ["dist/**", "vendor/**", "public/jassub/**"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2020,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": "off",
      // 原生 <img> 是常态(发现页海报直连 TMDB 图床、logo 是静态资源),
      // SPA 没有 next/image 那层服务端优化,放行。
      "@typescript-eslint/no-explicit-any": "off",
      // vendored 上游代码存在大量存量未用变量(Next 版配置不报),
      // 迁移期降为 warn,不做顺手清理。
      "@typescript-eslint/no-unused-vars": "warn",
    },
  },
  {
    // 构建/测试脚本跑在 Node 里,声明其全局。
    files: ["scripts/**/*.mjs", "test/**/*.mjs"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: nodeGlobals,
    },
  },
);
