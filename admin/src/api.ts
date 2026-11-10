export type User = {
  id: string;
  username: string;
  disabled: boolean;
  is_admin: boolean;
  created_at: string;
};

export type NavLink = { label: string; href: string };

export type WorkflowItem = {
  path: string;
  enabled: boolean;
  approved_at: string | null;
};

export type WorkflowRun = {
  id: string;
  workflow_path: string;
  event_kind: string;
  ref_name: string;
  status: string;
  conclusion: string | null;
  skipped_reason: string | null;
  attempt: number;
  workflow_dir: string;
  created_at: string;
  finished_at: string | null;
};

export type WorkflowRunner = {
  id: string;
  name: string;
  enabled: boolean;
  protocol_version: number;
  labels: string[];
  last_seen_at: string | null;
};


export type SiteSettings = {
  site_name: string;
  description: string;
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

export type ContentFormat = "markdown" | "html" | "typst" | "wikidot" | "css" | "svg";

export type Content = {
  id: string;
  slug: string;
  format: ContentFormat;
  body: string;
  published: boolean;
  created_at: string;
  updated_at: string;
  /** 当前版本指针：指向 content_revisions 里正在展示的版本。 */
  rev_seq: number;
};

export type ContentRevision = {
  id: string;
  content_id: string;
  seq: number;
  parent_seq: number | null;
  kind: "full" | "delta";
  slug: string;
  format: string;
  added: number;
  removed: number;
  author_id: string | null;
  author_name: string | null;
  message: string;
  byte_size: number;
  created_at: string;
  is_current: boolean;
};

/** 输出路径对应的源文件路径，用于编辑器展示。 */
export function sourcePath(item: { slug: string; format: ContentFormat }): string {
  if (item.format === "css") return item.slug;
  const ext =
    item.format === "markdown"
      ? ".md"
      : item.format === "typst"
        ? ".typ"
        : item.format === "wikidot"
          ? ".wj"
          : item.format === "svg"
            ? ".svg"
            : ".html";
  return item.slug.replace(/\.(html|svg)$/, ext);
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

export type Project = {
  id: string;
  slug: string;
  description: string;
  default_branch: string;
  created_by: string | null;
  created_at: string;
  updated_at: string;
  archived_at: string | null;
};

export type ProjectDirItem = {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
  oid: string;
};

export type ProjectFile = {
  path: string;
  oid: string;
  content: string;
};

export type GitToken = {
  id: string;
  name: string;
  created_at: string;
  expires_at: string | null;
  last_used_at: string | null;
};

export type GitSshKey = {
  id: string;
  name: string;
  fingerprint: string;
  public_key: string;
  created_at: string;
  last_used_at: string | null;
};


export type PrStatus = "open" | "merged" | "closed";

export type PullRequest = {
  id: string;
  project_id: string;
  number: number;
  title: string;
  body: string;
  source_branch: string;
  target_branch: string;
  status: PrStatus;
  created_at: string;
  updated_at: string;
  merged_at: string | null;
  author_name: string;
};

export type PrComment = {
  id: string;
  author_name: string;
  body: string;
  created_at: string;
};

export type PrDiffLine = {
  kind: "context" | "add" | "del";
  old_line: number | null;
  new_line: number | null;
  text: string;
};

export type PrDiffHunk = {
  header: string;
  lines: PrDiffLine[];
};

export type PrFileDiff = {
  path: string;
  status: "add" | "modify" | "delete";
  old_oid?: string | null;
  new_oid?: string | null;
  additions: number;
  deletions: number;
  /** 文件过大或不是文本时为 true，此时 hunks 为空。 */
  truncated: boolean;
  hunks: PrDiffHunk[];
};

export type PrDetail = PullRequest & {
  files: PrFileDiff[];
  mergeable: boolean;
  /** 目标分支是否已经是来源分支的祖先。 */
  is_fast_forward: boolean;
  /** 保留兼容旧后端；新后端恒为 false，不再覆盖目标分支。 */
  overwrites_target: boolean;
  /** 两侧相对合并基点都有不同改动的文件；非空时禁止合并。 */
  conflicts: string[];
  merge_base: string | null;
  comments: PrComment[];
};

export type CommitSummary = {
  oid: string;
  parent_oid: string | null;
  second_parent_oid: string | null;
  tree_oid: string;
  author_name: string;
  message: string;
  committed_at: string;
};

export type CommitDetail = CommitSummary & {
  files: PrFileDiff[];
};

export type Discussion = {
  id: string;
  project_id: string;
  title: string;
  body: string;
  author_name: string;
  created_at: string;
  closed_at: string | null;
  comment_count: number;
};

export type DiscussionComment = {
  id: string;
  author_name: string;
  body: string;
  created_at: string;
};

export type DiscussionDetail = Discussion & {
  comments: DiscussionComment[];
};

// API 基址：开发环境留空，请求走 vite 的 /api 代理（见 vite.config.ts）；
// 生产环境把 VITE_API_BASE_URL 设为 API 服务的完整 origin（如 https://kych.net）。
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

/** 公开的 Git 仓库地址；项目名按 URL 段编码。 */
export function gitUrl(slug: string): string {
  // Git 用 URL 最后一段作为默认克隆目录名；这里保留原始 UTF-8 项目名，
  // 否则中文项目会被克隆成 %E5%... 目录。服务端本来就会解码路径。
  return `${API_BASE}/git/${slug}.git`;
}

/** Git over SSH 地址；端口固定 2222，host 跟随 API 基址。 */
export function sshGitUrl(slug: string): string {
  let host = "kych.net";
  if (API_BASE) {
    try {
      host = new URL(API_BASE).host;
    } catch {
      // 配置异常时保留默认站名，至少地址可读。
    }
  }
  return `ssh://git@${host}:2222/${slug}.git`;
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
    format: ContentFormat;
    body: string;
    published: boolean;
    message?: string;
  }) =>
    request<{ item: Content }>("/contents", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  deleteContent: (id: string) => request<void>(`/contents/${id}`, { method: "DELETE" }),
  revisions: (id: string) =>
    request<{ items: ContentRevision[]; current_seq: number }>(`/contents/${id}/revisions`),
  revisionBody: (id: string, seq: number) =>
    request<{ seq: number; format: string; body: string }>(`/contents/${id}/revisions/${seq}`),
  revisionDiff: (id: string, seq: number, against?: number) =>
    request<{ diff: string }>(
      `/contents/${id}/revisions/${seq}/diff${against ? `?against=${against}` : ""}`,
    ),
  revertRevision: (id: string, seq: number) =>
    request<{ item: Content }>(`/contents/${id}/revisions/${seq}/revert`, { method: "POST" }),
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

  projects: (includeArchived = false) =>
    request<{ items: Project[] }>(
      `/projects${includeArchived ? "?include_archived=true" : ""}`,
    ),
  createProject: (payload: { slug: string; description: string }) =>
    request<{ item: Project }>("/projects", {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  project: (id: string) =>
    request<{ item: Project; branches: string[] }>(`/projects/${id}`),
  updateProject: (id: string, payload: { description?: string; archived?: boolean }) =>
    request<{ item: Project }>(`/projects/${id}`, {
      method: "PATCH",
      body: JSON.stringify(payload),
    }),
  deleteProject: (id: string) =>
    request<void>(`/projects/${id}`, { method: "DELETE" }),
  projectTree: (id: string, ref: string, path: string) =>
    request<{ items: ProjectDirItem[]; branches: string[]; default_branch: string }>(
      `/projects/${id}/tree?ref=${encodeURIComponent(ref)}&path=${encodeURIComponent(path)}`,
    ),
  projectFile: (id: string, ref: string, path: string) =>
    request<{ item: ProjectFile | null }>(
      `/projects/${id}/file?ref=${encodeURIComponent(ref)}&path=${encodeURIComponent(path)}`,
    ),
  saveProjectFile: (
    id: string,
    payload: {
      ref: string;
      path: string;
      content: string;
      message?: string;
      base_oid?: string;
    },
  ) =>
    request<{ item: { commit: string; path: string; oid: string } }>(`/projects/${id}/file`, {
      method: "PUT",
      body: JSON.stringify(payload),
    }),
  deleteProjectFile: (id: string, ref: string, path: string) =>
    request<void>(
      `/projects/${id}/file?ref=${encodeURIComponent(ref)}&path=${encodeURIComponent(path)}`,
      { method: "DELETE" },
    ),
  moveProjectFile: (
    id: string,
    payload: { ref: string; from: string; to: string; message?: string },
  ) =>
    request<{ item: { commit: string; path: string; oid: string } }>(
      `/projects/${id}/move`,
      { method: "POST", body: JSON.stringify(payload) },
    ),

  commitProjectChanges: (
    id: string,
    payload: {
      ref: string;
      message?: string;
      changes: Array<{ path: string; content?: string; delete?: boolean }>;
    },
  ) =>
    request<{ item: { commit: string; paths: string[] } }>(`/projects/${id}/commit`, {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  commits: (id: string, ref?: string, limit = 50, offset = 0) =>
    request<{ items: CommitSummary[] }>(
      `/projects/${id}/commits?limit=${limit}&offset=${offset}` +
        (ref ? `&ref=${encodeURIComponent(ref)}` : ""),
    ),
  commitDetail: (id: string, oid: string) =>
    request<{ item: CommitDetail }>(`/projects/${id}/commits/${oid}`),
  revertCommit: (id: string, payload: { ref: string; commit: string; message?: string }) =>
    request<{ item: { commit: string } }>(`/projects/${id}/revert`, {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  createBranch: (id: string, payload: { name: string; from?: string }) =>
    request<{ branches: string[] }>(`/projects/${id}/branches`, {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  deleteBranch: (id: string, name: string) =>
    request<{ branches: string[] }>(
      `/projects/${id}/branches/${encodeURIComponent(name)}`,
      { method: "DELETE" },
    ),

  prs: (id: string) => request<{ items: PullRequest[] }>(`/projects/${id}/prs`),
  createPr: (
    id: string,
    payload: { title: string; body: string; source_branch: string; target_branch: string },
  ) =>
    request<{ item: PullRequest }>(`/projects/${id}/prs`, {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  prDetail: (id: string, number: number) =>
    request<{ item: PrDetail }>(`/projects/${id}/prs/${number}`),
  addPrComment: (id: string, number: number, body: string) =>
    request<{ item: PrComment }>(`/projects/${id}/prs/${number}/comments`, {
      method: "POST",
      body: JSON.stringify({ body }),
    }),
  mergePr: (id: string, number: number) =>
    request<{ item: PullRequest; overwrote_target: boolean }>(
      `/projects/${id}/prs/${number}/merge`,
      { method: "POST" },
    ),
  closePr: (id: string, number: number) =>
    request<{ item: PullRequest }>(`/projects/${id}/prs/${number}/close`, { method: "POST" }),

  discussions: (id: string) =>
    request<{ items: Discussion[] }>(`/projects/${id}/discussions`),
  createDiscussion: (id: string, payload: { title: string; body: string }) =>
    request<{ item: Discussion }>(`/projects/${id}/discussions`, {
      method: "POST",
      body: JSON.stringify(payload),
    }),
  discussionDetail: (id: string, discussionId: string) =>
    request<{ item: DiscussionDetail }>(`/projects/${id}/discussions/${discussionId}`),
  addDiscussionComment: (id: string, discussionId: string, body: string) =>
    request<{ item: DiscussionComment }>(
      `/projects/${id}/discussions/${discussionId}/comments`,
      { method: "POST", body: JSON.stringify({ body }) },
    ),
  closeDiscussion: (id: string, discussionId: string) =>
    request<{ item: Discussion }>(
      `/projects/${id}/discussions/${discussionId}/close`,
      { method: "POST" },
    ),

  gitTokens: () => request<{ items: GitToken[] }>("/git-tokens"),
  createGitToken: (name: string) =>
    request<{ item: GitToken; token: string }>("/git-tokens", {
      method: "POST",
      body: JSON.stringify({ name }),
    }),
  revokeGitToken: (id: string) =>
    request<void>(`/git-tokens/${id}`, { method: "DELETE" }),
  sshKeys: () => request<{ items: GitSshKey[] }>("/ssh-keys"),
  createSshKey: (name: string, publicKey: string) =>
    request<{ item: GitSshKey }>("/ssh-keys", {
      method: "POST",
      body: JSON.stringify({ name, public_key: publicKey }),
    }),
  revokeSshKey: (id: string) =>
    request<void>(`/ssh-keys/${id}`, { method: "DELETE" }),
  projectWorkflows: (id: string) =>
    request<{ workflow_dir: string; default_branch: string; items: WorkflowItem[] }>(
      `/projects/${id}/workflows`,
    ),
  setProjectWorkflow: (id: string, path: string, enabled: boolean) =>
    request<{ path: string; enabled: boolean }>(`/projects/${id}/workflows`, {
      method: "PUT",
      body: JSON.stringify({ path, enabled }),
    }),
  dispatchProjectWorkflow: (id: string) =>
    request<{ queued: boolean }>(`/projects/${id}/workflows/dispatch`, {
      method: "POST",
      body: JSON.stringify({ inputs: {} }),
    }),
  projectRuns: (id: string) => request<{ items: WorkflowRun[] }>(`/projects/${id}/runs`),
  runners: () => request<{ items: WorkflowRunner[] }>("/runners"),
  createRunner: (name: string, labels: string[]) =>
    request<{ runner_id: string; registration_code: string }>("/runners", {
      method: "POST",
      body: JSON.stringify({ name, labels }),
    }),
  setRunnerEnabled: (id: string, enabled: boolean) =>
    request<{ id: string; enabled: boolean }>(`/runners/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
    }),

};
