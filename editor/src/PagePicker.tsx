import { useMemo, useState } from "react";
import type { Content } from "@admin/api";

const FORMAT_LABEL: Record<string, string> = {
  markdown: "MD",
  html: "HTML",
  typst: "Typst",
  wikidot: "Wikidot",
  css: "CSS",
  svg: "SVG",
};

// 页面选择器：搜索 + 列表 + 新建。移动优先，大触控目标。
export function PagePicker({
  pages,
  loading,
  error,
  onRetry,
  mode,
  onMode,
  onOpen,
  onNew,
}: {
  pages: Content[];
  loading: boolean;
  /** 加载失败信息：与「没有页面」区分开，失败时给重试按钮。 */
  error?: string;
  onRetry?: () => void;
  mode: "pages" | "projects";
  onMode: (mode: "pages" | "projects") => void;
  onOpen: (id: string) => void;
  onNew: () => void;
}) {
  const [query, setQuery] = useState("");
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    const sorted = [...pages].sort((a, b) =>
      b.updated_at.localeCompare(a.updated_at),
    );
    if (!q) return sorted;
    return sorted.filter((p) => p.slug.toLowerCase().includes(q));
  }, [pages, query]);

  return (
    <div className="picker">
      <header className="picker-head">
        <div className="picker-brand">
          <span className="brand-mark" aria-hidden="true">✎</span>
          <div>
            <h1>编辑器</h1>
            <p>选择要编辑的页面</p>
          </div>
        </div>
        <button type="button" className="btn btn-accent" onClick={onNew}>
          ＋ 新建
        </button>
      </header>

      <nav className="picker-modes" aria-label="编辑对象">
        <button type="button" className={mode === "pages" ? "is-active" : ""} aria-current={mode === "pages" ? "page" : undefined} onClick={() => onMode("pages")}>📄 页面</button>
        <button type="button" className={mode === "projects" ? "is-active" : ""} aria-current={mode === "projects" ? "page" : undefined} onClick={() => onMode("projects")}>⑂ 项目</button>
      </nav>

      <div className="picker-search">
        <input
          type="search"
          inputMode="search"
          placeholder="搜索路径…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label="搜索页面"
        />
      </div>

      <div className="picker-list">
        {loading && <div className="picker-empty">加载中…</div>}
        {!loading && error && (
          <div className="picker-error" role="alert">
            <p>加载页面失败：{error}</p>
            {onRetry && (
              <button type="button" className="btn" onClick={onRetry}>重试</button>
            )}
          </div>
        )}
        {!loading && !error && filtered.length === 0 && (
          <div className="picker-empty">没有匹配的页面</div>
        )}
        {!loading &&
          !error &&
          filtered.map((p) => (
            <button
              key={p.id}
              type="button"
              className="page-row"
              onClick={() => onOpen(p.id)}
            >
              <span className={`fmt-badge fmt-${p.format}`}>
                {FORMAT_LABEL[p.format] ?? p.format}
              </span>
              <span className="page-path">{p.slug}</span>
              <span className="page-meta">
                {!p.published && <span className="draft-tag">草稿</span>}
                <span className="page-date">{p.updated_at.slice(0, 10)}</span>
                <span className="chevron" aria-hidden="true">›</span>
              </span>
            </button>
          ))}
      </div>
    </div>
  );
}
