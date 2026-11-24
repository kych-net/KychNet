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
  onThemeChange: (theme: Theme) => void;
  onUserChange: (user: User) => void;
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

/** 读取我们自己写进 history.state 的视图索引；外部条目返回 null。 */
function readHistoryIndex(state: unknown): number | null {
  if (state && typeof state === "object" && "viewIndex" in state) {
    const value = (state as { viewIndex?: unknown }).viewIndex;
    if (typeof value === "number") return value;
  }
  return null;
}

export function Shell({
  user,
  site,
  siteUrl,
  theme,
  onThemeChange,
  onUserChange,
  onLogout,
}: Props) {
  const [view, setView] = useState<View>(() => viewFromHash(user.is_admin));
  const viewRef = useRef(view);
  viewRef.current = view;
  const [menuOpen, setMenuOpen] = useState(false);
  const [avatarFailed, setAvatarFailed] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const menuPanelRef = useRef<HTMLDivElement | null>(null);
  const avatarRef = useRef<HTMLButtonElement | null>(null);
  const { canLeave, dirty } = useNavigationGuard();
  const initial = user.username.slice(0, 1).toUpperCase();

  useEffect(() => {
    setAvatarFailed(false);
  }, [user.id, user.avatar_url]);

  // 视图在浏览器历史里的位置索引。用于前进/后退时判断方向，
  // 并在守卫拒绝时精确回拨到原条目，而不是用 pushState 覆盖地址栏
  // （后者会把前进历史整段丢掉）。
  const historyIndexRef = useRef(0);
  // 守卫拒绝后主动回拨历史产生的 popstate，需要忽略一次。
  const restoringRef = useRef(false);
  // go() 的防重入：连点两个不同视图时，避免两次 pushState 把历史栈写乱。
  const navigatingRef = useRef(false);

  // 首次挂载把当前视图写进地址栏（replace 不新增历史记录），并打上索引 0。
  useEffect(() => {
    window.history.replaceState({ viewIndex: 0 }, "", hashFor(viewRef.current));
  }, []);

  // 浏览器前进/后退：从地址栏恢复视图，离开前同样经过守卫。
  useEffect(() => {
    async function onPop(event: PopStateEvent) {
      if (restoringRef.current) {
        // 这是守卫拒绝后我们自己发起的回拨，不重复处理。
        restoringRef.current = false;
        return;
      }
      const next = viewFromHash(user.is_admin);
      const nextIndex = readHistoryIndex(event.state);
      if (next === viewRef.current) {
        // 地址栏与当前视图一致（例如非管理员访问管理端视图被回退），
        // 顺手把地址栏修正回当前视图。
        if (nextIndex !== null) historyIndexRef.current = nextIndex;
        const target = hashFor(viewRef.current);
        if (window.location.hash !== target) {
          window.history.replaceState({ viewIndex: historyIndexRef.current }, "", target);
        }
        return;
      }
      const currentIndex = historyIndexRef.current;
      if (!(await canLeave())) {
        if (nextIndex !== null && nextIndex !== currentIndex) {
          restoringRef.current = true;
          window.history.go(currentIndex - nextIndex);
        } else {
          // 条目上没有我们的索引（例如从外部直接打开）：只能重写地址栏。
          window.history.pushState({ viewIndex: currentIndex }, "", hashFor(viewRef.current));
        }
        return;
      }
      if (nextIndex !== null) historyIndexRef.current = nextIndex;
      setView(next);
      viewRef.current = next;
      setMenuOpen(false);
    }
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, [canLeave, user.is_admin]);

  // 头像菜单打开时把焦点放到第一项，符合 role="menu" 的键盘模型。
  useEffect(() => {
    if (!menuOpen) return;
    const first = menuPanelRef.current?.querySelector<HTMLElement>('[role="menuitem"]');
    first?.focus();
  }, [menuOpen]);

  /** 头像菜单的方向键模型：上下循环、Home/End 跳首尾、Esc 关闭并回焦头像按钮。 */
  function onMenuKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
    const nodes = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>('[role="menuitem"]'),
    );
    if (nodes.length === 0) return;
    const active = document.activeElement as HTMLElement | null;
    const index = active ? nodes.indexOf(active) : -1;
    let next = -1;
    switch (event.key) {
      case "ArrowDown":
        next = index < 0 ? 0 : (index + 1) % nodes.length;
        break;
      case "ArrowUp":
        next = index < 0 ? nodes.length - 1 : (index - 1 + nodes.length) % nodes.length;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = nodes.length - 1;
        break;
      case "Escape":
        event.preventDefault();
        setMenuOpen(false);
        avatarRef.current?.focus();
        return;
      case "Tab":
        // 离开菜单时收起，避免焦点落到菜单外但菜单还开着。
        setMenuOpen(false);
        return;
      default:
        return;
    }
    event.preventDefault();
    nodes[next]?.focus();
  }

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
    if (navigatingRef.current) return;
    navigatingRef.current = true;
    try {
      if (!(await canLeave())) return;
      setView(next);
      viewRef.current = next;
      const target = hashFor(next);
      if (window.location.hash !== target) {
        // pushState 不触发 popstate/hashchange，视图状态由这里直接更新。
        historyIndexRef.current += 1;
        window.history.pushState({ viewIndex: historyIndexRef.current }, "", target);
      }
      window.scrollTo?.({ top: 0 });
    } finally {
      navigatingRef.current = false;
    }
  }

  // 顶部可见导航的顺序，与 Alt+数字 快捷键一一对应。
  const shortcutViews: View[] = ["contents", "projects", ...(user.is_admin ? adminViews : [])];

  // 用 ref 始终指向最新的 go，避免把 go 放进监听依赖导致反复注册。
  const goRef = useRef(go);
  goRef.current = go;

  // Alt + 1..N 快速切换视图；在输入框/可编辑区内不拦截，避免干扰打字。
  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (!event.altKey || event.ctrlKey || event.metaKey) return;
      const target = event.target as HTMLElement | null;
      if (target?.isContentEditable) return;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      const index = Number(event.key) - 1;
      if (!Number.isInteger(index) || index < 0 || index >= shortcutViews.length) return;
      event.preventDefault();
      void goRef.current(shortcutViews[index]);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [shortcutViews.length]);

  function switchTheme() {
    onThemeChange(theme === "dark" ? "light" : "dark");
  }

  return (
    <div className="shell">
      <header className="appbar">
        {siteUrl ? (
          <a
            className="brand"
            href={siteUrl}
            target="_blank"
            rel="noreferrer"
            title={site.site_name}
            aria-label={site.site_name}
          >
            {site.logo_url ? (
              <img src={site.logo_url} alt={site.site_name} />
            ) : (
              <span className="brand-mark">{site.site_name.slice(0, 1)}</span>
            )}
            <span className="brand-name">{site.site_name}</span>
          </a>
        ) : (
          // 未配置站点地址时不做成链接：没有 href 的 <a> 不可聚焦、语义也错。
          <span className="brand" aria-label={site.site_name}>
            {site.logo_url ? (
              <img src={site.logo_url} alt={site.site_name} />
            ) : (
              <span className="brand-mark">{site.site_name.slice(0, 1)}</span>
            )}
            <span className="brand-name">{site.site_name}</span>
          </span>
        )}

        <nav className="appnav">
          <button
            className={view === "contents" ? "active" : ""}
            title="切换视图快捷键：Alt + 1"
            onClick={() => void go("contents")}
          >
            内容
          </button>
          <button
            className={view === "projects" ? "active" : ""}
            title="切换视图快捷键：Alt + 2"
            onClick={() => void go("projects")}
          >
            项目
          </button>
          {user.is_admin &&
            adminViews.map((item, index) => (
              <button
                key={item}
                className={view === item ? "active" : ""}
                title={`切换视图快捷键：Alt + ${index + 3}`}
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
              ref={avatarRef}
              onClick={() => setMenuOpen((open) => !open)}
              aria-label="用户菜单"
              aria-expanded={menuOpen}
              aria-haspopup="menu"
            >
              {!avatarFailed && avatarUrl(user.avatar_url) ? (
                <img
                  className="avatar-img"
                  src={avatarUrl(user.avatar_url) ?? undefined}
                  alt=""
                  onError={() => setAvatarFailed(true)}
                />
              ) : (
                initial
              )}
            </button>
            {menuOpen && (
              <div
                className="avatar-menu"
                role="menu"
                ref={menuPanelRef}
                onKeyDown={onMenuKeyDown}
              >
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
                    // 与切换视图一样先过离开守卫：有未保存改动时先确认。
                    if (!(await canLeave())) return;
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
            onUserChange={onUserChange}
          />
        )}
        </Suspense>
      </main>
    </div>
  );
}
