import { lazy, Suspense, useCallback, useEffect, useMemo, useState } from "react";
import { api, type Content, type ContentFormat, type Project, type User } from "@admin/api";
import type { PageLink } from "@admin/components/editorCompletions";
import { PagePicker } from "./PagePicker";
import { ProjectPicker } from "./project/ProjectPicker";
import { useVisualViewport } from "./hooks";

// 编辑器与项目工作区（CodeMirror 核心较重）都按需加载，页面选择器先轻量呈现。
const EditorShell = lazy(() =>
  import("./EditorShell").then((m) => ({ default: m.EditorShell })),
);
const ProjectShell = lazy(() =>
  import("./project/ProjectShell").then((m) => ({ default: m.ProjectShell })),
);

const FORMATS: ContentFormat[] = ["markdown", "html", "typst", "wikidot", "css", "svg"];
type Mode = "pages" | "projects";

function initialTheme(): "light" | "dark" {
  const saved = localStorage.getItem("kychnet-editor-theme");
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function initialMode(): Mode {
  const params = new URLSearchParams(window.location.search);
  return params.has("project") || params.get("tab") === "projects" ? "projects" : "pages";
}

function Loading({ text }: { text: string }) {
  return (
    <div className="splash">
      <div className="spinner" aria-hidden="true" />
      <p>{text}</p>
    </div>
  );
}

export function App() {
  const vp = useVisualViewport();
  const [theme, setTheme] = useState<"light" | "dark">(initialTheme);
  const [user, setUser] = useState<User | null | undefined>(undefined);
  const [mode, setMode] = useState<Mode>(initialMode);

  const [pages, setPages] = useState<Content[]>([]);
  const [loading, setLoading] = useState(true);
  const [active, setActive] = useState<Content | null>(null);
  const [newOpen, setNewOpen] = useState(false);

  const [projects, setProjects] = useState<Project[]>([]);
  const [projectsLoading, setProjectsLoading] = useState(true);
  const [activeProject, setActiveProject] = useState<Project | null>(null);
  const [projectRef, setProjectRef] = useState<string | undefined>(undefined);
  const [projectFile, setProjectFile] = useState<string | undefined>(undefined);

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

  const loadProjects = useCallback(async () => {
    setProjectsLoading(true);
    try {
      const res = await api.projects();
      setProjects(res.items);
    } catch {
      setProjects([]);
    } finally {
      setProjectsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!user) return;
    void loadPages();
    void loadProjects();
  }, [user, loadPages, loadProjects]);

  // 深链 ?page=<id> 直达某页；?project=<id> 直达项目工作区。
  useEffect(() => {
    if (!user || loading) return;
    const params = new URLSearchParams(window.location.search);
    const id = params.get("page");
    if (id) {
      const found = pages.find((p) => p.id === id);
      if (found) setActive(found);
    }
  }, [user, loading, pages]);

  useEffect(() => {
    if (!user) return;
    const params = new URLSearchParams(window.location.search);
    const id = params.get("project");
    if (!id) return;
    let cancelled = false;
    setProjectRef(params.get("ref") ?? undefined);
    setProjectFile(params.get("file") ?? undefined);
    api
      .project(id)
      .then((res) => {
        if (!cancelled) {
          setActiveProject(res.item);
          setMode("projects");
        }
      })
      .catch(() => {
        if (!cancelled) setActiveProject(null);
      });
    return () => {
      cancelled = true;
    };
  }, [user]);

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
      head: "",
      published: false,
      created_at: "",
      updated_at: "",
      rev_seq: 0,
    });
    setNewOpen(false);
  }, []);

  const switchMode = useCallback((next: Mode) => {
    setMode(next);
    setActive(null);
    setActiveProject(null);
    const url = new URL(window.location.href);
    url.searchParams.delete("page");
    url.searchParams.delete("project");
    url.searchParams.delete("ref");
    url.searchParams.delete("file");
    if (next === "projects") url.searchParams.set("tab", "projects");
    else url.searchParams.delete("tab");
    window.history.replaceState(null, "", url);
  }, []);

  const openProject = useCallback(async (id: string) => {
    try {
      const res = await api.project(id);
      setActiveProject(res.item);
      setProjectRef(undefined);
      setProjectFile(undefined);
      setMode("projects");
      const url = new URL(window.location.href);
      url.searchParams.set("project", id);
      url.searchParams.delete("page");
      url.searchParams.delete("ref");
      url.searchParams.delete("file");
      url.searchParams.delete("tab");
      window.history.replaceState(null, "", url);
    } catch (error) {
      window.alert(error instanceof Error ? error.message : "打开项目失败");
    }
  }, []);

  const backToProjects = useCallback(() => {
    setActiveProject(null);
    const url = new URL(window.location.href);
    for (const key of ["project", "ref", "file", "tab"]) url.searchParams.delete(key);
    url.searchParams.set("tab", "projects");
    window.history.replaceState(null, "", url);
    void loadProjects();
  }, [loadProjects]);

  const createProject = useCallback(
    async (slug: string, description: string) => {
      const res = await api.createProject({ slug, description });
      await loadProjects();
      await openProject(res.item.id);
    },
    [loadProjects, openProject],
  );

  const importProject = useCallback(
    async (url: string, slug: string, description: string, branch: string) => {
      const res = await api.importProject({
        url,
        slug,
        description,
        branch: branch || undefined,
      });
      await loadProjects();
      await openProject(res.item.id);
    },
    [loadProjects, openProject],
  );

  return (
    <div className="app" style={{ height: vp.height > 0 ? vp.height : "100dvh", top: vp.offsetTop }}>
      {user === undefined && <Loading text="加载中…" />}

      {user === null && (
        <div className="splash login">
          <span className="brand-mark big" aria-hidden="true">✎</span>
          <h1>需要登录</h1>
          <p>编辑器使用与管理端相同的账号。请先登录后再回来。</p>
          <a className="btn btn-accent" href="/管理/">前往登录</a>
        </div>
      )}

      {user && !active && !activeProject && mode === "pages" && (
        <PagePicker
          pages={pages}
          loading={loading}
          mode={mode}
          onMode={switchMode}
          onOpen={(id) => {
            const found = pages.find((p) => p.id === id);
            if (found) setActive(found);
          }}
          onNew={() => setNewOpen(true)}
        />
      )}

      {user && !active && !activeProject && mode === "projects" && (
        <ProjectPicker
          projects={projects}
          loading={projectsLoading}
          onMode={switchMode}
          onOpen={(id) => void openProject(id)}
          onCreate={createProject}
          onImport={importProject}
        />
      )}

      {user && active && mode === "pages" && (
        <Suspense fallback={<Loading text="编辑器加载中…" />}>
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

      {user && activeProject && (
        <Suspense fallback={<Loading text="项目工作区加载中…" />}>
          <ProjectShell
            key={activeProject.id}
            project={activeProject}
            initialRef={projectRef}
            initialFile={projectFile}
            theme={theme}
            onToggleTheme={() => setTheme((t) => (t === "dark" ? "light" : "dark"))}
            onBack={backToProjects}
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
