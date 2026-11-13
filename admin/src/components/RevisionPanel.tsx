import { useCallback, useEffect, useRef, useState } from "react";
import { api, Content, ContentRevision } from "../api";

type Mode =
  | { kind: "list" }
  | { kind: "diff"; from: number; to: number }
  | { kind: "show"; seq: number }
  | { kind: "blame"; seq: number };

type Snapshot = { seq: number; format: string; body: string };
type Blame = {
  lines: string[];
  origins: number[];
  truncated: boolean;
  from: number;
};

/** 版本快照下载时按格式还原源文件扩展名。 */
const EXT: Record<string, string> = {
  markdown: "md",
  html: "html",
  typst: "typ",
  wikidot: "wj",
  css: "css",
  svg: "svg",
};

/** 只按 \n 切行，去掉末尾空行，避免展示时多出一行空白。 */
function splitBody(text: string): string[] {
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

/**
 * 行级 LCS：返回新文本每一行来自旧文本的行号，-1 表示这一行是新增。
 * 用于前端逐行追溯（blame），无需后端参与。
 */
function lineOrigins(oldLines: string[], newLines: string[]): number[] {
  const n = oldLines.length;
  const m = newLines.length;
  if ((n + 1) * (m + 1) > 4_000_001) {
    throw new Error("版本行数过多，无法逐行追溯");
  }
  const width = m + 1;
  const dp = new Uint32Array((n + 1) * width);
  const at = (i: number, j: number) => i * width + j;
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[at(i, j)] =
        oldLines[i] === newLines[j]
          ? dp[at(i + 1, j + 1)] + 1
          : Math.max(dp[at(i + 1, j)], dp[at(i, j + 1)]);
    }
  }
  const origins = new Array<number>(m).fill(-1);
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (oldLines[i] === newLines[j]) {
      origins[j] = i;
      i++;
      j++;
    } else if (dp[at(i + 1, j)] >= dp[at(i, j + 1)]) {
      i++;
    } else {
      j++;
    }
  }
  return origins;
}

/** 逐版本回放，标注目标版本每行最后由哪个版本引入（git blame 等价物）。 */
async function buildBlame(
  contentId: string,
  items: ContentRevision[],
  target: number,
): Promise<Blame> {
  const chain = items
    .filter((item) => item.seq <= target)
    .sort((a, b) => a.seq - b.seq);
  const window = chain.slice(-30);
  const bodies = await Promise.all(
    window.map((item) =>
      api.revisionBody(contentId, item.seq).then((result) => result.body),
    ),
  );
  let lines: string[] = [];
  let origins: number[] = [];
  window.forEach((item, index) => {
    const next = splitBody(bodies[index]);
    if (index === 0) {
      lines = next;
      origins = next.map(() => item.seq);
    } else {
      const map = lineOrigins(lines, next);
      origins = map.map((oldIndex) =>
        oldIndex < 0 ? item.seq : (origins[oldIndex] ?? item.seq),
      );
      lines = next;
    }
  });
  return {
    lines,
    origins,
    truncated: chain.length > window.length,
    from: window[0]?.seq ?? target,
  };
}

/** 把 unified diff 按行染色。后端用 similar 输出标准 unified diff。 */
function DiffView({ diff }: { diff: string }) {
  if (!diff) return <p className="hint">两个版本内容相同。</p>;
  const lines = diff.split("\n");
  return (
    <pre className="revision-diff">
      {lines.map((line, index) => {
        const cls = line.startsWith("+")
          ? "diff-add"
          : line.startsWith("-")
            ? "diff-del"
            : line.startsWith("@@")
              ? "diff-hunk"
              : line.startsWith("---") || line.startsWith("+++")
                ? "diff-head"
                : "";
        return (
          <span key={index} className={cls}>
            {line || " "}
            {"\n"}
          </span>
        );
      })}
    </pre>
  );
}

/**
 * 内容页右栏的完整版本控制面板：
 * log（提交历史）/ diff（任意两版本对比）/ show（版本快照）/ blame（逐行追溯）/
 * revert（回滚指针）/ 快照复制与下载。全部复用现有后端接口，不改动服务端。
 */
