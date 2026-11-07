export type User = {
  id: string;
  username: string;
  disabled: boolean;
  is_admin: boolean;
  created_at: string;
};

export type NavLink = { label: string; href: string };

export type SiteSettings = {
  site_name: string;
  logo_url: string;
  nav: NavLink[];
};

export type Invite = {
  code: string;
  created_by: string | null;
  max_uses: number;
  used_count: number;
  expires_at: string | null;
  revoked_at: string | null;
  created_at: string;
};

export type ContentFormat = "markdown" | "html" | "typst" | "wikidot" | "css";

export type Content = {
  id: string;
  slug: string;
  title: string;
  format: ContentFormat;
  body: string;
  published: boolean;
  created_at: string;
  updated_at: string;
};

/** 输出路径对应的源文件路径，用于编辑器展示。 */
export function sourcePath(item: { slug: string; format: ContentFormat }): string {
  if (item.format === "css") return item.slug;
  const ext =
    item.format === "markdown" ? ".md" : item.format === "typst" ? ".typ" : item.format === "wikidot" ? ".wj" : ".html";
  return item.slug.replace(/\.html$/, ext);
}

/** 编辑器里显示的绝对路径，例如 /eoe/index.md。 */
export function editorPath(item: { slug: string; format: ContentFormat }): string {
  return `/${sourcePath(item)}`;
}

export type BuildRecord = {
  started_at: number;
  finished_at: number | null;
  exit_code: number | null;
  tail: string;
};

export type BuildStatus = {
  running: boolean;
  last: BuildRecord | null;
};

// API 基址：开发环境留空，请求走 vite 的 /api 代理（见 vite.config.ts）；
// 生产环境把 VITE_API_BASE_URL 设为 API 服务的完整 origin（如 https://api.kych.net）。
// 管理前端是独立发布的静态站，本身没有同源 /api 可以转发，必须显式指向 API 子域。
const API_BASE = (import.meta.env.VITE_API_BASE_URL ?? "").replace(/\/+$/, "");

function apiUrl(path: string): string {
  return `${API_BASE}/api${path}`;
}

/** 头像地址同样要跟随 API 基址，否则生产环境下头像会 404。 */
export function avatarUrl(id: string, version?: number): string {
  const suffix = version === undefined ? "" : `?v=${version}`;
  return `${apiUrl(`/avatar/${id}`)}${suffix}`;
}

export class ApiError extends Error {
  status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export function isStepUpRequired(error: unknown): boolean {
  return error instanceof ApiError && error.status === 403;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(apiUrl(path), {
    credentials: "include",
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    ...init,
  });
  if (!response.ok) {
    const data = await response.json().catch(() => null);
    throw new ApiError(data?.error ?? response.statusText, response.status);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

export const api = {
  me: () => request<User | null>("/me"),
  uploadAvatar: async (file: File) => {
    const response = await fetch(apiUrl("/me/avatar"), {
      method: "POST",
      credentials: "include",
      headers: { "Content-Type": "image/svg+xml" },
      body: await file.text(),
    });
    if (!response.ok) {
      const data = await response.json().catch(() => null);
      throw new Error(data?.error ?? response.statusText);
    }
    return (await response.json()) as User;
  },
  logout: () => request<void>("/auth/logout", { method: "POST" }),

  register: (code: string, username: string) =>
    request<{ user: User; flow_id: string; options: any }>("/auth/register", {
      method: "POST",
      body: JSON.stringify({ code, username }),
    }),
  finishRegister: (flowId: string, credential: any) =>
    request<User>("/auth/register/finish", {
      method: "POST",
      body: JSON.stringify({ flow_id: flowId, credential }),
    }),

  loginStart: (username: string) =>
    request<{ flow_id: string; options: any }>("/auth/login/start", {
      method: "POST",
      body: JSON.stringify({ username }),
    }),
  loginFinish: (flowId: string, credential: any) =>
    request<User>("/auth/login/finish", {
      method: "POST",
      body: JSON.stringify({ flow_id: flowId, credential }),
    }),
  totpLogin: (username: string, code: string) =>
    request<User>("/auth/totp/login", {
      method: "POST",
      body: JSON.stringify({ username, code }),
    }),

  stepUpTotp: (code: string) =>
    request<{ ok: boolean }>("/auth/step-up/totp", {
      method: "POST",
      body: JSON.stringify({ code }),
    }),
  stepUpPasskeyStart: () =>
    request<{ flow_id: string; options: any }>("/auth/step-up/passkey/start", {
      method: "POST",
    }),
  stepUpPasskeyFinish: (flowId: string, credential: any) =>
    request<{ ok: boolean }>("/auth/step-up/passkey/finish", {
      method: "POST",
      body: JSON.stringify({ flow_id: flowId, credential }),
    }),

  totpSetup: () =>
    request<{ otpauth_url: string; secret: string }>("/totp/setup", { method: "POST" }),
  totpConfirm: (code: string) =>
    request<{ ok: boolean }>("/totp/confirm", {
      method: "POST",
      body: JSON.stringify({ code }),
    }),
  passkeySetup: () =>
    request<{ flow_id: string; options: any }>("/passkey/setup", { method: "POST" }),
  passkeySetupFinish: (flowId: string, credential: any) =>
    request<void>("/passkey/setup/finish", {
      method: "POST",
      body: JSON.stringify({ flow_id: flowId, credential }),
    }),

  contents: () => request<{ items: Content[] }>("/contents"),
  saveContent: (payload: {
    id?: string;
    slug: string;
    title: string;
    format: ContentFormat;
    body: string;
    published: boolean;
  }) =>
    request<{ item: Content }>("/contents", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  deleteContent: (id: string) => request<void>(`/contents/${id}`, { method: "DELETE" }),
  previewContent: (format: ContentFormat, body: string) =>
    request<{ html: string }>("/preview", {
      method: "POST",
      body: JSON.stringify({ format, body }),
    }),

  invites: () => request<{ items: Invite[] }>("/invites"),
  createInvite: (maxUses: number, expiresInDays: number) =>
    request<{ item: Invite }>("/invites", {
      method: "POST",
      body: JSON.stringify({ max_uses: maxUses, expires_in_days: expiresInDays }),
    }),
  revokeInvite: (code: string) => request<void>(`/invites/${code}`, { method: "DELETE" }),

  users: () => request<{ items: User[] }>("/users"),
  setDisabled: (id: string, disabled: boolean) =>
    request<void>(`/users/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ disabled }),
    }),

  getSettings: () =>
    request<{ site: SiteSettings; site_url: string }>("/settings"),
  saveSettings: (site: SiteSettings) =>
    request<{ site: SiteSettings }>("/settings", {
      method: "PUT",
      body: JSON.stringify(site),
    }),

  buildStatus: () => request<BuildStatus>("/admin/rebuild"),
  rebuild: () => request<BuildStatus>("/admin/rebuild", { method: "POST" }),
};
