import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 仅开发环境使用：把 /api 代理到本地后端。
// 生产环境没有代理，前端通过 VITE_API_BASE_URL 直接请求 API 子域（见 .env.example）。
// 代理只改 Host、保留 Origin，因此后端的管理端 Origin 校验在开发时同样能通过。
// 管理端与内容站同域，挂在 /管理/ 子路径下（nginx 用 alias 指到 /opt/kych-admin）。
// 资源路径必须带这个前缀，否则会去请求内容站的 /assets/。
//
// 中文路径的实际链路：浏览器把 /管理/assets/x.js 编码成
// /%E7%AE%A1%E7%90%86/assets/x.js 发出，nginx 解码后再匹配 location，
// 所以 nginx 侧照常写中文即可，无需在配置里写百分号编码。
const BASE_PATH = process.env.VITE_BASE_PATH ?? "/管理/";

export default defineConfig({
  plugins: [react()],
  base: BASE_PATH,
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
