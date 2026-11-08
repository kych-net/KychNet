import { useCallback, useEffect, useState } from "react";
import { api, Content, ContentRevision } from "../api";

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

type Mode = { kind: "list" } | { kind: "diff"; seq: number; against: number } | { kind: "preview"; seq: number };

/** 内容页右栏的精简版历史面板：版本列表 / 双版本 diff / 预览 / 回滚。 */
export function RevisionPanel({
  contentId,
  onReverted,
}: {
  contentId: string | null;
  onReverted: (item: Content) => void;
}) {
  const [items, setItems] = useState<ContentRevision[]>([]);
  const [mode, setMode] = useState<Mode>({ kind: "list" });
  const [diff, setDiff] = useState("");
  const [preview, setPreview] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async (id: string) => {
    try {
      const result = await api.revisions(id);
      setItems(result.items);
      setError("");
    } catch (e) {
      setItems([]);
      setError((e as Error).message);
    }
  }, []);

  useEffect(() => {
    setMode({ kind: "list" });
    setError("");
    if (contentId) void load(contentId);
    else setItems([]);
  }, [contentId, load]);

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

  async function showDiff(seq: number, against: number) {
    setBusy(true);
    try {
      const result = await api.revisionDiff(contentId!, seq, against);
      setDiff(result.diff);
      setMode({ kind: "diff", seq, against });
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function showPreview(seq: number) {
    setBusy(true);
    try {
      const result = await api.revisionBody(contentId!, seq);
      setPreview(result.body);
      setMode({ kind: "preview", seq });
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function revert(seq: number) {
    if (!window.confirm(`回滚到版本 ${seq}？指针将直接移动，之后的版本仍保留在历史中。`)) return;
    setBusy(true);
    try {
      const result = await api.revertRevision(contentId!, seq);
      await load(contentId!);
      setMode({ kind: "list" });
      onReverted(result.item);
      setError("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  // 默认与当前版本比较；列表里第一项通常是最新版本。
  const latestSeq = items[0]?.seq;
  const currentSeq = items.find((item) => item.is_current)?.seq;

  return (
    <div className="revision-panel">
      <div className="revision-head">
        <strong>版本历史</strong>
        <button
          type="button"
          className="link-button"
          onClick={() => setMode({ kind: "list" })}
          disabled={mode.kind === "list"}
        >
          返回列表
        </button>
      </div>
      {error && <p className="error">{error}</p>}

      {mode.kind === "list" && (
        <ul className="revision-list">
          {items.length === 0 && <li className="hint">暂无版本。</li>}
          {items.map((item) => (
            <li key={item.seq} className={item.is_current ? "is-current" : ""}>
              <div className="revision-line">
                <span className="mono">v{item.seq}</span>
                {item.is_current && <span className="revision-badge">当前</span>}
                <span className="revision-meta">
                  {item.kind === "full" ? "全文" : "增量"} · {item.added}/{item.removed} 行 ·{" "}
                  {new Date(item.created_at).toLocaleString()}
                </span>
              </div>
              <div className="revision-line revision-sub">
                <span className="revision-meta">
                  {item.author_name ?? "未知"} · {item.byte_size} 字节
                  {item.message ? ` · ${item.message}` : ""}
                </span>
              </div>
              <div className="revision-actions">
                {latestSeq !== undefined && item.seq !== latestSeq && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => showDiff(latestSeq, item.seq)}
                  >
                    与最新对比
                  </button>
                )}
                {currentSeq !== undefined && item.seq !== currentSeq && (
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => showDiff(item.seq, currentSeq)}
                  >
                    与当前对比
                  </button>
                )}
                <button type="button" disabled={busy} onClick={() => showPreview(item.seq)}>
                  预览
                </button>
                {!item.is_current && (
                  <button type="button" disabled={busy} onClick={() => revert(item.seq)}>
                    回滚到此版
                  </button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}

      {mode.kind === "diff" && (
        <div className="revision-diff-wrap">
          <p className="hint mono">
            版本 {mode.against} → 版本 {mode.seq}
          </p>
          <DiffView diff={diff} />
        </div>
      )}

      {mode.kind === "preview" && (
        <div className="revision-preview">
          <p className="hint mono">版本 {mode.seq} 的正文（只读）</p>
          <pre className="revision-body">{preview}</pre>
        </div>
      )}
    </div>
  );
}
