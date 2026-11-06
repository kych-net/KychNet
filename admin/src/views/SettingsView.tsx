import { useEffect, useState } from "react";
import { api, NavLink } from "../api";

export function SettingsView() {
  const [siteName, setSiteName] = useState("");
  const [logoUrl, setLogoUrl] = useState("");
  const [nav, setNav] = useState<NavLink[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  useEffect(() => {
    api
      .getSettings()
      .then((result) => {
        setSiteName(result.site.site_name);
        setLogoUrl(result.site.logo_url);
        setNav(result.site.nav);
      })
      .catch((e) => setError((e as Error).message));
  }, []);

  function updateLink(index: number, field: keyof NavLink, value: string) {
    setNav((links) => links.map((link, i) => (i === index ? { ...link, [field]: value } : link)));
  }

  async function save(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      await api.saveSettings({ site_name: siteName, logo_url: logoUrl, nav });
      setNotice("站点设置已保存,重新构建后生效");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>站点设置</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form onSubmit={save}>
        <div className="row">
          <label>
            站点名称
            <input value={siteName} onChange={(e) => setSiteName(e.target.value)} required />
          </label>
          <label>
            图标 URL
            <input
              value={logoUrl}
              onChange={(e) => setLogoUrl(e.target.value)}
              placeholder="留空则显示站点名首字"
            />
          </label>
        </div>

        <h3>顶栏链接</h3>
        {nav.map((link, index) => (
          <div className="row align" key={index}>
            <label>
              标签
              <input value={link.label} onChange={(e) => updateLink(index, "label", e.target.value)} />
            </label>
            <label>
              地址
              <input value={link.href} onChange={(e) => updateLink(index, "href", e.target.value)} />
            </label>
            <button type="button" onClick={() => setNav(nav.filter((_, i) => i !== index))}>
              删除
            </button>
          </div>
        ))}
        <button type="button" onClick={() => setNav([...nav, { label: "", href: "" }])}>
          新增链接
        </button>
        <div className="row align">
          <button type="submit">保存</button>
        </div>
      </form>
    </section>
  );
}
