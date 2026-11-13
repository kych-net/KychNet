import { useCallback, useEffect, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { CodeEditor } from "@admin/components/CodeEditor";
import type { PageLink } from "@admin/components/editorCompletions";
import { api, type Content, type ContentFormat } from "@admin/api";
import { Toolbar } from "./Toolbar";
import { Preview } from "./Preview";
import { useAutosave, useMediaQuery, type SaveStatus } from "./hooks";

const FORMATS: ContentFormat[] = ["markdown", "html", "typst", "wikidot", "css", "svg"];
const STATUS_TEXT: Record<SaveStatus, string> = {
  idle: "就绪",
  dirty: "待保存",
  saving: "保存中…",
  saved: "已保存",
  error: "保存失败",
};

export function EditorShell({
  page,
  pages,
  onBack,
  onSaved,
  theme,
  onToggleTheme,
}: {
  page: Content;
  pages: PageLink[];
  onBack: () => void;
  onSaved: (item: Content) => void;
  theme: "light" | "dark";
  onToggleTheme: () => void;
}) {
  const isDesktop = useMediaQuery("(min-width: 900px)");
  const draftKey = page.id ? `kychnet-editor-draft:${page.id}` : null;

  const [body, setBody] = useState(() => {
    if (draftKey) {
      const draft = localStorage.getItem(draftKey);
      if (draft !== null) return draft;
    }
    return page.body;
  });
  const [format, setFormat] = useState<ContentFormat>(page.format);
  const [slug, setSlug] = useState(page.slug);
  const [published, setPublished] = useState(page.published);
  const [dirty, setDirty] = useState(false);
  const [view, setView] = useState<EditorView | null>(null);
  const [previewOn, setPreviewOn] = useState(isDesktop);
  const [focus, setFocus] = useState(false);
  const [fontSize, setFontSize] = useState(isDesktop ? 14 : 16);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [restored] = useState(() =>
    draftKey ? localStorage.getItem(draftKey) !== null : false,
  );

  const save = useCallback(async () => {
    const res = await api.saveContent({
      id: page.id || undefined,
      slug,
      format,
      body,
      published,
      message: "",
    });
    onSaved(res.item);
  }, [page.id, slug, format, body, published, onSaved]);

  const { status, flush } = useAutosave(save, draftKey, body, dirty);

  const onChange = useCallback((value: string) => {
    setBody(value);
    setDirty(true);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void flush();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [flush]);

  const split = previewOn && isDesktop;

  return (
    <div className={`shell${focus ? " is-focus" : ""}${split ? " is-split" : ""}`}>
      {!focus && (
        <header className="topbar">
          <button type="button" className="icon-btn back-btn" onClick={onBack} aria-label="返回列表">‹</button>
          <button type="button" className="title-btn" onClick={() => setSettingsOpen(true)} title="页面设置">
            <span className="title-path">{slug || "未命名"}</span>
            <span className={`fmt-badge fmt-${format}`}>{format}</span>
            {!published && <span className="draft-tag">草稿</span>}
          </button>
          <span className={`status-pill st-${status}`} role="status">
            <span className="dot" aria-hidden="true" />
            {STATUS_TEXT[status]}
          </span>
          <div className="topbar-actions">
            <button type="button" className="icon-btn" onClick={() => setPreviewOn((v) => !v)} aria-pressed={previewOn} title="预览" aria-label="预览">👁</button>
            <button type="button" className="icon-btn" onClick={() => setFocus(true)} title="专注模式" aria-label="专注模式">⛶</button>
            <button type="button" className="icon-btn" onClick={onToggleTheme} title="切换主题" aria-label="切换主题">◐</button>
            <button type="button" className="btn btn-accent save-btn" onClick={() => void flush()} disabled={status === "saving"}>保存</button>
          </div>
        </header>
      )}

      {focus && (
        <button type="button" className="focus-exit" onClick={() => setFocus(false)}>退出专注 ✕</button>
      )}

      <div className="workarea">
        <div className="editor-col">
          {restored && <div className="restore-note">已恢复本地草稿，保存后清除</div>}
          <CodeEditor
            value={body}
            format={format}
            onChange={onChange}
            onViewReady={setView}
            fontSize={fontSize}
            pages={pages}
          />
        </div>
        {split && <Preview format={format} body={body} active={previewOn} />}
      </div>

      <Toolbar format={format} view={view} />

      {previewOn && !isDesktop && (
        <div className="preview-overlay">
          <div className="preview-overlay-head">
            <span>实时预览</span>
            <button type="button" className="icon-btn" onClick={() => setPreviewOn(false)} aria-label="关闭预览">✕</button>
          </div>
          <Preview format={format} body={body} active={previewOn} />
        </div>
      )}

      {settingsOpen && (
        <SettingsModal
          slug={slug}
          format={format}
          published={published}
          fontSize={fontSize}
          onSlug={setSlug}
          onFormat={setFormat}
          onPublished={setPublished}
          onFontSize={setFontSize}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </div>
  );
}

function SettingsModal({
  slug,
  format,
  published,
  fontSize,
  onSlug,
  onFormat,
  onPublished,
  onFontSize,
  onClose,
}: {
  slug: string;
  format: ContentFormat;
  published: boolean;
  fontSize: number;
  onSlug: (v: string) => void;
  onFormat: (v: ContentFormat) => void;
  onPublished: (v: boolean) => void;
  onFontSize: (v: number) => void;
  onClose: () => void;
}) {
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} role="dialog" aria-label="页面设置">
        <h2>页面设置</h2>
        <label className="field">
          <span>路径</span>
          <input value={slug} onChange={(e) => onSlug(e.target.value)} placeholder="如 xef/index.md" autoCapitalize="off" autoCorrect="off" spellCheck={false} />
        </label>
        <label className="field">
          <span>格式</span>
          <select value={format} onChange={(e) => onFormat(e.target.value as ContentFormat)}>
            {FORMATS.map((f) => (
              <option key={f} value={f}>{f}</option>
            ))}
          </select>
        </label>
        <label className="field field-row">
          <span>发布</span>
          <input type="checkbox" checked={published} onChange={(e) => onPublished(e.target.checked)} />
        </label>
        <div className="field">
          <span>字号</span>
          <div className="font-ctrl">
            <button type="button" className="icon-btn" onClick={() => onFontSize(Math.max(12, fontSize - 1))} aria-label="减小字号">A−</button>
            <span className="font-val">{fontSize}px</span>
            <button type="button" className="icon-btn" onClick={() => onFontSize(Math.min(24, fontSize + 1))} aria-label="增大字号">A＋</button>
          </div>
        </div>
        <button type="button" className="btn btn-accent modal-done" onClick={onClose}>完成</button>
      </div>
    </div>
  );
}
