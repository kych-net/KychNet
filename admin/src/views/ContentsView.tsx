import { useEffect, useRef, useState } from "react";
import { marked } from "marked";
import { api, Content, ContentFormat, editorPath } from "../api";
import { CodeEditor } from "../components/CodeEditor";
import { DirTree } from "../components/DirTree";
import { RevisionPanel } from "../components/RevisionPanel";

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

/// 会产出 HTML 页面的格式才支持自定义 <head>；css / svg 是静态资源，没有页面结构。
function rendersHtml(format: ContentFormat): boolean {
  return (
    format === "markdown" || format === "html" || format === "wikidot" || format === "typst"
  );
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

export function ContentsView() {
  const [items, setItems] = useState<Content[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [contentId, setContentId] = useState<string | null>(null);
  const [slug, setSlug] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const [body, setBody] = useState("");
  // 页面自定义 <head>：只有会产出 HTML 的格式才显示这个编辑区。
  const [head, setHead] = useState("");
  const [published, setPublished] = useState(false);
  const [message, setMessage] = useState("");
  const [previewOn, setPreviewOn] = useState(true);
  const [previewHtml, setPreviewHtml] = useState("");
  const [previewError, setPreviewError] = useState("");
  const [previewLoading, setPreviewLoading] = useState(false);
  // 窄屏三选一视图：编辑器（默认）/ 目录 / Git 区域。
  const [mobilePane, setMobilePane] = useState<"editor" | "tree" | "git">("editor");
  // 保存/删除会让右栏 Git 区域的版本历史重新拉取（contentId 不变时也需要）。
  const [historyKey, setHistoryKey] = useState(0);

  // 实时跟踪最新的正文与路径：保存响应回来时比对提交时的快照，
  // 判断用户是否已继续输入。渲染期同步，避免 effect 延迟带来的竞态窗口。
  const bodyRef = useRef(body);
  const slugRef = useRef(slug);
  bodyRef.current = body;
  slugRef.current = slug;

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
    // 记录提交时的内容快照，响应回来时据此判断是否回写。
    const submittedBody = body;
    const submittedSlug = slug;
    try {
      const result = await api.saveContent({
        id: contentId ?? undefined,
        slug,
        format,
        body,
        head,
        published,
        message: message.trim() || undefined,
      });
      // 服务端会把 /xef/index、/eoe/index.md 归一化成输出路径，这里回显源文件路径。
      // 守卫：保存后用户已继续输入（正文或路径变了）就丢弃回写，
      // 避免慢响应覆盖新输入；同一路径只保留一条内容，下次保存仍会命中它。
      if (bodyRef.current === submittedBody && slugRef.current === submittedSlug) {
        setContentId(result.item.id);
        setSlug(editorPath(result.item));
        setMessage("");
      }
      setNotice(published ? "内容已保存，并已自动编译当前页" : "内容已保存（草稿，暂不生成页面）");
      await refresh();
      setHistoryKey((value) => value + 1);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteContent(id);
      if (id === contentId) newContent();
      await refresh();
      setHistoryKey((value) => value + 1);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function edit(item: Content) {
    setContentId(item.id);
    setSlug(editorPath(item));
    setFormat(item.format);
    setBody(item.body);
    setHead(item.head ?? "");
    setPublished(item.published);
    setMessage("");
  }

  function changeFormat(next: ContentFormat) {
    setFormat(next);
    setSlug((current) => current.replace(/\.(html|md|typ|wj|svg)$/i, extensionFor(next)));
  }

  function editFromTree(item: Content) {
    edit(item);
    // 从目录打开文件后回到编辑器视图。
    setMobilePane("editor");
  }

  function newContent() {
    setMobilePane("editor");
    setContentId(null);
    setSlug("");
    setFormat("markdown");
    setBody("");
    setHead("");
    setPublished(false);
    setMessage("");
    setError("");
    setNotice("");
  }

  return (
    <section>
      <h2>内容</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      {/* 窄屏选项卡：一次只显示一个视图，默认编辑器；宽屏三栏同屏不显示。 */}
      <div className="contents-mobile-tabs" role="tablist" aria-label="内容视图">
        {(
          [
            ["editor", "编辑器"],
            ["tree", "目录"],
            ["git", "Git"],
          ] as const
        ).map(([pane, label]) => (
          <button
            key={pane}
            type="button"
            role="tab"
            aria-selected={mobilePane === pane}
            className={mobilePane === pane ? "active" : ""}
            onClick={() => setMobilePane(pane)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="contents-layout" data-pane={mobilePane}>
        <aside className="contents-tree">
          <div className="contents-tree-head">
            <div>
              <span className="contents-tree-kicker">URL</span>
              <strong>内容树</strong>
            </div>
            <span className="contents-tree-count">{items.length}</span>
          </div>
          <DirTree items={items} onEdit={editFromTree} />
        </aside>

        <div className="contents-main">
      <form id="content-editor-form" onSubmit={save}>
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
              {rendersHtml(format) && (
                <details className="head-editor" open={head.trim() !== ""}>
                  <summary>
                    页面 &lt;head&gt; 自定义
                    <span className="hint">
                      {head.trim() ? `已设置 ${head.trim().length} 字符` : "未设置"}
                    </span>
                  </summary>
                  <p className="hint">
                    原样注入到 &lt;/head&gt; 之前，可写 meta、link、script 等标签；
                    只影响当前这一页。css / svg 类内容没有页面结构，不提供此项。
                  </p>
                  <textarea
                    className="head-editor-input"
                    value={head}
                    spellCheck={false}
                    placeholder={'<meta name="robots" content="noindex" />'}
                    onChange={(event) => setHead(event.target.value)}
                  />
                </details>
              )}
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

      {/* 全站清单默认收起：当前文件的版本历史由右栏 Git 区域承担。 */}
      <details className="contents-all">
        <summary>全部内容（{items.length}）· 编辑与删除</summary>
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
      </details>
        </div>
        <aside className="contents-aux">
          {/* 内容页的所有 Git 操作集中在这一栏：提交说明 + 版本历史 / 对比 / 快照 / 回滚。
              输入框用 form 属性关联回编辑器表单，按 Enter 与表单内的行为保持一致。 */}
          <div className="contents-git">
            <div className="contents-git-head">
              <div>
                <span className="contents-tree-kicker">GIT</span>
                <strong>Git</strong>
              </div>
              <span className="contents-git-badge">版本与提交</span>
            </div>
            <div className="contents-git-commit">
              <label htmlFor="content-commit-message">
                版本说明
                <input
                  id="content-commit-message"
                  form="content-editor-form"
                  value={message}
                  onChange={(e) => setMessage(e.target.value)}
                  placeholder="可选，保存时记入版本历史"
                />
              </label>
              <p className="hint">
                在编辑器点「保存」时，说明随新版本一起记录；正文或格式没有变化则不会产生新版本。
              </p>
            </div>
            <RevisionPanel
              contentId={contentId}
              refreshKey={historyKey}
              onReverted={(item) => {
                // 回滚是移动指针：编辑器同步显示回滚后的正文与格式。
                setContentId(item.id);
                setSlug(editorPath(item));
                setFormat(item.format);
                setBody(item.body);
                setHead(item.head ?? "");
                setPublished(item.published);
                setNotice(`已回滚到版本 ${item.rev_seq}`);
                setHistoryKey((value) => value + 1);
                void refresh();
              }}
            />
          </div>
        </aside>
      </div>
    </section>
  );
}
