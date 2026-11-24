import { useCallback, useEffect, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { CodeEditor } from "@admin/components/CodeEditor";
import type { PageLink } from "@admin/components/editorCompletions";
import { api, type Content, type ContentFormat } from "@admin/api";
import { Toolbar } from "./Toolbar";
import { Preview } from "./Preview";
import { Modal } from "./Modal";
import { useConfirm } from "./confirm";
import { useAutosave, useMediaQuery, type SaveStatus } from "./hooks";

const FORMATS: ContentFormat[] = ["markdown", "html", "typst", "wikidot", "css", "svg"];
const STATUS_TEXT: Record<SaveStatus, string> = {
  idle: "就绪",
  dirty: "待保存",
  saving: "保存中…",
  saved: "已保存",
  error: "保存失败",
};

function draftKeyFor(page: Content): string | null {
  return page.id ? `kychnet-editor-draft:${page.id}` : null;
}

/**
 * 页面编辑器外壳。
 *
 * 实例由父组件按「打开动作」保持稳定：自动保存只更新 page 属性，不会重建
 * 本组件，因此光标 / undo / 输入内容在首次保存后仍然保留。
 * 未保存判定的快照覆盖 路径 / 格式 / 发布状态 / 正文，任一变化都会触发保存。
 */
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
  const draftKey = draftKeyFor(page);

  // 首次挂载时读一次本地草稿：与服务端内容相同就当没有草稿（顺手清掉），
  // 不同才恢复并提示。
  const [initial] = useState(() => {
    if (!draftKey) return { body: page.body, restored: false };
    try {
      const raw = localStorage.getItem(draftKey);
      if (raw === null) return { body: page.body, restored: false };
      if (raw === page.body) {
        localStorage.removeItem(draftKey);
        return { body: page.body, restored: false };
      }
      return { body: raw, restored: true };
    } catch {
      return { body: page.body, restored: false };
    }
  });

  const [body, setBody] = useState(initial.body);
  const [format, setFormat] = useState<ContentFormat>(page.format);
  const [slug, setSlug] = useState(page.slug);
  const [published, setPublished] = useState(page.published);
  const [restored, setRestored] = useState(initial.restored);
  const [view, setView] = useState<EditorView | null>(null);
  const [previewOn, setPreviewOn] = useState(isDesktop);
  const [focus, setFocus] = useState(false);
  const [fontSize, setFontSize] = useState(isDesktop ? 14 : 16);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [errorHidden, setErrorHidden] = useState(false);
  // 上次保存成功的快照：路径 / 格式 / 发布状态 / 正文任一变化都算「待保存」。
  const [savedSignature, setSavedSignature] = useState(() =>
    JSON.stringify({
      slug: page.slug,
      format: page.format,
      published: page.published,
      body: page.body,
    }),
  );
  const { confirm, confirmNode } = useConfirm();

  const signature = JSON.stringify({ slug, format, published, body });
  const dirty = signature !== savedSignature;

  const save = useCallback(async () => {
    const sent = signature;
    const res = await api.saveContent({
      id: page.id || undefined,
      slug,
      format,
      body,
      // 必须原样传回：后端把「缺省」解释为置空，漏传会清掉已有的自定义 head。
      head: page.head ?? "",
      published,
      message: "",
    });
    setSavedSignature(sent);
    setRestored(false);
    onSaved(res.item);
  }, [page.id, page.head, slug, format, body, published, signature, onSaved]);

  const { status, error, flush } = useAutosave(save, draftKey, body, dirty);

  // 新的保存失败重新弹出横幅（用户手动关掉过也要再提示）。
  useEffect(() => {
    if (error) setErrorHidden(false);
  }, [error]);

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

  // 未保存就关闭 / 刷新标签页时让浏览器确认。
  const unsaved = dirty || status === "saving" || status === "error";
  useEffect(() => {
    if (!unsaved) return;
    const handler = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = "";
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [unsaved]);

  const requestBack = useCallback(async () => {
    if (dirty || status === "saving") {
      const ok = await confirm({
        title: "离开编辑器？",
        body: "还有未保存的改动。本地草稿会保留在本机，下次打开这一页可以恢复。",
        confirmText: "离开",
        danger: true,
      });
      if (!ok) return;
    }
    onBack();
  }, [dirty, status, confirm, onBack]);

  const split = previewOn && isDesktop;
  const previewFrameRef = useRef<HTMLIFrameElement | null>(null);

  // 编辑器 → 预览的滚动同步：按滚动比例映射。两侧排版高度不同，按比例是
  // 误差最小的做法；预览重绘后（iframe 重新加载）重新对齐一次。
  useEffect(() => {
    if (!split || !view) return;
    const scroller = view.scrollDOM;
    const frame = previewFrameRef.current;
    let raf = 0;

    // 源码行与渲染高度不成比例：先按「上一个/下一个标题」锚点插值，找不到再退回比例。
    const normalizeHeading = (text: string) =>
      text.replace(/^#{1,6}\s+/, "").replace(/[`*_~]/g, "").trim();

    const headingY = (doc: Document, label: string): number | null => {
      const el = Array.from(doc.querySelectorAll("h1,h2,h3,h4,h5,h6")).find(
        (node) => normalizeHeading(node.textContent ?? "") === label,
      );
      if (!el) return null;
      const rect = el.getBoundingClientRect();
      return rect.top + doc.documentElement.scrollTop;
    };

    const closestHeading = (docText: (n: number) => string, from: number, dir: 1 | -1, limit: number) => {
      for (let n = from, i = 0; i < limit && n >= 1 && n <= view.state.doc.lines; n += dir, i++) {
        const text = docText(n);
        if (/^#{1,6}\s+\S/.test(text)) return { line: n, label: normalizeHeading(text) };
      }
      return null;
    };

    const applyPreviewScroll = () => {
      const doc = frame?.contentDocument;
      if (!doc) return;
      const root = doc.documentElement;
      const editorMax = scroller.scrollHeight - scroller.clientHeight;
      if (editorMax <= 0) return;
      const ratio = Math.min(1, Math.max(0, scroller.scrollTop / editorMax));

      const src = view.state.doc;
      const topLine = src.lineAt(view.viewport.from).number;
      const docText = (n: number) => src.line(n).text;
      const prev = closestHeading(docText, topLine, -1, 300);
      const next = closestHeading(docText, topLine + 1, 1, 300);
      const yPrev = prev ? headingY(doc, prev.label) : null;
      const yNext = next ? headingY(doc, next.label) : null;

      if (yPrev !== null && yNext !== null && prev && next) {
        const span = Math.max(1, next.line - prev.line);
        const t = Math.min(1, Math.max(0, (topLine - prev.line) / span));
        frame?.contentWindow?.scrollTo(0, Math.round(yPrev + (yNext - yPrev) * t));
        return;
      }
      if (yPrev !== null) {
        frame?.contentWindow?.scrollTo(0, Math.round(yPrev));
        return;
      }
      if (yNext !== null) {
        frame?.contentWindow?.scrollTo(0, Math.round(yNext));
        return;
      }

      const previewMax = root.scrollHeight - root.clientHeight;
      if (previewMax <= 0) return;
      frame?.contentWindow?.scrollTo(0, Math.round(ratio * previewMax));
    };

    const onScroll = () => {
      if (raf) return;
      raf = window.requestAnimationFrame(() => {
        raf = 0;
        applyPreviewScroll();
      });
    };

    scroller.addEventListener("scroll", onScroll, { passive: true });
    frame?.addEventListener("load", applyPreviewScroll);
    return () => {
      scroller.removeEventListener("scroll", onScroll);
      frame?.removeEventListener("load", applyPreviewScroll);
      if (raf) window.cancelAnimationFrame(raf);
    };
  }, [split, view]);

  return (
    <div className={`shell${focus ? " is-focus" : ""}${split ? " is-split" : ""}`}>
      {!focus && (
        <header className="topbar">
          <button type="button" className="icon-btn back-btn" onClick={() => void requestBack()} aria-label="返回列表">‹</button>
          <button type="button" className="title-btn" onClick={() => setSettingsOpen(true)} title="页面设置">
            <span className="title-path">{slug || "未命名"}</span>
            <span className={`fmt-badge fmt-${format}`}>{format}</span>
            {!published && <span className="draft-tag">草稿</span>}
          </button>
          <span className={`status-pill st-${status}`} role="status" aria-live="polite">
            <span className="dot" aria-hidden="true" />
            {STATUS_TEXT[status]}
          </span>
          <div className="topbar-actions">
            <button type="button" className="icon-btn" onClick={() => setPreviewOn((v) => !v)} aria-pressed={previewOn} title="预览" aria-label="预览">👁</button>
            <button type="button" className="icon-btn" onClick={() => setFocus(true)} title="专注模式" aria-label="专注模式">⛶</button>
            <button type="button" className="icon-btn" onClick={onToggleTheme} title={theme === "dark" ? "切换为浅色主题" : "切换为深色主题"} aria-label={theme === "dark" ? "切换为浅色主题" : "切换为深色主题"}>◐</button>
            <button type="button" className="btn btn-accent save-btn" onClick={() => void flush()} disabled={status === "saving"}>保存</button>
          </div>
        </header>
      )}

      {/* 失败横幅跟随最近一次错误：即使随后继续输入（状态回到「待保存」）也仍可见，直到保存成功。 */}
      {(error || status === "error") && !errorHidden && (
        <div className="save-error" role="alert">
          <span className="save-error-text">
            保存失败{error ? `：${error}` : ""}。改动仍在本机草稿里，可重试。
          </span>
          <button type="button" className="btn btn-accent save-error-retry" onClick={() => void flush()}>
            重试保存
          </button>
          <button
            type="button"
            className="icon-btn save-error-close"
            aria-label="关闭保存失败提示"
            onClick={() => setErrorHidden(true)}
          >
            ✕
          </button>
        </div>
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
            onChange={setBody}
            onViewReady={setView}
            fontSize={fontSize}
            pages={pages}
          />
        </div>
        {split && <Preview ref={previewFrameRef} format={format} body={body} active={previewOn} />}
      </div>

      <Toolbar format={format} view={view} />

      {previewOn && !isDesktop && (
        <Modal
          title="实时预览"
          onClose={() => setPreviewOn(false)}
          className="modal-preview"
        >
          <Preview format={format} body={body} active={previewOn} />
        </Modal>
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

      {confirmNode}
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
    <Modal title="页面设置" onClose={onClose}>
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
    </Modal>
  );
}
