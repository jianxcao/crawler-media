import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

/**
 * 开发:Vite dev 起在 3334,API 走 `VITE_API_BASE_URL` 直连后端
 * (如 http://127.0.0.1:18765/api/v1),跨域由后端 CORS 层放行,这里不配代理。
 * 生产:`vite build` 产出 dist/ 静态文件,由 Rust 二进制托管
 * (CRAWLER_MEDIA_UI 指向 dist,单端口同源,天然无跨域问题)。
 */
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "."),
    },
  },
  server: {
    host: "127.0.0.1",
    port: 3334,
    strictPort: true,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:18765",
        changeOrigin: true,
      },
      "/posters": {
        target: "http://127.0.0.1:18765",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
  },
  // jassub 的 dist 里有 `new Worker(new URL(...))` 兜底,Vite 会把它当 worker
  // 入口打包;默认 iife 格式不支持分包,必须用 es 格式(运行时实际走我们传入的
  // /jassub/ 独立 URL,这条兜底只在显式 workerUrl 缺失时才会被真正构造)。
  worker: {
    format: "es",
  },
});
