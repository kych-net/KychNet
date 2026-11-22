import { useMemo, useState } from "react";
import type { Project } from "./types";
import { errText, formatTime } from "./types";

/** 项目选择器：搜索、新建、从外部仓库导入。 */
export function ProjectPicker({
  projects,
  loading,
  error,
  onRetry,
  onMode,
  onOpen,
  onCreate,
  onImport,
}: {
  projects: Project[];
  loading: boolean;
  /** 加载失败信息：与「没有项目」区分开，失败时给重试按钮。 */
  error?: string;
  onRetry?: () => void;
  onMode: (mode: "pages" | "projects") => void;
  onOpen: (id: string) => void;
  onCreate: (slug: string, description: string) => Promise<void>;
  onImport: (url: string, slug: string, description: string, branch: string) => Promise<void>;
}) {
  const [query, setQuery] = useState("");
  const [mode, setMode] = useState<"list" | "create" | "import" | null>(null);
  const [slug, setSlug] = useState("");
  const [description, setDescription] = useState("");
  const [url, setUrl] = useState("");
  const [branch, setBranch] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState("");

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const sorted = [...projects].sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    return q ? sorted.filter((p) => p.slug.toLowerCase().includes(q) || p.description.toLowerCase().includes(q)) : sorted;
  }, [projects, query]);

  const submit = async () => {
    setBusy(true);
    setFormError("");
    try {
      if (mode === "create") await onCreate(slug.trim(), description.trim());
      else if (mode === "import") {
        const guessed = slug.trim() || url.trim().replace(/\/+$/, "").replace(/\.git$/i, "").split(/[/:]/).filter(Boolean).pop() || "";
        await onImport(url.trim(), guessed, description.trim(), branch.trim());
      }
      setMode(null);
      setSlug("");
      setDescription("");
      setUrl("");
      setBranch("");
    } catch (e) {
      setFormError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="picker">
      <header className="picker-head">
        <div className="picker-brand">
          <span className="brand-mark" aria-hidden="true">⑂</span>
          <div>
            <h1>项目</h1>
            <p>选择仓库开始编辑</p>
          </div>
        </div>
        <div className="pv-inline-actions">
          <button type="button" className="btn" onClick={() => setMode(mode === "import" ? null : "import")}>导入</button>
          <button type="button" className="btn btn-accent" onClick={() => setMode(mode === "create" ? null : "create")}>＋ 新建</button>
        </div>
      </header>

      <nav className="picker-modes" aria-label="编辑对象">
        <button type="button" onClick={() => onMode("pages")}>📄 页面</button>
        <button type="button" className="is-active" aria-current="page" onClick={() => onMode("projects")}>⑂ 项目</button>
      </nav>

      <div className="picker-search">
        <input type="search" inputMode="search" placeholder="搜索项目…" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="搜索项目" />
      </div>

      {mode && (
        <section className="pv-card pv-picker-form">
          <h2>{mode === "create" ? "新建项目" : "导入外部仓库"}</h2>
          {mode === "import" && (
            <label className="pv-field"><span>仓库地址</span><input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://github.com/user/repo.git" autoCapitalize="off" spellCheck={false} /></label>
          )}
          <label className="pv-field"><span>项目名{mode === "import" ? "（留空自动推断）" : ""}</span><input value={slug} onChange={(e) => setSlug(e.target.value)} placeholder="如 notes" autoCapitalize="off" spellCheck={false} /></label>
          <label className="pv-field"><span>描述</span><input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="一句话说明" /></label>
          {mode === "import" && (
            <label className="pv-field"><span>只导入指定分支（可留空）</span><input value={branch} onChange={(e) => setBranch(e.target.value)} placeholder="main" autoCapitalize="off" spellCheck={false} /></label>
          )}
          {formError && <p className="pv-error" role="alert">{formError}</p>}
          <div className="pv-inline-actions">
            <button type="button" className="pv-btn pv-btn-accent" disabled={busy || (mode === "create" ? !slug.trim() : !url.trim())} onClick={() => void submit()}>{busy ? "处理中…" : "确定"}</button>
            <button type="button" className="pv-btn" onClick={() => setMode(null)}>取消</button>
          </div>
        </section>
      )}

      <div className="picker-list">
        {loading && <div className="picker-empty">加载中…</div>}
        {!loading && error && (
          <div className="picker-error" role="alert">
            <p>加载项目失败：{error}</p>
            {onRetry && (
              <button type="button" className="btn" onClick={onRetry}>重试</button>
            )}
          </div>
        )}
        {!loading && !error && filtered.length === 0 && <div className="picker-empty">没有匹配的项目</div>}
        {!loading && !error && filtered.map((project) => (
          <button key={project.id} type="button" className="page-row" onClick={() => onOpen(project.id)}>
            <span className="fmt-badge fmt-html">GIT</span>
            <span className="page-path">{project.slug}</span>
            <span className="page-meta">
              {project.archived_at && <span className="draft-tag">已归档</span>}
              <span className="page-date">{formatTime(project.updated_at).slice(0, 10)}</span>
              <span className="chevron" aria-hidden="true">›</span>
            </span>
          </button>
        ))}
      </div>
    </div>
  );
}
