import { useEffect, useState } from "react";
import { api, SiteSettings, User } from "./api";
import { AuthPage } from "./pages/AuthPage";
import { Shell } from "./components/Shell";
import { applyTheme, readTheme, Theme } from "./theme";

const defaultSite: SiteSettings = { site_name: "KychNet", logo_url: "", nav: [] };

export function App() {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState(true);
  const [siteUrl, setSiteUrl] = useState("https://www.kych.net");
  const [site, setSite] = useState<SiteSettings>(defaultSite);
  const [theme, setTheme] = useState<Theme>(() => readTheme());

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
      onThemeChange={setTheme}
      onUserChange={setUser}
      onLogout={() => setUser(null)}
    />
  );
}
