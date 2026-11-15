import { useEffect, useMemo, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { CodeEditor } from "@admin/components/CodeEditor";
import { api } from "@admin/api";
import { Toolbar } from "../Toolbar";
import { diffText } from "./diff";
import { LocalDiffView } from "./DiffView";
import type { BlameResult, FileHistoryEntry } from "./types";
import { copyText, errText, firstLine, formatForPath, formatTime, shortOid } from "./types";

export type OpenFile = {
  path: string;
  content: string;
  oid: string;
  /** 服务端没有这个文件（新建）。 */
  isNew: boolean;
};

type Panel = "diff" | "history" | "blame" | null;

/** 单文件编辑 + 该文件自己的差异 / 历史 / 逐行追溯。 */
export function FileEditor({
  projectId,
  refName,
  file,
  draft,
  readOnly,
  fontSize,
  onDraft,
  onDiscard,
  onQuickCommit,
  onRename,
  onDelete,
  onOpenRef,
  onNotify,
}: {
  projectId: string;
  /** 不用 `ref`：React 会把它当成组件引用。 */
  refName: string;
  file: OpenFile | null;
  draft: string;
  readOnly: boolean;
  fontSize: number;
  onDraft: (value: string) => void;
  onDiscard: () => void;
  onQuickCommit: (message: string) => Promise<void>;
  onRename: (to: string) => Promise<void>;
  onDelete: () => Promise<void>;
  onOpenRef: (ref: string, path: string) => void;
  onNotify: (message: string) => void;
}) {
  const [view, setView] = useState<EditorView | null>(null);
  const [panel, setPanel] = useState<Panel>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [renameTo, setRenameTo] = useState("");
  const [renaming, setRenaming] = useState(false);
  const [history, setHistory] = useState<FileHistoryEntry[]>([]);
  const [blame, setBlame] = useState<BlameResult | null>(null);

  const path = file?.path ?? "";
  useEffect(() => {
    setPanel(null);
    setError("");
    setMessage("");
    setRenameTo(path);
    setHistory([]);
    setBlame(null);
  }, [path, refName]);

  const localDiff = useMemo(
    () => diffText(file?.content ?? "", draft),
    [file?.content, draft],
  );
  const changed = draft !== (file?.content ?? "");

  const openPanel = async (next: Exclude<Panel, null>) => {
    if (panel === next) {
      setPanel(null);
      return;
    }
    setPanel(next);
    setError("");
    if (!path) return;
    setBusy(true);
    try {
      if (next === "history") {
        const res = await api.fileHistory(projectId, refName, path, 60);
        setHistory(res.items);
      } else if (next === "blame") {
        const res = await api.blame(projectId, refName, path);
        setBlame(res.item);
      }
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const quickCommit = async () => {
    if (!message.trim()) {
      setError("请填写提交说明");
      return;
    }
    setBusy(true);
    setError("");
    try {
      await onQuickCommit(message.trim());
      setMessage("");
      setPanel(null);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doRename = async () => {
    if (!renameTo.trim() || renameTo === path) return;
    setBusy(true);
    setError("");
    try {
      await onRename(renameTo.trim());
      setRenaming(false);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doDelete = async () => {
    if (!window.confirm(`确定删除 ${path}？此操作会生成一次提交。`)) return;
    setBusy(true);
    setError("");
    try {
      await onDelete();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const download = () => {
    const blob = new Blob([draft], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = path.split("/").pop() || "file.txt";
    link.click();
    URL.revokeObjectURL(url);
  };

  if (!file) {
    return <div className="pv-loading">打开文件…</div>;
  }

  const format = formatForPath(path);

  return (
    <div className="pv-editor">
      <div className="pv-editor-head">
        <div className="pv-editor-title">
          <code className="pv-path">{path}</code>
          {file.isNew && <span className="pv-chip pv-chip-add">新文件</span>}
          {changed && <span className="pv-chip pv-chip-modify">已修改</span>}
          {readOnly && <span className="pv-chip">只读</span>}
        </div>
        <div className="pv-editor-actions">
          <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copyText(path).then(() => onNotify("路径已复制")).catch((e) => setError(errText(e)))}>复制路径</button>
          <button type="button" className="pv-btn pv-btn-ghost" onClick={download}>下载</button>
          <button type="button" className="pv-btn pv-btn-ghost" onClick={() => setRenaming((v) => !v)}>移动/改名</button>
          <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void doDelete()} disabled={readOnly || file.isNew || busy}>删除</button>
        </div>
      </div>

      {readOnly && (
        <div className="pv-readonly">
          当前 ref 不是分支，文件只读。可在「分支」页从这里新建分支后继续编辑。
        </div>
      )}

      {renaming && (
        <div className="pv-inline-form">
          <input
            value={renameTo}
            onChange={(e) => setRenameTo(e.target.value)}
            aria-label="新路径"
            autoCapitalize="off"
            spellCheck={false}
          />
          <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !renameTo.trim() || renameTo === path} onClick={() => void doRename()}>移动</button>
          <button type="button" className="pv-btn" onClick={() => setRenaming(false)}>取消</button>
        </div>
      )}

      <div className="pv-editor-tabs">
        <button type="button" className={`pv-tab${panel === "diff" ? " is-active" : ""}`} onClick={() => void openPanel("diff")}>差异{localDiff.additions + localDiff.deletions > 0 ? ` (${localDiff.additions + localDiff.deletions})` : ""}</button>
        <button type="button" className={`pv-tab${panel === "history" ? " is-active" : ""}`} onClick={() => void openPanel("history")}>文件历史</button>
        <button type="button" className={`pv-tab${panel === "blame" ? " is-active" : ""}`} onClick={() => void openPanel("blame")}>逐行追溯</button>
        <button type="button" className="pv-tab" onClick={() => { onDraft(file.content); setPanel(null); }} disabled={!changed || readOnly}>撤销本地改动</button>
      </div>

      {error && <p className="pv-error">{error}</p>}

      {panel === "diff" && (
        <div className="pv-panel">
          <LocalDiffView diff={localDiff} empty="与当前版本一致。" />
        </div>
      )}

      {panel === "history" && (
        <div className="pv-panel">
          {busy && <p className="pv-hint">加载中…</p>}
          {!busy && history.length === 0 && <p className="pv-hint">这个文件没有历史提交。</p>}
          {history.map((item) => (
            <button type="button" className="pv-commit-row" key={item.oid} onClick={() => onOpenRef(item.oid, path)}>
              <span className="pv-oid">{shortOid(item.oid)}</span>
              <span className="pv-commit-msg">{firstLine(item.message)}</span>
              <span className="pv-commit-meta">{item.author_name} · {formatTime(item.committed_at)}</span>
              <span className={`pv-chip pv-chip-${item.change === "delete" ? "delete" : item.change === "add" ? "add" : "modify"}`}>{item.change === "add" ? "新增" : item.change === "delete" ? "删除" : "修改"}</span>
            </button>
          ))}
        </div>
      )}

      {panel === "blame" && (
        <div className="pv-panel">
          {busy && <p className="pv-hint">加载中…</p>}
          {!busy && !blame && <p className="pv-hint">无法追溯该文件。</p>}
          {blame?.truncated && <p className="pv-hint">文件过大，只显示前部分行。</p>}
          {blame && (
            <div className="pv-blame">
              {blame.lines.map((line, index) => {
                const prev = blame.lines[index - 1];
                const grouped = prev?.commit === line.commit;
                return (
                  <div className={`pv-blame-row${grouped ? " is-grouped" : ""}`} key={line.line}>
                    <span className="pv-blame-no">{line.line}</span>
                    <button type="button" className="pv-blame-oid" onClick={() => onOpenRef(line.commit, path)} title={firstLine(line.message)}>
                      {grouped ? "" : shortOid(line.commit)}
                    </button>
                    <span className="pv-blame-msg">{grouped ? "" : firstLine(line.message)}</span>
                    <code className="pv-blame-text">{line.text || " "}</code>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      <div className="pv-code">
        <CodeEditor
          value={draft}
          format={format}
          onChange={onDraft}
          onViewReady={setView}
          fontSize={fontSize}
        />
      </div>

      <div className="pv-quick-commit">
        <input
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder="提交说明（单文件快速提交）"
          disabled={readOnly || busy}
          aria-label="提交说明"
        />
        <button type="button" className="pv-btn pv-btn-accent" onClick={() => void quickCommit()} disabled={readOnly || busy || !changed || !message.trim()}>提交此文件</button>
        <button type="button" className="pv-btn" onClick={onDiscard} disabled={!changed}>放弃草稿</button>
      </div>

      <Toolbar format={format} view={view} />
    </div>
  );
}
