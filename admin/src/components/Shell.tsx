import { lazy, Suspense, useEffect, useState } from "react";
import { api, avatarUrl, SiteSettings, User } from "../api";
import { Theme } from "../theme";
const ContentsView = lazy(() => import("../views/ContentsView").then((module) => ({ default: module.ContentsView })));
const InvitesView = lazy(() => import("../views/InvitesView").then((module) => ({ default: module.InvitesView })));
const ProfileView = lazy(() => import("../views/ProfileView").then((module) => ({ default: module.ProfileView })));
const ProjectsView = lazy(() => import("../views/ProjectsView").then((module) => ({ default: module.ProjectsView })));
const SettingsView = lazy(() => import("../views/SettingsView").then((module) => ({ default: module.SettingsView })));
const RunnersView = lazy(() => import("../views/RunnersView").then((module) => ({ default: module.RunnersView })));
const UsersView = lazy(() => import("../views/UsersView").then((module) => ({ default: module.UsersView })));
const SshKeysView = lazy(() => import("../views/SshKeysView").then((module) => ({ default: module.SshKeysView })));

type View = "contents" | "projects" | "runners" | "invites" | "users" | "settings" | "profile";

type Props = {
  user: User;
  site: SiteSettings;
  siteUrl: string;
  theme: Theme;
  avatarVersion: number;
  onThemeChange: (theme: Theme) => void;
  onUserChange: (user: User) => void;
  onAvatarChange: () => void;
  onLogout: () => void;
};

const adminViews: View[] = ["runners", "invites", "users", "settings"];

const viewMeta: Record<View, { title: string; subtitle: string }> = {
  contents: { title: "内容", subtitle: "维护页面、样式与发布状态" },
  projects: { title: "项目", subtitle: "站内 Git 仓库、分支与协作" },
  runners: { title: "执行器", subtitle: "全站共享的工作流执行器与注册码" },
  invites: { title: "邀请码", subtitle: "控制注册入口与有效期" },
  users: { title: "用户", subtitle: "账号状态与权限" },
  settings: { title: "站点设置", subtitle: "站点信息、导航与发布" },
  profile: { title: "个人资料", subtitle: "头像、Git 令牌与安全设置" },
};

export function Shell({
  user,
  site,
  siteUrl,
  theme,
  avatarVersion,
  onThemeChange,
  onUserChange,
  onAvatarChange,
  onLogout,
}: Props) {
  const [view, setView] = useState<View>("contents");
  const [menuOpen, setMenuOpen] = useState(false);
  const [avatarFailed, setAvatarFailed] = useState(false);
  const initial = user.username.slice(0, 1).toUpperCase();

  useEffect(() => {
    setAvatarFailed(false);
  }, [avatarVersion, user.id]);

  function go(next: View) {
    setView(next);
    setMenuOpen(false);
  }

  function switchTheme() {
    onThemeChange(theme === "dark" ? "light" : "dark");
  }

  return (
    <div className="shell">
      <header className="appbar">
        <a
          className="brand"
          href={siteUrl || undefined}
          target="_blank"
          rel="noreferrer"
          title={site.site_name}
        >
          {site.logo_url ? (
            <img src={site.logo_url} alt={site.site_name} />
          ) : (
            <span className="brand-mark">{site.site_name.slice(0, 1)}</span>
          )}
          <span className="brand-name">{site.site_name}</span>
        </a>

        <nav className="appnav">
          <button className={view === "contents" ? "active" : ""} onClick={() => go("contents")}>
            内容
          </button>
          <button className={view === "projects" ? "active" : ""} onClick={() => go("projects")}>
            项目
          </button>
          {user.is_admin &&
            adminViews.map((item) => (
              <button
                key={item}
                className={view === item ? "active" : ""}
                onClick={() => go(item)}
              >
                {item === "runners" ? "执行器" : item === "invites" ? "邀请码" : item === "users" ? "用户" : "站点设置"}
              </button>
            ))}
        </nav>

        <div className="appactions">
          <button className="theme-button" onClick={switchTheme}>
            {theme === "dark" ? "浅色" : "深色"}
          </button>
          <div className="avatar-wrap">
            <button
              className="avatar"
              onClick={() => setMenuOpen((open) => !open)}
              aria-label="用户菜单"
            >
              {avatarFailed ? (
                initial
              ) : (
                <img
                  className="avatar-img"
                  src={avatarUrl(user.id, avatarVersion)}
                  alt=""
                  onError={() => setAvatarFailed(true)}
                />
              )}
            </button>
            {menuOpen && (
              <div className="avatar-menu">
                <button onClick={() => go("profile")}>用户设置</button>
                {user.is_admin && <button onClick={() => go("contents")}>网站管理</button>}
                {siteUrl && (
                  <a href={siteUrl} target="_blank" rel="noreferrer">
                    前往内容端
                  </a>
                )}
                <button
                  onClick={async () => {
                    await api.logout().catch(() => undefined);
                    onLogout();
                  }}
                >
                  退出登录
                </button>
              </div>
            )}
          </div>
        </div>

      </header>

      <main className="appmain">
        <div className="page-head">
          <div>
            <span className="page-eyebrow">KychNet Admin</span>
            <h1>{viewMeta[view].title}</h1>
            <p>{viewMeta[view].subtitle}</p>
          </div>
          <span className="page-badge">{user.is_admin ? "管理员" : "编辑者"}</span>
        </div>
        <Suspense fallback={<p className="hint">加载视图…</p>}>
        {view === "contents" && <ContentsView />}
        {view === "projects" && <ProjectsView isAdmin={user.is_admin} />}
        {view === "runners" && user.is_admin && <RunnersView />}
        {view === "invites" && user.is_admin && <InvitesView />}
        {view === "users" && user.is_admin && <UsersView />}
        {view === "settings" && user.is_admin && <SettingsView />}
        {view === "profile" && (
          <ProfileView
            user={user}
            avatarVersion={avatarVersion}
            onUserChange={onUserChange}
            onAvatarChange={onAvatarChange}
          />
        )}
        </Suspense>
      </main>
    </div>
  );
}
