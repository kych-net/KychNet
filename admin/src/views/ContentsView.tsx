import { useEffect, useState } from "react";
import { api, Content, ContentFormat } from "../api";

export function ContentsView() {
  const [items, setItems] = useState<Content[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const [body, setBody] = useState("");
  const [published, setPublished] = useState(false);

  useEffect(() => {
    void refresh();
  }, []);

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
      await api.saveContent({ slug, title, format, body, published });
      setNotice("内容已保存");
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
    setSlug(item.slug);
    setTitle(item.title);
    setFormat(item.format);
    setBody(item.body);
    setPublished(item.published);
  }

  return (
    <section>
      <h2>内容</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form onSubmit={save}>
        <div className="row">
          <label>
            Slug
            <input value={slug} onChange={(e) => setSlug(e.target.value)} required />
          </label>
          <label>
            标题
            <input value={title} onChange={(e) => setTitle(e.target.value)} required />
          </label>
          <label>
            格式
            <select value={format} onChange={(e) => setFormat(e.target.value as ContentFormat)}>
              <option value="markdown">Markdown</option>
              <option value="html">HTML</option>
              <option value="typst">Typst</option>
            </select>
          </label>
        </div>
        <label>
          正文
          <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={16} />
        </label>
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
        </div>
      </form>

      <table>
        <thead>
          <tr>
            <th>标题</th>
            <th>Slug</th>
            <th>格式</th>
            <th>状态</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {items.map((item) => (
            <tr key={item.id}>
              <td>{item.title}</td>
              <td className="mono small">{item.slug}</td>
              <td>{item.format}</td>
              <td>{item.published ? "已发布" : "草稿"}</td>
              <td>
                <button onClick={() => edit(item)}>编辑</button>
                <button onClick={() => remove(item.id)}>删除</button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
