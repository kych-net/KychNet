import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "@admin/api";
import type { ChangeMap } from "./drafts";
import type { ProjectDirItem } from "./types";
import { errText } from "./types";

/** 单层懒加载的仓库文件树；目录展开时才请求 /tree。 */
export function FileTree({
  projectId,
  refName,
  refreshKey,
  activePath,
  changes,
  onOpen,
}: {
  projectId: string;
  /** 不要用 `ref`：React 会把它当成组件引用而不传进 props。 */
  refName: string;
  refreshKey: number;
  activePath: string | null;
  changes: ChangeMap;
  onOpen: (path: string) => void;
}) {
  const [items, setItems] = useState<Record<string, ProjectDirItem[]>>({});
  const [open, setOpen] = useState<Record<string, boolean>>({ "": true });
  const [loading, setLoading] = useState<Record<string, boolean>>({});
  const [error, setError] = useState("");
  const epoch = useRef(0);

  const loadDir = useCallback(
    async (dir: string, token: number) => {
      setLoading((prev) => ({ ...prev, [dir]: true }));
      try {
        const res = await api.projectTree(projectId, refName, dir);
        if (epoch.current !== token) return;
        setItems((prev) => ({ ...prev, [dir]: res.items }));
      } catch (e) {
        if (epoch.current === token) setError(errText(e));
      } finally {
        if (epoch.current === token) {
          setLoading((prev) => ({ ...prev, [dir]: false }));
        }
      }
    },
    [projectId, refName],
  );

  useEffect(() => {
    const token = ++epoch.current;
    setItems({});
    setOpen({ "": true });
    setError("");
    void loadDir("", token);
  }, [loadDir, refreshKey]);

  const toggle = (dir: string) => {
    const next = !open[dir];
    setOpen((prev) => ({ ...prev, [dir]: next }));
    if (next && !items[dir]) void loadDir(dir, epoch.current);
  };

  const changedCount = (prefix: string) =>
    Object.keys(changes).filter((path) => !prefix || path.startsWith(`${prefix}/`)).length;

  const renderDir = (dir: string, depth: number): ReactNode => {
    const list = items[dir];
    if (!list) {
      return <div className="pv-tree-loading">加载中…</div>;
    }
    if (list.length === 0 && depth === 0) {
      return <p className="pv-hint">仓库是空的，新建一个文件开始吧。</p>;
    }
    return (
      <ul className="pv-tree-list">
        {[...list]
          .sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name))
          .map((item) => {
            if (item.is_dir) {
              const count = changedCount(item.path);
              return (
                <li key={item.path}>
                  <button
                    type="button"
                    className="pv-tree-row pv-tree-dir"
                    onClick={() => toggle(item.path)}
                    aria-expanded={!!open[item.path]}
                  >
                    <span className="pv-tree-caret">{open[item.path] ? "▾" : "▸"}</span>
                    <span className="pv-tree-icon">📁</span>
                    <span className="pv-tree-name">{item.name}</span>
                    {count > 0 && <span className="pv-change-dot">{count}</span>}
                  </button>
                  {open[item.path] && renderDir(item.path, depth + 1)}
                </li>
              );
            }
            const change = changes[item.path];
            return (
              <li key={item.path}>
                <button
                  type="button"
                  className={`pv-tree-row pv-tree-file${activePath === item.path ? " is-active" : ""}`}
                  onClick={() => onOpen(item.path)}
                >
                  <span className="pv-tree-caret" />
                  <span className="pv-tree-icon">📄</span>
                  <span className="pv-tree-name">{item.name}</span>
                  {change && (
                    <span className={`pv-change-tag pv-change-${change.kind}`}>
                      {change.kind === "add" ? "A" : change.kind === "delete" ? "D" : "M"}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
      </ul>
    );
  };

  return (
    <div className="pv-tree">
      {error && <p className="pv-error">{error}</p>}
      {loading[""] ? <div className="pv-tree-loading">加载文件树…</div> : renderDir("", 0)}
    </div>
  );
}
