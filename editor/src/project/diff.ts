/**
 * 浏览器端行级 unified diff（Myers 的 LCS 动态规划版）。
 *
 * 只用于「本地草稿 vs 服务端原文」的实时预览：数据小、无需引入额外依赖，
 * 与服务端 /commits、/compare 返回的 PrFileDiff 采用同一套展示结构。
 * 超过行数上限时直接标记 truncated，避免在主线程做 O(n*m) 的大矩阵。
 */
export type DiffRow = {
  kind: "context" | "add" | "del";
  oldLine: number | null;
  newLine: number | null;
  text: string;
};

export type DiffHunk = {
  header: string;
  rows: DiffRow[];
};

export type LocalDiff = {
  additions: number;
  deletions: number;
  truncated: boolean;
  hunks: DiffHunk[];
};

const MAX_LINES = 1800;
const CONTEXT = 3;

function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split("\n");
  if (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

type Op = { kind: DiffRow["kind"]; text: string };

/** 计算行级编辑序列。old/new 行数都受 MAX_LINES 限制。 */
function diffOps(oldText: string, newText: string): { ops: Op[]; truncated: boolean } {
  const oldLines = splitLines(oldText);
  const newLines = splitLines(newText);
  if (oldLines.length > MAX_LINES || newLines.length > MAX_LINES) {
    return { ops: [], truncated: true };
  }
  const n = oldLines.length;
  const m = newLines.length;
  // dp[i][j] = old[i:] 与 new[j:] 的 LCS 长度。
  const width = m + 1;
  const dp = new Int32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      dp[i * width + j] = oldLines[i] === newLines[j]
        ? dp[(i + 1) * width + j + 1] + 1
        : Math.max(dp[(i + 1) * width + j], dp[i * width + j + 1]);
    }
  }
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (oldLines[i] === newLines[j]) {
      ops.push({ kind: "context", text: oldLines[i] });
      i += 1;
      j += 1;
    } else if (dp[(i + 1) * width + j] >= dp[i * width + j + 1]) {
      ops.push({ kind: "del", text: oldLines[i] });
      i += 1;
    } else {
      ops.push({ kind: "add", text: newLines[j] });
      j += 1;
    }
  }
  while (i < n) {
    ops.push({ kind: "del", text: oldLines[i] });
    i += 1;
  }
  while (j < m) {
    ops.push({ kind: "add", text: newLines[j] });
    j += 1;
  }
  return { ops, truncated: false };
}

export function diffText(oldText: string, newText: string): LocalDiff {
  const { ops, truncated } = diffOps(oldText, newText);
  if (truncated) {
    return { additions: 0, deletions: 0, truncated: true, hunks: [] };
  }
  let additions = 0;
  let deletions = 0;
  for (const op of ops) {
    if (op.kind === "add") additions += 1;
    if (op.kind === "del") deletions += 1;
  }
  if (additions === 0 && deletions === 0) {
    return { additions: 0, deletions: 0, truncated: false, hunks: [] };
  }

  // 先按「变更 ± CONTEXT」切出区间，再合并重叠区间。
  const ranges: Array<[number, number]> = [];
  for (let index = 0; index < ops.length; index += 1) {
    if (ops[index].kind === "context") continue;
    const start = Math.max(0, index - CONTEXT);
    const end = Math.min(ops.length, index + CONTEXT + 1);
    const last = ranges[ranges.length - 1];
    if (last && start <= last[1]) last[1] = Math.max(last[1], end);
    else ranges.push([start, end]);
  }

  let oldLine = 1;
  let newLine = 1;
  const lineNumbers = ops.map((op) => {
    const row: DiffRow = {
      kind: op.kind,
      oldLine: op.kind === "add" ? null : oldLine,
      newLine: op.kind === "del" ? null : newLine,
      text: op.text,
    };
    if (op.kind !== "add") oldLine += 1;
    if (op.kind !== "del") newLine += 1;
    return row;
  });

  const hunks: DiffHunk[] = ranges.map(([start, end]) => {
    const rows = lineNumbers.slice(start, end);
    const oldStart = rows.find((row) => row.oldLine !== null)?.oldLine ?? 0;
    const newStart = rows.find((row) => row.newLine !== null)?.newLine ?? 0;
    const oldCount = rows.filter((row) => row.kind !== "add").length;
    const newCount = rows.filter((row) => row.kind !== "del").length;
    return {
      header: `@@ -${oldStart},${oldCount} +${newStart},${newCount} @@`,
      rows,
    };
  });
  return { additions, deletions, truncated: false, hunks };
}
