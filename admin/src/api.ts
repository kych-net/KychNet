export type User = {
  id: string;
  username: string;
  display_name: string;
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

export type ContentFormat = "markdown" | "html" | "typst";

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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api${path}`, {
    credentials: "include",
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    ...init,
  });
  if (!response.ok) {
    const data = await response.json().catch(() => null);
    throw new Error(data?.error ?? response.statusText);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

export const api = {
  me: () => request<User | null>("/me"),
  updateProfile: (displayName: string) =>
    request<User>("/me", {
      method: "PATCH",
      body: JSON.stringify({ display_name: displayName }),
    }),
  logout: () => request<void>("/auth/logout", { method: "POST" }),

  register: (code: string, username: string, displayName: string) =>
    request<{ user: User; flow_id: string; options: any }>("/auth/register", {
      method: "POST",
      body: JSON.stringify({ code, username, display_name: displayName }),
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
};
