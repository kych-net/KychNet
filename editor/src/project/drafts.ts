/**
 * 项目工作区的「本地变更」。
 *
 * 站内 Git 没有磁盘工作树：编辑器把未提交的改动存在 localStorage，
 * 按 项目 id + 分支名 分桶。切换分支不会丢掉另一分支的草稿，提交成功后
 * 只清掉本次提交的那些路径。
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

export function draftKey(projectId: string, ref: string): string {
  return `${PREFIX}${projectId}:${ref}`;
}

export function loadChanges(key: string): ChangeMap {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: ChangeMap = {};
    for (const [path, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (!value || typeof value !== "object") continue;
      const item = value as Partial<LocalChange>;
      if (
        typeof item.base === "string" &&
        typeof item.baseOid === "string" &&
        typeof item.content === "string" &&
        (item.kind === "add" || item.kind === "modify" || item.kind === "delete")
      ) {
        out[path] = {
          path,
          kind: item.kind,
          base: item.base,
          baseOid: item.baseOid,
          content: item.content,
        };
      }
    }
    return out;
  } catch {
    return {};
  }
}

export function persistChanges(key: string, changes: ChangeMap): void {
  try {
    if (Object.keys(changes).length === 0) {
      localStorage.removeItem(key);
    } else {
      localStorage.setItem(key, JSON.stringify(changes));
    }
  } catch {
    // 配额满时不阻断编辑，改动仍留在内存里。
  }
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
