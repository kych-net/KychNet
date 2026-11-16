import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "@admin/api";
import { useMediaQuery } from "../hooks";
import type { ChangeMap, LocalChange } from "./drafts";
import { draftKey, loadChanges, normalizeChange, persistChanges } from "./drafts";
import { FileTree } from "./FileTree";
import { FileEditor, type OpenFile } from "./FileEditor";
import { ChangesView } from "./ChangesView";
import { HistoryView } from "./HistoryView";
import { BranchesView } from "./BranchesView";
import { TagsView } from "./TagsView";
import { PrsView } from "./PrsView";
import type { Project, ProjectRefs } from "./types";
import { errText, joinPath, parentPath, shortOid } from "./types";

type Tab = "files" | "changes" | "history" | "branches" | "tags" | "prs";

const TABS: Array<{ key: Tab; label: string; icon: string }> = [
  { key: "files", label: "文件", icon: "📄" },
  { key: "changes", label: "变更", icon: "✎" },
  { key: "history", label: "历史", icon: "🕘" },
  { key: "branches", label: "分支", icon: "⑂" },
  { key: "tags", label: "标签", icon: "🏷" },
  { key: "prs", label: "PR", icon: "⇄" },
];

/** 项目工作区：文件树 + 编辑器 + 完整 Git 面板。 */
export function ProjectShell({
  project,
  initialRef,
  initialFile,
  onBack,
  theme,
  onToggleTheme,
}: {
  project: Project;
  initialRef?: string;
  initialFile?: string;
  onBack: () => void;
  theme: "light" | "dark";
  onToggleTheme: () => void;
}) {
  const isDesktop = useMediaQuery("(min-width: 900px)");
  const [refName, setRefName] = useState(initialRef || project.default_branch);
  const [refs, setRefs] = useState<ProjectRefs | null>(null);
  const [tab, setTab] = useState<Tab>(() => {
    const value = new URLSearchParams(window.location.search).get("tab");
    return TABS.some((item) => item.key === value) ? (value as Tab) : "files";
  });
  const [changes, setChanges] = useState<ChangeMap>({});
  const [openPath, setOpenPath] = useState<string | null>(null);
  const [openFile, setOpenFile] = useState<OpenFile | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [newOpen, setNewOpen] = useState(false);
  const [newPath, setNewPath] = useState("");
  const [dir, setDir] = useState("");
  const openEpoch = useRef(0);
  const refsEpoch = useRef(0);

  const isBranch = refs?.branches.includes(refName) ?? refName === project.default_branch;
  const readOnly = !isBranch;
  const fontSize = isDesktop ? 14 : 16;
  const pendingCount = Object.keys(changes).length;

  const notify = useCallback((message: string) => {
    setNotice(message);
    if (message) window.setTimeout(() => setNotice(""), 3200);
  }, []);

  const applyChanges = useCallback(
    (next: ChangeMap) => {
      setChanges(next);
      persistChanges(draftKey(project.id, refName), next);
    },
    [project.id, refName],
  );

  const loadRefs = useCallback(
    async (token: number) => {
      try {
        const res = await api.projectRefs(project.id, refName);
        if (refsEpoch.current !== token) return;
        setRefs(res);
      } catch (e) {
        if (refsEpoch.current === token) setError(errText(e));
      }
    },
    [project.id, refName],
  );

  // ref 变化时：换一桶本地草稿、重新取引用概览。
  useEffect(() => {
    const token = ++refsEpoch.current;
    setChanges(loadChanges(draftKey(project.id, refName)));
    void loadRefs(token);
  }, [refName, project.id, loadRefs]);

  const refresh = useCallback(async () => {
    setRefreshKey((value) => value + 1);
    await loadRefs(++refsEpoch.current);
    if (openPath) {
      const token = ++openEpoch.current;
      try {
        const res = await api.projectFile(project.id, refName, openPath);
        if (openEpoch.current !== token) return;
        const item = res.item;
        setOpenFile({
          path: openPath,
          content: item?.content ?? "",
          oid: item?.oid ?? "",
          isNew: !item,
        });
      } catch {
        // 文件可能在提交里被删掉，保留编辑器里的当前内容。
      }
    }
  }, [loadRefs, openPath, project.id, refName]);

  // 深链同步到地址栏，方便把某个分支 / 文件直接发给别人。
  useEffect(() => {
    const url = new URL(window.location.href);
    url.searchParams.set("project", project.id);
    if (refName !== project.default_branch) url.searchParams.set("ref", refName);
    else url.searchParams.delete("ref");
    if (openPath) url.searchParams.set("file", openPath);
    else url.searchParams.delete("file");
    if (tab !== "files") url.searchParams.set("tab", tab);
    else url.searchParams.delete("tab");
    url.searchParams.delete("page");
    window.history.replaceState(null, "", url);
  }, [project.id, project.default_branch, refName, openPath, tab]);

  const openFileAt = useCallback(
    async (path: string, targetRef = refName) => {
      const token = ++openEpoch.current;
      setOpenPath(path);
      setOpenFile(null);
      setTab("files");
      setError("");
      try {
        const res = await api.projectFile(project.id, targetRef, path);
        if (openEpoch.current !== token) return;
        const item = res.item;
        setOpenFile({
          path,
          content: item?.content ?? "",
          oid: item?.oid ?? "",
          isNew: !item,
        });
      } catch (e) {
        if (openEpoch.current === token) setError(errText(e));
      }
    },
    [project.id, refName],
  );

  useEffect(() => {
    if (initialFile) void openFileAt(initialFile);
    // 只在首次挂载时处理深链文件。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const switchRef = useCallback(
    (next: string, path?: string) => {
      setRefName(next);
      setOpenPath(null);
      setOpenFile(null);
      setError("");
      setTab("files");
      if (path) void openFileAt(path, next);
    },
    [openFileAt],
  );

  const onDraft = useCallback(
    (value: string) => {
      if (!openFile || readOnly) return;
      const path = openFile.path;
      const next = { ...changes };
      const existing = next[path];
      const candidate: LocalChange = {
        path,
        kind: existing?.kind ?? (openFile.isNew ? "add" : "modify"),
        base: existing?.base ?? openFile.content,
        baseOid: existing?.baseOid ?? openFile.oid,
        content: value,
      };
      const normalized = normalizeChange(candidate);
      if (normalized) next[path] = normalized;
      else delete next[path];
      applyChanges(next);
    },
    [openFile, readOnly, changes, applyChanges],
  );

  const discard = useCallback(
    (paths: string[]) => {
      const next = { ...changes };
      for (const path of paths) delete next[path];
      applyChanges(next);
    },
    [changes, applyChanges],
  );

  const commit = useCallback(
    async (paths: string[], message: string) => {
      const payload = paths.map((path) => {
        const change = changes[path];
        return change.kind === "delete"
          ? { path, delete: true }
          : { path, content: change.content };
      });
      await api.commitProjectChanges(project.id, { ref: refName, message, changes: payload });
      const next = { ...changes };
      for (const path of paths) delete next[path];
      applyChanges(next);
      notify(`已提交 ${paths.length} 个变更`);
      await refresh();
    },
    [changes, project.id, refName, applyChanges, notify, refresh],
  );

  const quickCommit = useCallback(
    async (message: string) => {
      if (!openFile || !openPath) return;
      const content = changes[openPath]?.content ?? openFile.content;
      await api.saveProjectFile(project.id, {
        ref: refName,
        path: openPath,
        content,
        message,
        base_oid: openFile.oid || undefined,
      });
      discard([openPath]);
      notify("文件已提交");
      await refresh();
    },
    [openFile, openPath, changes, project.id, refName, discard, notify, refresh],
  );

  const renameFile = useCallback(
    async (to: string) => {
      if (!openPath) return;
      await api.moveProjectFile(project.id, { ref: refName, from: openPath, to });
      discard([openPath]);
      setOpenPath(to);
      notify("文件已移动");
      await refresh();
    },
    [openPath, project.id, refName, discard, notify, refresh],
  );

  const deleteFile = useCallback(
    async () => {
      if (!openPath) return;
      await api.deleteProjectFile(project.id, refName, openPath);
      discard([openPath]);
      setOpenPath(null);
      setOpenFile(null);
      notify("文件已删除");
      await refresh();
    },
    [openPath, project.id, refName, discard, notify, refresh],
  );

  const createFile = async () => {
    const path = joinPath(dir, newPath.trim());
    if (!path) return;
    setBusy(true);
    setError("");
    try {
      const res = await api.projectFile(project.id, refName, path);
      if (res.item) {
        setError("该路径已存在，请直接打开编辑");
        return;
      }
      setNewOpen(false);
      setNewPath("");
      setOpenPath(path);
      setOpenFile({ path, content: "", oid: "", isNew: true });
      setTab("files");
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const draft = useMemo(() => {
    if (!openFile) return "";
    return changes[openFile.path]?.content ?? openFile.content;
  }, [openFile, changes]);

  return (
    <div className="pv-shell">
      <header className="pv-topbar">
        <button type="button" className="icon-btn back-btn" onClick={onBack} aria-label="返回项目列表">‹</button>
        <button type="button" className="pv-title" onClick={() => setTab("branches")} title="引用与分支">
          <span className="pv-title-slug">{project.slug}</span>
          <span className="pv-title-ref">{refName.length === 40 ? shortOid(refName) : refName}</span>
        </button>
        <button type="button" className={`pv-pending${pendingCount ? " has-changes" : ""}`} onClick={() => setTab("changes")} title="本地变更">
          {pendingCount ? `${pendingCount} 项待提交` : "工作区干净"}
        </button>
        <div className="pv-topbar-actions">
          <button type="button" className="icon-btn" onClick={() => void refresh()} title="刷新" aria-label="刷新">⟳</button>
          <button type="button" className="icon-btn" onClick={onToggleTheme} title="切换主题" aria-label="切换主题">◐</button>
        </div>
      </header>

      <nav className="pv-tabs" aria-label="项目功能">
        {TABS.map((item) => (
          <button
            key={item.key}
            type="button"
            className={`pv-main-tab${tab === item.key ? " is-active" : ""}`}
            onClick={() => setTab(item.key)}
          >
            <span aria-hidden="true">{item.icon}</span> {item.label}
            {item.key === "changes" && pendingCount > 0 && <span className="pv-tab-badge">{pendingCount}</span>}
          </button>
        ))}
      </nav>

      {(error || notice) && (
        <div className={`pv-flash${error ? " is-error" : ""}`} onClick={() => { setError(""); setNotice(""); }}>
          {error || notice}
        </div>
      )}

      <main className="pv-main">
        {tab === "files" && (
          <div className={`pv-files${openPath ? " has-file" : ""}`}>
            <aside className="pv-tree-col">
              <div className="pv-tree-head">
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => setNewOpen((v) => !v)}>＋ 新建文件</button>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void refresh()}>刷新</button>
              </div>
              {newOpen && (
                <div className="pv-inline-form">
                  <input value={newPath} onChange={(e) => setNewPath(e.target.value)} placeholder={dir ? `${dir}/文件名` : "路径，如 docs/readme.md"} aria-label="新文件路径" autoCapitalize="off" spellCheck={false} />
                  <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !newPath.trim()} onClick={() => void createFile()}>创建</button>
                </div>
              )}
              {readOnly && <div className="pv-readonly">当前引用只读；切到分支即可编辑。</div>}
              <FileTree
                projectId={project.id}
                refName={refName}
                refreshKey={refreshKey}
                activePath={openPath}
                changes={changes}
                onOpen={(path) => {
                  setDir(parentPath(path));
                  void openFileAt(path);
                }}
              />
            </aside>
            <section className="pv-editor-col">
              {openPath && (
                <>
                  <button type="button" className="pv-tree-back" onClick={() => { setOpenPath(null); setOpenFile(null); }}>‹ 文件树</button>
                  <FileEditor
                    projectId={project.id}
                    refName={refName}
                    file={openFile}
                    draft={draft}
                    readOnly={readOnly}
                    fontSize={fontSize}
                    onDraft={onDraft}
                    onDiscard={() => discard([openPath])}
                    onQuickCommit={quickCommit}
                    onRename={renameFile}
                    onDelete={deleteFile}
                    onOpenRef={(targetRef, path) => switchRef(targetRef, path)}
                    onNotify={notify}
                  />
                </>
              )}
              {!openPath && (
                <div className="pv-empty-state">
                  <span className="pv-empty-icon" aria-hidden="true">📄</span>
                  <h2>选择文件</h2>
                  <p>从左侧文件树打开一个文件，或新建一个文件开始编辑。</p>
                </div>
              )}
            </section>
          </div>
        )}

        {tab === "changes" && (
          <ChangesView
            changes={changes}
            readOnly={readOnly}
            onCommit={commit}
            onDiscard={discard}
            onOpen={(path) => void openFileAt(path)}
          />
        )}

        {!refs && tab !== "files" && <div className="pv-loading">加载引用…</div>}

        {refs && tab === "history" && (
          <HistoryView
            projectId={project.id}
            refName={refName}
            refreshKey={refreshKey}
            readOnly={readOnly}
            onOpenRef={(targetRef) => switchRef(targetRef)}
            onChanged={() => void refresh()}
            onNotify={notify}
          />
        )}

        {refs && tab === "branches" && (
          <BranchesView
            project={project}
            refs={refs}
            currentRef={refName}
            onSwitch={(targetRef) => switchRef(targetRef)}
            onChanged={() => void refresh()}
            onOpenRef={(targetRef) => switchRef(targetRef)}
            onNotify={notify}
          />
        )}

        {refs && tab === "tags" && (
          <TagsView
            projectId={project.id}
            refs={refs}
            currentRef={refName}
            onSwitch={(targetRef) => switchRef(targetRef)}
            onChanged={() => void refresh()}
            onNotify={notify}
          />
        )}

        {refs && tab === "prs" && (
          <PrsView
            projectId={project.id}
            refs={refs}
            refreshKey={refreshKey}
            onChanged={() => void refresh()}
            onNotify={notify}
          />
        )}
      </main>
    </div>
  );
}
