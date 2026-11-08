import { useEffect, useState } from "react";
import { marked } from "marked";
import { api, Content, ContentFormat, editorPath } from "../api";
import { CodeEditor } from "../components/CodeEditor";

type Props = {
  editContentId?: string | null;
  onEditHandled?: () => void;
};

function formatLabel(format: ContentFormat): string {
  switch (format) {
    case "markdown":
      return "Markdown";
    case "html":
      return "HTML";
    case "typst":
      return "Typst";
    case "wikidot":
      return "Wikidot";
    case "css":
      return "CSS";
    case "svg":
      return "SVG";
  }
}

function extensionFor(format: ContentFormat): string {
  return format === "markdown" ? ".md" : format === "typst" ? ".typ" : format === "wikidot" ? ".wj" : format === "css" ? ".css" : format === "svg" ? ".svg" : ".html";
}

function stripPreviewHeader(body: string): string {
  const match = body.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?/);
  if (!match || !/^\s*(css|styles)\s*:/im.test(match[1])) {
    return body;
  }
  return body.slice(match[0].length);
}

function previewDocument(html: string): string {
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<style>
  :root { color-scheme: light; }
  body { margin: 0; padding: 1rem; color: #1a1a1a; background: #fff; font-family: system-ui, -apple-system, "Segoe UI", sans-serif; line-height: 1.7; }
  .preview-content { overflow-wrap: anywhere; }
  .preview-content img { max-width: 100%; height: auto; }
  .preview-content table { width: 100%; border-collapse: collapse; }
  .preview-content th, .preview-content td { border: 1px solid #ddd; padding: .4rem .55rem; text-align: left; }
  .preview-content pre { overflow-x: auto; padding: .8rem; background: #f6f7f9; border: 1px solid #e1e4e8; border-radius: 6px; }
  .preview-content blockquote { margin-left: 0; padding-left: .8rem; border-left: 3px solid #ddd; color: #555; }
  .preview-content .wiki-note { padding: .75rem 1rem; border-left: 4px solid #2f7d5c; background: #eef7f1; border-radius: 6px; }
  .preview-content .wj-code { display: block; margin: 1rem 0; border: 1px solid #ddd; border-radius: 6px; overflow: hidden; }
  .preview-content .wj-code-panel { display: flex; justify-content: space-between; padding: .3rem .65rem; background: #f6f7f9; border-bottom: 1px solid #ddd; font-size: .8rem; }
  .preview-content .wj-code pre { margin: 0; border: 0; border-radius: 0; }
  .preview-content .wj-collapsible { margin: 1rem 0; border: 1px solid #ddd; border-radius: 6px; }
  .preview-content .wj-collapsible-button { padding: .5rem .7rem; background: #f6f7f9; cursor: pointer; }
  .preview-content .wj-collapsible-content { padding: .65rem .8rem; }
  .preview-content .wj-footnote-ref-marker, .preview-content .wj-footnote-list-item-marker { color: #1f6feb; }
  .preview-content .wj-footnote-list { margin-top: 1.5rem; padding-top: .75rem; border-top: 1px solid #ddd; font-size: .9rem; }
  .preview-content .wj-math-ml { display: inline-block; }
</style>
</head>
<body><article class="preview-content">${html}</article></body>
</html>`;
}

export function ContentsView({ editContentId, onEditHandled }: Props) {
  const [items, setItems] = useState<Content[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [contentId, setContentId] = useState<string | null>(null);
  const [slug, setSlug] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const [body, setBody] = useState("");
  const [published, setPublished] = useState(false);
  const [previewOn, setPreviewOn] = useState(true);
  const [previewHtml, setPreviewHtml] = useState("");
  const [previewError, setPreviewError] = useState("");
  const [previewLoading, setPreviewLoading] = useState(false);

  useEffect(() => {
    void refresh();
  }, []);
  const previewSupported =
    format === "markdown" || format === "html" || format === "wikidot" || format === "svg";

  useEffect(() => {
    if (!previewOn || !previewSupported) {
      setPreviewHtml("");
      setPreviewError("");
      setPreviewLoading(false);
      return;
    }
    // SVG 直接交给 iframe；Markdown 和 HTML 用浏览器端渲染，不走 API。
    if (format === "svg") {
      setPreviewHtml(body);
      setPreviewError("");
      setPreviewLoading(false);
      return;
    }
    if (format === "html" || format === "markdown") {
      try {
        const source = stripPreviewHeader(body);
        const html =
          format === "html"
            ? source
            : (marked.parse(source, {
                async: false,
                gfm: true,
                breaks: false,
              }) as string);
        setPreviewHtml(html);
        setPreviewError("");
      } catch (e) {
        setPreviewHtml("");
        setPreviewError((e as Error).message);
      }
      setPreviewLoading(false);
      return;
    }

    // Wikidot 需要 ftml，继续走服务端预览，并做防抖。
    let cancelled = false;
    setPreviewLoading(true);
    const timer = window.setTimeout(() => {
      api
        .previewContent(format, body)
        .then((result) => {
          if (!cancelled) {
            setPreviewHtml(result.html);
            setPreviewError("");
          }
        })
        .catch((e) => {
          if (!cancelled) {
            setPreviewHtml("");
            setPreviewError((e as Error).message);
          }
        })
        .finally(() => {
          if (!cancelled) setPreviewLoading(false);
        });
    }, 350);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [body, format, previewOn, previewSupported]);


  useEffect(() => {
    if (!editContentId) {
      return;
    }
    const item = items.find((entry) => entry.id === editContentId);
    if (item) {
      edit(item);
      onEditHandled?.();
    }
  }, [editContentId, items, onEditHandled]);

  async function refresh() {
    try {
      setItems((await api.contents()).items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function save(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      const result = await api.saveContent({
        id: contentId ?? undefined,
        slug,
        format,
        body,
        published,
      });
      // 服务端会把 /xef/index、/eoe/index.md 归一化成输出路径，这里回显源文件路径。
      setContentId(result.item.id);
      setSlug(editorPath(result.item));
      setNotice(published ? "内容已保存，并已自动编译当前页" : "内容已保存（草稿，暂不生成页面）");
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteContent(id);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function edit(item: Content) {
    setContentId(item.id);
    setSlug(editorPath(item));
    setFormat(item.format);
    setBody(item.body);
    setPublished(item.published);
  }

  function changeFormat(next: ContentFormat) {
    setFormat(next);
    setSlug((current) => current.replace(/\.(html|md|typ|wj|svg)$/i, extensionFor(next)));
  }

  function newContent() {
    setContentId(null);
    setSlug("");
    setFormat("markdown");
    setBody("");
    setPublished(false);
    setError("");
    setNotice("");
  }

  return (
    <section>
      <h2>内容</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form onSubmit={save}>
        <div className="row">
          <label>
            路径
            <input
              value={slug}
              onChange={(e) => setSlug(e.target.value)}
              placeholder="/xef/index 或 /reff/93df/l.html"
              required
            />
          </label>
          <label>
            格式
            <select value={format} onChange={(e) => changeFormat(e.target.value as ContentFormat)}>
              <option value="markdown">Markdown</option>
              <option value="html">HTML</option>
              <option value="typst">Typst</option>
              <option value="wikidot">Wikidot</option>
              <option value="css">CSS</option>
              <option value="svg">SVG</option>
            </select>
          </label>
        </div>
        <p className="hint">
          目录主页必须写 <code>/eoe/index</code>，不再接受 <code>/eoe/</code>；普通页面用 <code>/reff/93df/l.html</code>。
          <code>.html/.md/.typ/.wj</code> 最终都输出同一个 <code>.html</code>，同一路径只保留一条内容。
          <code>/api</code> 是保留路径；<code>/index.html</code> 是主页，只能编辑不能删除。
          SVG 文件使用 <code>/media/xxx.svg</code> 路径和 SVG 格式，原样输出，不套页面模板。
          CSS 文件必须使用 <code>/css/xxx</code> 路径和 CSS 格式；页面正文最前面写
          <code>---</code>、<code>css: /css/xxx.css</code>、<code>---</code> 才会加载它。
        </p>
        <div className="editor-shell">
          <div className="editor-toolbar">
            <span>正文</span>
            <span className="editor-toolbar-right">
              {formatLabel(format)} · {body.length} 字符
              {previewSupported && (
                <button
                  type="button"
                  className="preview-toggle"
                  onClick={() => setPreviewOn((value) => !value)}
                >
                  {previewOn ? "隐藏预览" : "实时预览"}
                </button>
              )}
            </span>
          </div>
          <div className={`editor-workbench ${previewOn && previewSupported ? "with-preview" : ""}`}>
            <div className="editor-pane">
              <CodeEditor value={body} format={format} onChange={setBody} />
            </div>
            {previewOn && previewSupported && (
              <div className="preview-pane">
                <div className="preview-head">
                  <span>预览</span>
                  {previewLoading && <span>渲染中…</span>}
                </div>
                {previewError ? (
                  <p className="error preview-error">{previewError}</p>
                ) : (
                  <iframe
                    className="preview-frame"
                    title="内容预览"
                    sandbox=""
                    srcDoc={previewDocument(previewHtml)}
                  />
                )}
              </div>
            )}
            {previewOn && !previewSupported && (
              <div className="preview-empty">
                实时预览目前支持 Markdown、HTML 和 Wikidot；Typst / CSS 请保存后查看正式页面。
              </div>
            )}
          </div>
        </div>
        <div className="row align">
          <label className="checkbox">
            <input
              type="checkbox"
              checked={published}
              onChange={(e) => setPublished(e.target.checked)}
            />
            发布
          </label>
          <button type="submit">保存</button>
          <button type="button" onClick={newContent}>
            新建内容
          </button>
        </div>
      </form>

      <div className="table-wrap">
      <table>
        <thead>
          <tr>
            <th>路径</th>
            <th>格式</th>
            <th>状态</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {items.map((item) => (
            <tr key={item.id}>
              <td className="mono small">{editorPath(item)}</td>
              <td>{item.format}</td>
              <td>{item.published ? "已发布" : "草稿"}</td>
              <td>
                <button onClick={() => edit(item)}>编辑</button>
                {item.slug === "index.html" ? (
                  <span className="hint"> 主页不可删除</span>
                ) : (
                  <button onClick={() => remove(item.id)}>删除</button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}