export function RevisionPanel({
  contentId,
  refreshKey = 0,
  onReverted,
}: {
  contentId: string | null;
  refreshKey?: number;
  onReverted: (item: Content) => void;
}) {
  const [items, setItems] = useState<ContentRevision[]>([]);
  const [currentSeq, setCurrentSeq] = useState<number | null>(null);
  const [mode, setMode] = useState<Mode>({ kind: "list" });
  const [base, setBase] = useState<number | "">("");
  const [target, setTarget] = useState<number | "">("");
  const [diff, setDiff] = useState("");
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const [blame, setBlame] = useState<Blame | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");

  // 代际守卫：快速切换内容时递增代际号，响应回来比对，
  // 不一致就丢弃，避免陈旧列表覆盖新列表（进而回滚错版本）。
  const loadGen = useRef(0);

  const load = useCallback(async (id: string) => {
    const gen = ++loadGen.current;
    try {
      const result = await api.revisions(id);
      if (gen !== loadGen.current) return;
      setItems(result.items);
      setCurrentSeq(result.current_seq);
      setBase(result.current_seq);
      setTarget(result.items[0]?.seq ?? "");
    } catch (e) {
      // 已被更新的加载取代时，陈旧请求的错误也一并丢弃。
      if (gen === loadGen.current) throw e;
    }
  }, []);

  useEffect(() => {
    setMode({ kind: "list" });
    setError("");
    setNotice("");
    setExpanded(false);
    if (contentId) {
      void load(contentId).catch((e) => {
        setItems([]);
        setError((e as Error).message);
      });
    } else {
      // 清空选择时让仍在途的加载作废，响应回来不再写入。
      loadGen.current += 1;
      setItems([]);
      setCurrentSeq(null);
      setBase("");
      setTarget("");
    }
  }, [contentId, refreshKey, load]);

  if (!contentId) {
    return (
      <div className="revision-panel">
        <div className="revision-head">
          <strong>版本历史</strong>
        </div>
        <p className="hint">先选择或新建一篇内容。</p>
      </div>
    );
  }

  async function runDiff(from: number, to: number) {
    setBusy(true);
    setBusyLabel("生成 diff…");
    try {
      const result = await api.revisionDiff(contentId!, to, from);
      setDiff(result.diff);
      setMode({ kind: "diff", from, to });
      setError("");
      setNotice("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function showSnapshot(seq: number) {
    setBusy(true);
    setBusyLabel("读取快照…");
    try {
      const result = await api.revisionBody(contentId!, seq);
      setSnapshot(result);
      setMode({ kind: "show", seq });
      setError("");
      setNotice("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function showBlame(seq: number) {
    setBusy(true);
    setBusyLabel("重建版本链…");
    try {
      const result = await buildBlame(contentId!, items, seq);
      setBlame(result);
      setMode({ kind: "blame", seq });
      setError("");
      setNotice("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function revert(seq: number) {
    if (!window.confirm(`回滚到版本 ${seq}？指针将直接移动，之后的版本仍保留在历史中。`)) return;
    setBusy(true);
    setBusyLabel("回滚…");
    try {
      const result = await api.revertRevision(contentId!, seq);
      await load(contentId!);
      setMode({ kind: "list" });
      onReverted(result.item);
      setError("");
      setNotice(`已回滚到版本 ${seq}`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function copyText(text: string, label: string) {
    try {
      await navigator.clipboard.writeText(text);
      setNotice(`已复制${label}`);
    } catch {
      setError("复制失败，请手动选择文本。");
    }
  }

  function downloadSnapshot() {
    if (!snapshot) return;
    const item = items.find((entry) => entry.seq === snapshot.seq);
    const name = (item?.slug ?? "content").split("/").pop() ?? "content";
    const stem = name.replace(/\.[^.]+$/, "");
    const blob = new Blob([snapshot.body], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `${stem}-v${snapshot.seq}.${EXT[snapshot.format] ?? "txt"}`;
    anchor.click();
    URL.revokeObjectURL(url);
  }

  const latestSeq = items[0]?.seq;
  const visible = expanded ? items : items.slice(0, 12);

  return (
    <div className="revision-panel">
      <div className="revision-head">
        <strong>
          版本历史 <span className="revision-count">{items.length}</span>
        </strong>
        <div className="revision-head-actions">
          <button
            type="button"
            className="link-button"
            disabled={busy}
            onClick={() =>
              void load(contentId!).catch((e) => setError((e as Error).message))
            }
          >
            刷新
          </button>
          {mode.kind !== "list" && (
            <button
              type="button"
              className="link-button"
              onClick={() => {
                setMode({ kind: "list" });
                setNotice("");
              }}
            >
              返回列表
            </button>
          )}
        </div>
      </div>

      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}
      {busy && busyLabel && <p className="hint">{busyLabel}</p>}

      {/* git diff A B：任意两版本自由对比 */}
      <div className="revision-toolbar">
        <span className="revision-toolbar-label">对比</span>
        <select
          aria-label="基准版本"
          value={base}
          onChange={(e) => setBase(e.target.value ? Number(e.target.value) : "")}
        >
          {items.map((item) => (
            <option key={item.seq} value={item.seq}>
              v{item.seq}
              {item.seq === currentSeq ? " 当前" : ""}
            </option>
          ))}
        </select>
        <span className="revision-toolbar-arrow">→</span>
        <select
          aria-label="目标版本"
          value={target}
          onChange={(e) => setTarget(e.target.value ? Number(e.target.value) : "")}
        >
          {items.map((item) => (
            <option key={item.seq} value={item.seq}>
              v{item.seq}
              {item.seq === currentSeq ? " 当前" : ""}
            </option>
          ))}
        </select>
        <button
          type="button"
          disabled={busy || base === "" || target === "" || base === target}
          onClick={() => runDiff(Number(base), Number(target))}
        >
          对比
        </button>
        <button
          type="button"
          disabled={busy || base === "" || target === ""}
          onClick={() => {
            setBase(target);
            setTarget(base);
          }}
        >
          交换
        </button>
      </div>
      <div className="revision-quick">
        {currentSeq !== null && latestSeq !== undefined && currentSeq !== latestSeq && (
          <button
            type="button"
            className="link-button"
            disabled={busy}
            onClick={() => runDiff(currentSeq, latestSeq)}
          >
            当前 → 最新
          </button>
        )}
        <span className="hint">点版本行的「设为基准 / 设为目标」可任意组合</span>
      </div>

      {/* git log：完整提交记录 */}
      {mode.kind === "list" && (
        <ul className="revision-list">
          {items.length === 0 && <li className="hint">暂无版本。</li>}
          {visible.map((item) => (
            <li
              key={item.seq}
              className={`revision-card ${item.seq === currentSeq ? "is-current" : ""}`}
            >
              <div className="revision-line">
                <span className="mono revision-seq">v{item.seq}</span>
                {item.seq === currentSeq && (
                  <span className="revision-badge">当前</span>
                )}
                <span className="revision-msg" title={item.message}>
                  {item.message || "（无提交说明）"}
                </span>
              </div>
              <div className="revision-line revision-sub">
                <span className="revision-meta">
                  {item.author_name ?? "未知"} ·{" "}
                  {new Date(item.created_at).toLocaleString()}
                </span>
              </div>
              <div className="revision-line revision-sub">
                <span className="revision-stat">
                  <b className="plus">+{item.added}</b>{" "}
                  <b className="minus">-{item.removed}</b>
                </span>
                <span className="revision-meta">
                  {item.kind === "full" ? "全文检查点" : "增量"} · {item.byte_size} B · 父 v
                  {item.parent_seq ?? "—"}
                </span>
              </div>
              <div className="revision-actions">
                <button type="button" onClick={() => setBase(item.seq)}>
                  设为基准
                </button>
                <button type="button" onClick={() => setTarget(item.seq)}>
                  设为目标
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => showSnapshot(item.seq)}
                >
                  查看快照
                </button>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => showBlame(item.seq)}
                >
                  逐行追溯
                </button>
                {item.seq !== currentSeq && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => revert(item.seq)}
                  >
                    回滚到此版
                  </button>
                )}
              </div>
            </li>
          ))}
          {items.length > 12 && (
            <li className="revision-more">
              <button
                type="button"
                className="link-button"
                onClick={() => setExpanded((value) => !value)}
              >
                {expanded ? "收起" : `显示全部 ${items.length} 个版本`}
              </button>
            </li>
          )}
        </ul>
      )}

      {/* git diff：unified diff */}
      {mode.kind === "diff" && (
        <div className="revision-diff-wrap">
          <div className="revision-view-head">
            <span className="hint mono">
              v{mode.from} → v{mode.to}
            </span>
            <div className="revision-view-actions">
              <button
                type="button"
                className="link-button"
                disabled={busy}
                onClick={() => runDiff(mode.to, mode.from)}
              >
                反向对比
              </button>
              <button
                type="button"
                className="link-button"
                disabled={!diff}
                onClick={() => void copyText(diff, " diff")}
              >
                复制 diff
              </button>
            </div>
          </div>
          <DiffView diff={diff} />
        </div>
      )}

      {/* git show：版本快照 */}
      {mode.kind === "show" && snapshot && (
        <div className="revision-preview">
          <div className="revision-view-head">
            <span className="hint mono">
              v{mode.seq} · {snapshot.format} · 只读
            </span>
          </div>
          <div className="revision-actions">
            <button type="button" onClick={() => void copyText(snapshot.body, "快照")}>
              复制全文
            </button>
            <button type="button" onClick={downloadSnapshot}>
              下载快照
            </button>
          </div>
          <pre className="revision-body">{snapshot.body}</pre>
        </div>
      )}

      {/* git blame：逐行追溯 */}
      {mode.kind === "blame" && blame && (
        <div className="revision-blame">
          <p className="hint">
            v{mode.seq} 每行最后由哪个版本修改
            {blame.truncated
              ? `（版本较多，仅追溯最近 30 个，自 v${blame.from} 起）`
              : ""}
          </p>
          <table className="blame-table">
            <tbody>
              {blame.lines.map((line, index) => (
                <tr key={index}>
                  <td className="blame-seq mono">v{blame.origins[index]}</td>
                  <td className="blame-no mono">{index + 1}</td>
                  <td className="blame-text">{line || " "}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
