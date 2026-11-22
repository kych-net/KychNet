/**
 * 项目工作区的「本地变更」。
 *
 * 站内 Git 没有磁盘工作树：编辑器把未提交的改动存在 localStorage，
 * 按 项目 id + 分支名 分桶。切换分支不会丢掉另一分支的草稿，提交成功后
 * 只清掉本次提交的那些路径。
 *
 * 存储布局（一个文件一个 key，避免每次按键都全量 stringify 整个工作区）：
 *   kychnet-editor-project:<project>:<ref>               → 该桶的路径索引（JSON 数组）
 *   kychnet-editor-project:<project>:<ref>:draft:<path>  → 单个 LocalChange（JSON 对象）
 * 写入按 300ms 节流合并。
 */
export type LocalChange = {
  path: string;
  kind: "add" | "modify" | "delete";
  /** 服务端原始内容，用于算 diff 和撤销。 */
  base: string;
  /** 服务端 blob OID，提交时做乐观锁。 */
  baseOid: string;
  /** 删除时为空串。 */
  content: string;
};

export type ChangeMap = Record<string, LocalChange>;

const PREFIX = "kychnet-editor-project:";
const DRAFT_MARK = ":draft:";
const PERSIST_DELAY = 300;

/** 桶的基准 key（索引也存这里，兼容旧的「整表 JSON」格式）。 */
export function draftKey(projectId: string, ref: string): string {
  return `${PREFIX}${projectId}:${ref}`;
}

function fileKey(base: string, path: string): string {
  return `${base}${DRAFT_MARK}${path}`;
}

function parseChange(path: string, value: unknown): LocalChange | null {
  if (!value || typeof value !== "object") return null;
  const item = value as Partial<LocalChange>;
  if (
    typeof item.base === "string" &&
    typeof item.baseOid === "string" &&
    typeof item.content === "string" &&
    (item.kind === "add" || item.kind === "modify" || item.kind === "delete")
  ) {
    return {
      path,
      kind: item.kind,
      base: item.base,
      baseOid: item.baseOid,
      content: item.content,
    };
  }
  return null;
}

function parseMap(raw: string | null): ChangeMap {
  try {
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: ChangeMap = {};
    for (const [path, value] of Object.entries(parsed as Record<string, unknown>)) {
      const change = parseChange(path, value);
      if (change) out[path] = change;
    }
    return out;
  } catch {
    return {};
  }
}

// 每个桶上次落盘的原文，用来算「哪些文件变了」，避免无谓的写入。
const written = new Map<string, Map<string, string>>();

export function loadChanges(base: string): ChangeMap {
  let raw: string | null = null;
  try {
    raw = localStorage.getItem(base);
  } catch {
    return {};
  }
  let paths: string[] | null = null;
  try {
    const parsed: unknown = raw ? JSON.parse(raw) : null;
    if (Array.isArray(parsed) && parsed.every((item) => typeof item === "string")) {
      paths = parsed as string[];
    }
  } catch {
    paths = null;
  }

  // 旧版本把整个 ChangeMap 存在桶 key 上：读出来迁移成按文件分 key。
  if (paths === null) {
    const legacy = parseMap(raw);
    const next = new Map<string, string>();
    const out: ChangeMap = {};
    for (const [path, change] of Object.entries(legacy)) {
      const json = JSON.stringify(change);
      next.set(path, json);
      out[path] = change;
      try {
        localStorage.setItem(fileKey(base, path), json);
      } catch {
        /* 配额满时仅保留内存中的改动 */
      }
    }
    written.set(base, next);
    if (Object.keys(out).length === 0) {
      if (raw !== null) {
        try {
          localStorage.removeItem(base);
        } catch {
          /* 忽略 */
        }
      }
    } else {
      // 迁移完成：桶 key 改存路径索引。
      try {
        localStorage.setItem(base, JSON.stringify([...next.keys()]));
      } catch {
        /* 配额满时下次再迁移 */
      }
    }
    return out;
  }

  const out: ChangeMap = {};
  const snapshot = new Map<string, string>();
  for (const path of paths) {
    let item: string | null = null;
    try {
      item = localStorage.getItem(fileKey(base, path));
    } catch {
      item = null;
    }
    if (!item) continue;
    let change: LocalChange | null = null;
    try {
      change = parseChange(path, JSON.parse(item));
    } catch {
      change = null;
    }
    if (!change) continue;
    out[path] = change;
    snapshot.set(path, item);
  }
  written.set(base, snapshot);
  return out;
}

function writeNow(base: string, changes: ChangeMap): void {
  const previous = written.get(base) ?? new Map<string, string>();
  const next = new Map<string, string>();
  try {
    for (const [path, change] of Object.entries(changes)) {
      const json = JSON.stringify(change);
      next.set(path, json);
      if (previous.get(path) !== json) localStorage.setItem(fileKey(base, path), json);
    }
    for (const path of previous.keys()) {
      if (!next.has(path)) localStorage.removeItem(fileKey(base, path));
    }
    if (next.size === 0) localStorage.removeItem(base);
    else localStorage.setItem(base, JSON.stringify([...next.keys()]));
    written.set(base, next);
  } catch {
    // 配额满时不阻断编辑，改动仍留在内存里。
  }
}

let pendingWrite: { base: string; changes: ChangeMap } | null = null;
let persistTimer: number | null = null;

/**
 * 按 300ms 节流落盘：连续输入只写一次。
 * 桶 key 随任务一起记录，切换分支时不会把草稿写进新分支。
 */
export function persistChanges(base: string, changes: ChangeMap): void {
  pendingWrite = { base, changes };
  if (typeof window === "undefined" || persistTimer !== null) return;
  persistTimer = window.setTimeout(() => {
    persistTimer = null;
    flushPersist();
  }, PERSIST_DELAY);
}

/** 立即写盘（页面卸载前、切换分支前调用）。 */
export function flushPersist(): void {
  if (typeof window !== "undefined" && persistTimer !== null) {
    window.clearTimeout(persistTimer);
    persistTimer = null;
  }
  const job = pendingWrite;
  pendingWrite = null;
  if (job) writeNow(job.base, job.changes);
}

/** 与服务端原文一致时不再算作变更。 */
export function normalizeChange(change: LocalChange): LocalChange | null {
  if (change.kind === "delete") return change;
  // 新建文件还没写内容时不算变更；已有文件改回原文也不算。
  if (change.kind === "add" && change.content === "") return null;
  if (change.content === change.base && change.kind === "modify") return null;
  return change;
}

export function changeStats(changes: ChangeMap): { modified: number; added: number; deleted: number } {
  let modified = 0;
  let added = 0;
  let deleted = 0;
  for (const change of Object.values(changes)) {
    if (change.kind === "delete") deleted += 1;
    else if (change.kind === "add") added += 1;
    else modified += 1;
  }
  return { modified, added, deleted };
}
