import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "node:path";

// 独立轻量编辑器，挂在 /编辑器/（中文路径，nginx alias 到构建产物）。
// 复用管理端的 API 客户端与 CodeEditor 组件（@admin 别名），不重复实现。
const BASE = process.env.VITE_BASE_PATH ?? "/编辑器/";

// 共享依赖去重：CodeEditor 从 ../admin/src 引入，bare import 统一解析到
// 本工程的 node_modules，避免 React/CodeMirror 出现两份实例。
const shared = [
  "react",
  "react-dom",
  "@codemirror/state",
  "@codemirror/view",
  "@codemirror/language",
  "@codemirror/autocomplete",
  "@codemirror/commands",
  "@codemirror/lang-markdown",
  "@codemirror/lang-html",
  "@codemirror/lang-css",
  "@lezer/highlight",
  "codemirror",
];

// 显式把每个共享包别名到本工程 node_modules：跨项目引入 CodeEditor 时，
// 仅靠 dedupe 仍可能让 dev 预打包产生第二份实例（补全 facet 不一致会静默失效）。
const alias: Record<string, string> = {
  "@admin": path.resolve(__dirname, "../admin/src"),
};
for (const pkg of shared) {
  alias[pkg] = path.resolve(__dirname, "node_modules", pkg);
}

export default defineConfig({
  plugins: [react()],
  base: BASE,
  resolve: {
    alias,
    dedupe: shared,
  },
  optimizeDeps: {
    include: shared.filter((p) => p !== "react" && p !== "react-dom"),
  },
  server: {
    port: 5174,
    proxy: {
      "/api": {
        target: process.env.VITE_PROXY_TARGET ?? "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
  build: {
    chunkSizeWarningLimit: 700,
  },
});
