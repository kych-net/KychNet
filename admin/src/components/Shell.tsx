import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { api, avatarUrl, SiteSettings, User } from "../api";
import { Theme } from "../theme";
import { useNavigationGuard } from "../components/NavigationGuardProvider";
const ContentsView = lazy(() => import("../views/ContentsView").then((module) => ({ default: module.ContentsView })));
const InvitesView = lazy(() => import("../views/InvitesView").then((module) => ({ default: module.InvitesView })));
const ProfileView = lazy(() => import("../views/ProfileView").then((module) => ({ default: module.ProfileView })));
const ProjectsView = lazy(() => import("../views/ProjectsView").then((module) => ({ default: module.ProjectsView })));
const SettingsView = lazy(() => import("../views/SettingsView").then((module) => ({ default: module.SettingsView })));
const RunnersView = lazy(() => import("../views/RunnersView").then((module) => ({ default: module.RunnersView })));
const ResourcesView = lazy(() => import("../views/ResourcesView").then((module) => ({ default: module.ResourcesView })));
const UsersView = lazy(() => import("../views/UsersView").then((module) => ({ default: module.UsersView })));

type View = "contents" | "projects" | "runners" | "invites" | "users" | "settings" | "profile" | "system";

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

const adminViews: View[] = ["runners", "invites", "users", "settings", "system"];
const validViews: View[] = [
  "contents", "projects", "runners", "invites", "users", "settings", "profile", "system",
];

const viewMeta: Record<View, { title: string; subtitle: string }> = {
  contents: { title: "内容", subtitle: "维护页面、样式与发布状态" },
  projects: { title: "项目", subtitle: "站内 Git 仓库、分支与协作" },
  runners: { title: "执行器", subtitle: "全站共享的工作流执行器与注册码" },
  invites: { title: "邀请码", subtitle: "控制注册入口与有效期" },
  users: { title: "用户", subtitle: "账号状态与权限" },
  settings: { title: "站点设置", subtitle: "站点信息、导航与发布" },
  profile: { title: "个人资料", subtitle: "头像、Git 令牌与安全设置" },
  system: { title: "资源监控", subtitle: "负载、内存、磁盘与存储占用" },
};

function hashFor(view: View): string {
  return `#/${view}`;
}

function viewFromHash(isAdmin: boolean): View {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const found = validViews.find((view) => view === raw);
  if (!found) return "contents";
  if (!isAdmin && adminViews.includes(found)) return "contents";
  return found;
}

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
  const [view, setView] = useState<View>(() => viewFromHash(user.is_admin));
  const viewRef = useRef(view);
  viewRef.current = view;
  const [menuOpen, setMenuOpen] = useState(false);
  const [avatarFailed, setAvatarFailed] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const { canLeave, dirty } = useNavigationGuard();
  const initial = user.username.slice(0, 1).toUpperCase();

  useEffect(() => {
    setAvatarFailed(false);
  }, [avatarVersion, user.id]);

  // 首次挂载把当前视图写进地址栏（replace 不新增历史记录）。
  useEffect(() => {
    const target = hashFor(viewRef.current);
    if (window.location.hash !== target) {
      window.history.replaceState(null, "", target);
    }
  }, []);

  // 浏览器前进/后退：从地址栏恢复视图，离开前同样经过守卫。
  useEffect(() => {
    async function onPop() {
      const next = viewFromHash(user.is_admin);
      if (next === viewRef.current) return;
      if (!(await canLeave())) {
        // 守卫拒绝：把地址栏恢复到当前视图。
        window.history.pushState(null, "", hashFor(viewRef.current));
        return;
      }
      setView(next);
      viewRef.current = next;
      setMenuOpen(false);
    }
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, [canLeave, user.is_admin]);

  // 头像菜单：点击外部或 Esc 关闭。
  useEffect(() => {
    if (!menuOpen) return;
    function onDown(event: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(event.target as Node)) {
        setMenuOpen(false);
      }
    }
    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") setMenuOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [menuOpen]);

  async function go(next: View) {
    setMenuOpen(false);
    if (next === viewRef.current) return;
    if (!(await canLeave())) return;
    setView(next);
    viewRef.current = next;
    const target = hashFor(next);
    if (window.location.hash !== target) {
      // pushState 不触发 popstate/hashchange，视图状态由这里直接更新。
      window.history.pushState(null, "", target);
    }
    window.scrollTo?.({ top: 0 });
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
          <button className={view === "contents" ? "active" : ""} onClick={() => void go("contents")}>
            内容
          </button>
          <button className={view === "projects" ? "active" : ""} onClick={() => void go("projects")}>
            项目
          </button>
          {user.is_admin &&
            adminViews.map((item) => (
              <button
                key={item}
                className={view === item ? "active" : ""}
                onClick={() => void go(item)}
              >
                {viewMeta[item].title}
              </button>
            ))}
        </nav>

        <div className="appactions">
          {dirty && <span className="unsaved-badge" title="当前视图有未保存的改动">未保存</span>}
          <button className="theme-button" onClick={switchTheme}>
            {theme === "dark" ? "浅色" : "深色"}
          </button>
          <div className="avatar-wrap" ref={menuRef}>
            <button
              className="avatar"
              onClick={() => setMenuOpen((open) => !open)}
              aria-label="用户菜单"
              aria-expanded={menuOpen}
              aria-haspopup="menu"
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
              <div className="avatar-menu" role="menu">
                <button role="menuitem" onClick={() => void go("profile")}>用户设置</button>
                {user.is_admin && <button role="menuitem" onClick={() => void go("contents")}>网站管理</button>}
                {siteUrl && (
                  <a role="menuitem" href={siteUrl} target="_blank" rel="noreferrer">
                    前往内容端
                  </a>
                )}
                <button
                  role="menuitem"
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
            <span className="page-eyebrow">{site.site_name || "Admin"} Admin</span>
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
        {view === "system" && user.is_admin && <ResourcesView />}
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
