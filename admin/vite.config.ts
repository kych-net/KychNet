import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 仅开发环境使用：把 /api 代理到本地后端。
// 生产环境没有代理，前端通过 VITE_API_BASE_URL 直接请求 API 子域（见 .env.example）。
// 代理只改 Host、保留 Origin，因此后端的管理端 Origin 校验在开发时同样能通过。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: process.env.VITE_PROXY_TARGET ?? "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
});
