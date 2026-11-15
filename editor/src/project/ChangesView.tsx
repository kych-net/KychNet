import { useEffect, useMemo, useState } from "react";
import type { ChangeMap } from "./drafts";
import { changeStats } from "./drafts";
import { diffText } from "./diff";
import { LocalDiffView } from "./DiffView";
import { errText } from "./types";

/** 本地变更暂存区：勾选文件 → 写提交说明 → 一次性提交。 */
export function ChangesView({
  changes,
  readOnly,
  onCommit,
  onDiscard,
  onOpen,
}: {
  changes: ChangeMap;
  readOnly: boolean;
  onCommit: (paths: string[], message: string) => Promise<void>;
  onDiscard: (paths: string[]) => void;
  onOpen: (path: string) => void;
}) {
  const paths = useMemo(() => Object.keys(changes).sort(), [changes]);
  const [selected, setSelected] = useState<Record<string, boolean>>({});
  const [expanded, setExpanded] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    setSelected((prev) => {
      const next: Record<string, boolean> = {};
      for (const path of paths) next[path] = prev[path] ?? true;
      return next;
    });
  }, [paths]);

  const chosen = paths.filter((path) => selected[path]);
  const stats = changeStats(changes);

  const commit = async () => {
    if (chosen.length === 0) {
      setError("请先勾选要提交的变更");
      return;
    }
    if (!message.trim()) {
      setError("请填写提交说明");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await onCommit(chosen, message.trim());
      setMessage("");
      setExpanded(null);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  if (paths.length === 0) {
    return (
      <div className="pv-empty-state">
        <span className="pv-empty-icon" aria-hidden="true">✓</span>
        <h2>工作区干净</h2>
        <p>在「文件」里打开并修改文件，改动会先暂存在本地，确认后再提交到当前分支。</p>
      </div>
    );
  }

  return (
    <div className="pv-changes">
      <div className="pv-section-head">
        <div>
          <h2>本地变更 <span className="pv-count">{paths.length}</span></h2>
          <p className="pv-hint">
            修改 {stats.modified} · 新增 {stats.added} · 删除 {stats.deleted}
          </p>
        </div>
        <div className="pv-inline-actions">
          <button type="button" className="pv-btn pv-btn-ghost" onClick={() => setSelected(Object.fromEntries(paths.map((p) => [p, true])))}>全选</button>
          <button type="button" className="pv-btn pv-btn-ghost" onClick={() => setSelected({})}>全不选</button>
        </div>
      </div>

      {readOnly && <div className="pv-readonly">当前 ref 不是分支，无法提交。请切换到分支。</div>}
      {error && <p className="pv-error">{error}</p>}

      <div className="pv-change-list">
        {paths.map((path) => {
          const change = changes[path];
          const diff = diffText(change.base, change.kind === "delete" ? "" : change.content);
          return (
            <section className="pv-change-item" key={path}>
              <div className="pv-change-row">
                <label className="pv-check">
                  <input type="checkbox" checked={!!selected[path]} onChange={(e) => setSelected((prev) => ({ ...prev, [path]: e.target.checked }))} />
                </label>
                <span className={`pv-chip pv-chip-${change.kind === "delete" ? "delete" : change.kind === "add" ? "add" : "modify"}`}>
                  {change.kind === "add" ? "新增" : change.kind === "delete" ? "删除" : "修改"}
                </span>
                <button type="button" className="pv-path-btn" onClick={() => onOpen(path)}><code className="pv-path">{path}</code></button>
                <span className="pv-diff-stat">
                  <span className="pv-plus">+{diff.additions}</span>
                  <span className="pv-minus">-{diff.deletions}</span>
                </span>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => setExpanded((v) => (v === path ? null : path))}>{expanded === path ? "收起" : "差异"}</button>
              </div>
              {expanded === path && (
                <div className="pv-change-diff"><LocalDiffView diff={diff} empty="与当前版本一致。" /></div>
              )}
            </section>
          );
        })}
      </div>

      <div className="pv-commit-box">
        <label className="pv-field">
          <span>提交说明</span>
          <textarea
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            placeholder="描述这次改动…"
            rows={2}
            disabled={readOnly || busy}
          />
        </label>
        <div className="pv-inline-actions">
          <button type="button" className="pv-btn pv-btn-accent" onClick={() => void commit()} disabled={readOnly || busy || chosen.length === 0}>
            提交 {chosen.length} 个变更
          </button>
          <button type="button" className="pv-btn" onClick={() => onDiscard(chosen.length ? chosen : paths)} disabled={busy}>
            放弃{chosen.length ? "所选" : "全部"}草稿
          </button>
        </div>
        <p className="pv-hint">提交会一次性生成一个新的 commit，并触发默认分支的站点构建。</p>
      </div>
    </div>
  );
}
