import { useEffect, useState } from "react";
import { api, avatarUrl, SiteSettings, User } from "../api";
import { Theme } from "../theme";
import { ContentsView } from "../views/ContentsView";
import { InvitesView } from "../views/InvitesView";
import { ProfileView } from "../views/ProfileView";
import { SettingsView } from "../views/SettingsView";
import { UsersView } from "../views/UsersView";

type View = "contents" | "invites" | "users" | "settings" | "profile";

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
  const initial = (user.display_name || user.username).slice(0, 1).toUpperCase();

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
          href={siteUrl}
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

        <nav className="appnav">
          <button className={view === "contents" ? "active" : ""} onClick={() => go("contents")}>
            内容
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
                <a href={siteUrl} target="_blank" rel="noreferrer">
                  前往内容端
                </a>
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
        {view === "contents" && <ContentsView />}
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
      </main>
    </div>
  );
}
