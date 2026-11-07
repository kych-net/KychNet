import { useEffect, useState } from "react";
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
  }
}

function extensionFor(format: ContentFormat): string {
  return format === "markdown" ? ".md" : format === "typst" ? ".typ" : format === "wikidot" ? ".wj" : format === "css" ? ".css" : ".html";
}

export function ContentsView({ editContentId, onEditHandled }: Props) {
  const [items, setItems] = useState<Content[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [contentId, setContentId] = useState<string | null>(null);
  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const [body, setBody] = useState("");
  const [published, setPublished] = useState(false);

  useEffect(() => {
    void refresh();
  }, []);

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
        title,
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
    setTitle(item.title);
    setFormat(item.format);
    setBody(item.body);
    setPublished(item.published);
  }

  function changeFormat(next: ContentFormat) {
    setFormat(next);
    setSlug((current) => current.replace(/\.(html|md|typ)$/i, extensionFor(next)));
  }

  function newContent() {
    setContentId(null);
    setSlug("");
    setTitle("");
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
            标题
            <input value={title} onChange={(e) => setTitle(e.target.value)} required />
          </label>
          <label>
            格式
            <select value={format} onChange={(e) => changeFormat(e.target.value as ContentFormat)}>
              <option value="markdown">Markdown</option>
              <option value="html">HTML</option>
              <option value="typst">Typst</option>
              <option value="wikidot">Wikidot</option>
              <option value="css">CSS</option>
            </select>
          </label>
        </div>
        <p className="hint">
          目录主页必须写 <code>/eoe/index</code>，不再接受 <code>/eoe/</code>；普通页面用 <code>/reff/93df/l.html</code>。
          <code>.html/.md/.typ/.wj</code> 最终都输出同一个 <code>.html</code>，同一路径只保留一条内容。
          <code>/api</code> 是保留路径；<code>/index.html</code> 是主页，只能编辑不能删除。
          CSS 文件必须使用 <code>/css/xxx</code> 路径和 CSS 格式；页面正文最前面写
          <code>---</code>、<code>css: /css/xxx.css</code>、<code>---</code> 才会加载它。
        </p>
        <div className="editor-shell">
          <div className="editor-toolbar">
            <span>正文</span>
            <span>
              {formatLabel(format)} · {body.length} 字符
            </span>
          </div>
          <CodeEditor value={body} format={format} onChange={setBody} />
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

      <table>
        <thead>
          <tr>
            <th>标题</th>
            <th>路径</th>
            <th>格式</th>
            <th>状态</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {items.map((item) => (
            <tr key={item.id}>
              <td>{item.title}</td>
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
    </section>
  );
}
