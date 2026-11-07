import { lazy, Suspense, useEffect, useState } from "react";
import { api, avatarUrl, Content, SiteSettings, User } from "../api";
import { DirTree } from "./DirTree";
import { Theme } from "../theme";
const ContentsView = lazy(() => import("../views/ContentsView").then((module) => ({ default: module.ContentsView })));
const InvitesView = lazy(() => import("../views/InvitesView").then((module) => ({ default: module.InvitesView })));
const ProfileView = lazy(() => import("../views/ProfileView").then((module) => ({ default: module.ProfileView })));
const ProjectsView = lazy(() => import("../views/ProjectsView").then((module) => ({ default: module.ProjectsView })));
const SettingsView = lazy(() => import("../views/SettingsView").then((module) => ({ default: module.SettingsView })));
const UsersView = lazy(() => import("../views/UsersView").then((module) => ({ default: module.UsersView })));

type View = "contents" | "projects" | "invites" | "users" | "settings" | "profile";

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

const adminViews: View[] = ["invites", "users", "settings"];

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
  const [treeOpen, setTreeOpen] = useState(false);
  const [contents, setContents] = useState<Content[]>([]);
  const [editContentId, setEditContentId] = useState<string | null>(null);
  const initial = user.username.slice(0, 1).toUpperCase();

  useEffect(() => {
    setAvatarFailed(false);
  }, [avatarVersion, user.id]);

  useEffect(() => {
    void loadContents();
  }, []);

  async function loadContents() {
    try {
      setContents((await api.contents()).items);
    } catch {
      // 目录树加载失败不阻塞主界面；用户仍可从内容列表操作。
    }
  }

  function toggleTree() {
    if (!treeOpen) {
      void loadContents();
    }
    setTreeOpen((value) => !value);
  }

  function go(next: View) {
    setView(next);
    setMenuOpen(false);
    setTreeOpen(false);
  }

  function editFromTree(content: Content) {
    setEditContentId(content.id);
    setView("contents");
    setTreeOpen(false);
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
        </a>

        <button className="dir-tree-toggle" type="button" onClick={toggleTree}>
          目录
        </button>

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
                {item === "invites" ? "邀请码" : item === "users" ? "用户" : "站点设置"}
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

        {treeOpen && (
          <div className="dir-tree-panel">
            <div className="dir-tree-title">目录</div>
            <DirTree items={contents} onEdit={editFromTree} />
          </div>
        )}
      </header>

      <main className="appmain">
        <Suspense fallback={<p className="hint">加载视图…</p>}>
        {view === "contents" && (
          <ContentsView
            editContentId={editContentId}
            onEditHandled={() => setEditContentId(null)}
          />
        )}
        {view === "projects" && <ProjectsView />}
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
