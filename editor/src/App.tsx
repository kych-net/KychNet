import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { api, type Content, type ContentFormat, type User } from "@admin/api";
import type { PageLink } from "@admin/components/editorCompletions";
import { PagePicker } from "./PagePicker";
import { useVisualViewport } from "./hooks";

// 编辑器（CodeMirror 核心较重）按需加载：页面选择器先轻量呈现，
// 打开某页时才拉取编辑器代码，改善移动端首屏。
const EditorShell = lazy(() =>
  import("./EditorShell").then((m) => ({ default: m.EditorShell })),
);

const FORMATS: ContentFormat[] = ["markdown", "html", "typst", "wikidot", "css", "svg"];

function initialTheme(): "light" | "dark" {
  const saved = localStorage.getItem("kychnet-editor-theme");
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function App() {
  const vp = useVisualViewport();
  const [theme, setTheme] = useState<"light" | "dark">(initialTheme);
  const [user, setUser] = useState<User | null | undefined>(undefined);
  const [pages, setPages] = useState<Content[]>([]);
  const [loading, setLoading] = useState(true);
  const [active, setActive] = useState<Content | null>(null);
  const [newOpen, setNewOpen] = useState(false);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem("kychnet-editor-theme", theme);
  }, [theme]);

  useEffect(() => {
    api.me().then(setUser).catch(() => setUser(null));
  }, []);

  const loadPages = useCallback(async () => {
    setLoading(true);
    try {
      const res = await api.contents();
      setPages(res.items);
    } catch {
      setPages([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (user) void loadPages();
  }, [user, loadPages]);

  // 深链 ?page=<id> 直达某页。
  useEffect(() => {
    if (!user || loading) return;
    const id = new URLSearchParams(window.location.search).get("page");
    if (id) {
      const found = pages.find((p) => p.id === id);
      if (found) setActive(found);
    }
  }, [user, loading, pages]);

  const pageLinks: PageLink[] = useMemo(
    () => pages.map((p) => ({ label: p.slug, path: "/" + p.slug })),
    [pages],
  );

  const onSaved = useCallback((item: Content) => {
    setActive(item);
    setPages((prev) => {
      const idx = prev.findIndex((p) => p.id === item.id);
      if (idx >= 0) {
        const next = [...prev];
        next[idx] = item;
        return next;
      }
      return [item, ...prev];
    });
    const url = new URL(window.location.href);
    url.searchParams.set("page", item.id);
    window.history.replaceState(null, "", url);
  }, []);

  const onBack = useCallback(() => {
    setActive(null);
    const url = new URL(window.location.href);
    url.searchParams.delete("page");
    window.history.replaceState(null, "", url);
    void loadPages();
  }, [loadPages]);

  const createNew = useCallback((slug: string, format: ContentFormat) => {
    setActive({
      id: "",
      slug,
      format,
      body: "",
      published: false,
      created_at: "",
      updated_at: "",
      rev_seq: 0,
    });
    setNewOpen(false);
  }, []);

  return (
    <div className="app" style={{ height: vp.height > 0 ? vp.height : "100dvh", top: vp.offsetTop }}>
      {user === undefined && (
        <div className="splash">
          <div className="spinner" aria-hidden="true" />
          <p>加载中…</p>
        </div>
      )}

      {user === null && (
        <div className="splash login">
          <span className="brand-mark big" aria-hidden="true">✎</span>
          <h1>需要登录</h1>
          <p>编辑器使用与管理端相同的账号。请先登录后再回来。</p>
          <a className="btn btn-accent" href="/管理/">前往登录</a>
        </div>
      )}

      {user && !active && (
        <PagePicker
          pages={pages}
          loading={loading}
          onOpen={(id) => {
            const found = pages.find((p) => p.id === id);
            if (found) setActive(found);
          }}
          onNew={() => setNewOpen(true)}
        />
      )}

      {user && active && (
        <Suspense
          fallback={
            <div className="splash">
              <div className="spinner" aria-hidden="true" />
              <p>编辑器加载中…</p>
            </div>
          }
        >
          <EditorShell
            key={active.id || "new"}
            page={active}
            pages={pageLinks}
            onBack={onBack}
            onSaved={onSaved}
            theme={theme}
            onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
          />
        </Suspense>
      )}

      {newOpen && (
        <NewPageModal onCreate={createNew} onClose={() => setNewOpen(false)} />
      )}
    </div>
  );
}

function NewPageModal({
  onCreate,
  onClose,
}: {
  onCreate: (slug: string, format: ContentFormat) => void;
  onClose: () => void;
}) {
  const [slug, setSlug] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const valid = slug.trim().length > 0;
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} role="dialog" aria-label="新建页面">
        <h2>新建页面</h2>
        <label className="field">
          <span>路径</span>
          <input
            value={slug}
            onChange={(e) => setSlug(e.target.value)}
            placeholder="如 notes/hello.md"
            autoFocus
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
          />
        </label>
        <label className="field">
          <span>格式</span>
          <select value={format} onChange={(e) => setFormat(e.target.value as ContentFormat)}>
            {FORMATS.map((f) => (
              <option key={f} value={f}>{f}</option>
            ))}
          </select>
        </label>
        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>取消</button>
          <button type="button" className="btn btn-accent" disabled={!valid} onClick={() => onCreate(slug.trim(), format)}>创建</button>
        </div>
      </div>
    </div>
  );
}
