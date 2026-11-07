import { useEffect, useState } from "react";
import { api, SiteSettings, User } from "./api";
import { AuthPage } from "./pages/AuthPage";
import { Shell } from "./components/Shell";
import { applyTheme, readTheme, Theme } from "./theme";

const defaultSite: SiteSettings = { site_name: "KychNet", logo_url: "", nav: [] };

export function App() {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState(true);
  // 内容站地址以 API 返回的 SITE_URL 为准；这里只是首帧渲染前的兜底，
  // 取不到时宁可不显示「前往内容端」链接，也不要指向一个错的域名。
  const [siteUrl, setSiteUrl] = useState("");
  const [site, setSite] = useState<SiteSettings>(defaultSite);
  const [theme, setTheme] = useState<Theme>(() => readTheme());
  const [avatarVersion, setAvatarVersion] = useState(0);

  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  useEffect(() => {
    api
      .me()
      .then(setUser)
      .catch(() => setUser(null))
      .finally(() => setLoading(false));
    api
      .getSettings()
      .then((result) => {
        setSiteUrl(result.site_url);
        setSite(result.site);
      })
      .catch(() => undefined);
  }, []);

  if (loading) {
    return <p className="hint">加载中…</p>;
  }
  if (!user) {
    return <AuthPage onLogin={setUser} />;
  }
  return (
    <Shell
      user={user}
      site={site}
      siteUrl={siteUrl}
      theme={theme}
      avatarVersion={avatarVersion}
      onThemeChange={setTheme}
      onUserChange={setUser}
      onAvatarChange={() => setAvatarVersion((value) => value + 1)}
      onLogout={() => setUser(null)}
    />
  );
}
