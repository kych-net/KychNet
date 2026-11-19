import type { ContentFormat } from "@admin/api";
import type {
  BlameResult,
  BranchCompare,
  CommitDetail,
  CommitSummary,
  FileHistoryEntry,
  MergeResult,
  PrDetail,
  PrFileDiff,
  Project,
  ProjectDirItem,
  ProjectFile,
  ProjectRefs,
  PullRequest,
  ResetResult,
  TagInfo,
} from "@admin/api";

export type {
  BlameResult,
  BranchCompare,
  CommitDetail,
  CommitSummary,
  FileHistoryEntry,
  MergeResult,
  PrDetail,
  PrFileDiff,
  Project,
  ProjectDirItem,
  ProjectFile,
  ProjectRefs,
  PullRequest,
  ResetResult,
  TagInfo,
};

/** 按扩展名给 CodeMirror 选高亮；未知格式退化为纯文本（用 markdown 占位）。 */
export function formatForPath(path: string): ContentFormat {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "md":
    case "markdown":
      return "markdown";
    case "html":
    case "htm":
    case "vue":
    case "xml":
      return "html";
    case "svg":
      return "svg";
    case "css":
      return "css";
    case "typ":
      return "typst";
    case "wj":
      return "wikidot";
    default:
      return "markdown";
  }
}

export function shortOid(oid: string): string {
  return oid.slice(0, 7);
}

export function formatTime(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function firstLine(value: string): string {
  return value.split("\n")[0] || "(无提交信息)";
}

export function errText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function joinPath(dir: string, name: string): string {
  const clean = name.trim().replace(/^\/+/, "");
  return dir ? `${dir}/${clean}` : clean;
}

export function parentPath(path: string): string {
  const parts = path.split("/");
  parts.pop();
  return parts.join("/");
}

export function pathName(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** 复制文本到剪贴板，失败时回退到旧的 execCommand。 */
export async function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const area = document.createElement("textarea");
  area.value = value;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  const ok = document.execCommand("copy");
  document.body.removeChild(area);
  if (!ok) throw new Error("复制失败，请手动选择文本");
}
