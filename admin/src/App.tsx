import { lazy, Suspense, useEffect, useState } from "react";
import { api, SiteSettings, User } from "./api";
import { AuthPage } from "./pages/AuthPage";
import { applyTheme, readTheme, Theme } from "./theme";

// 登录后再加载管理端 Shell，避免把 CodeMirror 等重依赖塞进首屏。
const Shell = lazy(() => import("./components/Shell").then((module) => ({ default: module.Shell })));

const defaultSite: SiteSettings = {
  // 站点名以 /api/settings 返回为准；留空让标题与页眉先呈现中性文案，
  // 接口失败也不至于显示错误品牌。
  site_name: "",
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

  // 标题跟随站点设置，不再硬编码品牌名；PWA 加到主屏后的名称同源。
  useEffect(() => {
    document.title = site.site_name ? `${site.site_name} 管理` : "管理";
  }, [site.site_name]);

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
