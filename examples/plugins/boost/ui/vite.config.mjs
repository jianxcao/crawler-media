import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../../../..");
const webRoot = path.join(repoRoot, "web");
const require = createRequire(import.meta.url);

const vitePkgPath = path.join(webRoot, "node_modules/vite/dist/node/index.js");
const reactPluginPath = path.join(webRoot, "node_modules/@vitejs/plugin-react/dist/index.js");
const mfPluginPath = path.join(webRoot, "node_modules/@module-federation/vite/lib/index.js");

const { defineConfig } = await import(pathToFileURL(vitePkgPath).href);
const { default: react } = await import(pathToFileURL(reactPluginPath).href);
const { federation } = await import(pathToFileURL(mfPluginPath).href);

export default defineConfig({
  root: here,
  base: "./",
  logLevel: "warn",
  resolve: {
    alias: {
      react: path.dirname(require.resolve(path.join(webRoot, "node_modules/react/package.json"))),
      "react-dom": path.dirname(require.resolve(path.join(webRoot, "node_modules/react-dom/package.json"))),
      "@crawler-media/plugin-ui-sdk": path.join(repoRoot, "sdk/plugin-ui/src/index.ts"),
      "@crawler-media/plugin-ui-kit": path.join(repoRoot, "sdk/plugin-ui-kit/src/index.tsx"),
    },
  },
  plugins: [
    react(),
    federation({
      name: "cmBuiltinBoost",
      filename: "assets/remoteEntry.js",
      exposes: {
        "./Config": "./src/Config.tsx",
        "./Page": "./src/Page.tsx",
        "./App": "./src/App.tsx",
        "./Dashboard": "./src/Dashboard.tsx",
      },
      dts: false,
      manifest: false,
      shared: {
        react: { singleton: true, requiredVersion: "19.3.0", import: false },
        "react-dom": { singleton: true, requiredVersion: "19.3.0", import: false },
        "react/jsx-runtime": { singleton: true, requiredVersion: "19.3.0", import: false },
        "@crawler-media/plugin-ui-sdk": {
          singleton: true,
          requiredVersion: "1.0.0",
          import: false,
        },
        "@crawler-media/plugin-ui-kit": {
          singleton: true,
          requiredVersion: "1.0.0",
          import: false,
        },
      },
    }),
  ],
  build: {
    outDir: "ui-dist/ui",
    emptyOutDir: true,
    target: "es2022",
    cssCodeSplit: false,
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      input: path.join(here, "src/virtual-entry.ts"),
      output: {
        assetFileNames: (info) =>
          info.name && info.name.endsWith(".css") ? "assets/style.css" : "assets/[name]-[hash][extname]",
      },
    },
  },
});
