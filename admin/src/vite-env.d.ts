/// <reference types="vite/client" />

// 项目用到的 Vite 环境变量。之前缺少 vite/client 类型，
// 导致在 api.ts 里访问 import.meta.env 时 tsc 报 TS2339。
interface ImportMetaEnv {
  /** API 服务完整 origin；留空时走开发代理 */
  readonly VITE_API_BASE_URL?: string;
  /** Git over SSH 的对外主机，默认 git.kych.net（SSH 不走 CDN） */
  readonly VITE_GIT_SSH_HOST?: string;
  /** Git over SSH 的对外端口，默认 2222 */
  readonly VITE_GIT_SSH_PORT?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
