import { useEffect, useMemo, useState } from "react";
import type { ChangeMap } from "./drafts";
import { changeStats } from "./drafts";
import { diffText } from "./diff";
import { LocalDiffView } from "./DiffView";
import { useConfirm } from "../confirm";
import { useBusy } from "../useBusy";

const MESSAGE_MAX = 200;

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
  const [error, setError] = useState("");
  const { busy, error: busyError, run } = useBusy();
  const { confirm, confirmNode } = useConfirm();

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
    setError("");
    await run(async () => {
      await onCommit(chosen, message.trim());
      setMessage("");
      setExpanded(null);
    });
  };

  const discard = async () => {
    const targets = chosen.length ? chosen : paths;
    const ok = await confirm({
      title: "放弃草稿？",
      body: `将丢弃 ${targets.length} 个文件的本地草稿（${targets.slice(0, 3).join("、")}${targets.length > 3 ? " 等" : ""}），并回到服务端当前版本。此操作不可撤销。`,
      confirmText: "放弃草稿",
      danger: true,
    });
    if (!ok) return;
    onDiscard(targets);
    setError("");
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

      <div className="pv-change-list">
        {paths.map((path) => {
          const change = changes[path];
          const diff = diffText(change.base, change.kind === "delete" ? "" : change.content);
          return (
            <section className="pv-change-item" key={path}>
              <div className="pv-change-row">
                <label className="pv-check">
                  <input
                    type="checkbox"
                    checked={!!selected[path]}
                    aria-label={`选择 ${path}`}
                    onChange={(e) => setSelected((prev) => ({ ...prev, [path]: e.target.checked }))}
                  />
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
            maxLength={MESSAGE_MAX}
            aria-describedby="pv-commit-message-count"
            disabled={readOnly || busy}
          />
        </label>
        <p className="pv-hint pv-char-count" id="pv-commit-message-count">
          {message.length}/{MESSAGE_MAX} 字
        </p>
        {/* 校验/请求错误紧贴提交按钮显示：列表很长时用户不会错过。 */}
        {(error || busyError) && (
          <p className="pv-error pv-commit-error" role="alert">{error || busyError}</p>
        )}
        <div className="pv-inline-actions">
          <button type="button" className="pv-btn pv-btn-accent" onClick={() => void commit()} disabled={readOnly || busy || chosen.length === 0}>
            {busy ? "提交中…" : `提交 ${chosen.length} 个变更`}
          </button>
          <button type="button" className="pv-btn" onClick={() => void discard()} disabled={busy}>
            放弃{chosen.length ? "所选" : "全部"}草稿
          </button>
        </div>
        <p className="pv-hint">提交会一次性生成一个新的 commit，并触发默认分支的站点构建。</p>
      </div>

      {confirmNode}
    </div>
  );
}
