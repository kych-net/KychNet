import { lazy, Suspense, useEffect, useState } from "react";
import { api, SiteSettings, User } from "./api";
import { AuthPage } from "./pages/AuthPage";
import { applyTheme, readTheme, Theme } from "./theme";

// 登录后再加载管理端 Shell，避免把 CodeMirror 等重依赖塞进首屏。
const Shell = lazy(() => import("./components/Shell").then((module) => ({ default: module.Shell })));

const defaultSite: SiteSettings = {
  site_name: "KychNet",
  description: "",
  logo_url: "",
  nav: [],
  icp_beian: "",
  police_beian: "",
  police_beian_code: "",
};

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
    <Suspense fallback={<p className="hint">加载管理端…</p>}>
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
    </Suspense>
  );
}
