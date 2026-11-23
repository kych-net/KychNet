import { useEffect, useState } from "react";
import { startAuthentication, startRegistration } from "@simplewebauthn/browser";
import { api, User } from "../api";

type Props = {
  onLogin: (user: User) => void;
};

function withTimeout<T>(promise: Promise<T>, milliseconds: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(() => {
      reject(new DOMException("等待通行密钥窗口超时", "NotAllowedError"));
    }, milliseconds);
    promise.then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        window.clearTimeout(timer);
        reject(error);
      },
    );
  });
}

function completeLogin(user: User, onLogin: (user: User) => void) {
  onLogin(user);
  // OAuth 授权页会带 next 参数把用户送到管理端登录；只允许回跳本站授权地址。
  const next = new URLSearchParams(window.location.search).get("next");
  if (next && next.startsWith("https://kych.net/oauth/authorize?")) {
    window.location.assign(next);
  }
}

function loginError(error: unknown): string {
  if (error instanceof DOMException) {
    if (error.name === "NotAllowedError") {
      return "通行密钥验证被取消或超时；请重试，或改用动态验证码。";
    }
    if (error.name === "SecurityError") {
      return "当前页面或浏览器不允许使用通行密钥；请改用动态验证码。";
    }
  }
  return error instanceof Error ? error.message : String(error);
}

export function AuthPage({ onLogin }: Props) {
  const passkeySupported =
    typeof window !== "undefined" && "PublicKeyCredential" in window;
  const [mode, setMode] = useState<"login" | "register">("login");
  const [method, setMethod] = useState<"passkey" | "totp">(
    passkeySupported ? "passkey" : "totp",
  );
  const [username, setUsername] = useState("");
  const [code, setCode] = useState("");
  const [invite, setInvite] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [platformAuthenticator, setPlatformAuthenticator] = useState<boolean | null>(
    passkeySupported ? null : false,
  );

  const [siteName, setSiteName] = useState("");
  // 登录页在鉴权之前就要显示站点名，自行拉一次设置；失败则保持空。
  useEffect(() => {
    api
      .getSettings()
      .then((result) => setSiteName(result.site.site_name))
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    if (!passkeySupported) return;
    if (typeof window.isSecureContext === "boolean" && !window.isSecureContext) {
      setPlatformAuthenticator(false);
      return;
    }
    const check = window.PublicKeyCredential
      ?.isUserVerifyingPlatformAuthenticatorAvailable;
    if (typeof check !== "function") {
      setPlatformAuthenticator(true);
      return;
    }
    let cancelled = false;
    check
      .call(window.PublicKeyCredential)
      .then((available) => {
        if (!cancelled) setPlatformAuthenticator(available);
      })
      .catch(() => {
        if (!cancelled) setPlatformAuthenticator(false);
      });
    return () => {
      cancelled = true;
    };
  }, [passkeySupported]);

  async function handleLogin(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      if (method === "totp") {
        completeLogin(await api.totpLogin(username, code), onLogin);
      } else {
        const started = await api.loginStart(username);
        // webauthn-rs 返回 { publicKey: {...} }，SimpleWebAuthn 需要里面的 publicKey 对象
        const options = started.options.publicKey ?? started.options;
        const credential = await withTimeout(
          startAuthentication({ optionsJSON: options }),
          60_000,
        );
        completeLogin(await api.loginFinish(started.flow_id, credential), onLogin);
      }
    } catch (e) {
      console.error("登录失败", e);
      setError(loginError(e));
    } finally {
      setBusy(false);
    }
  }

  async function handleRegister(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      const started = await api.register(invite, username);
      const options = started.options.publicKey ?? started.options;
      const credential = await startRegistration({ optionsJSON: options });
      onLogin(await api.finishRegister(started.flow_id, credential));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="auth">
      <div className="auth-card">
        <div className="auth-brand">
          <span className="auth-brand-mark" aria-hidden="true" />
          <div className="auth-brand-copy">
            <h1>{siteName || "管理"}</h1>
            <p className="auth-brand-sub">管理后台 · 安全登录</p>
          </div>
        </div>

        <div className="tabs auth-tabs" aria-label="登录或注册">
          <button
            type="button"
            className={mode === "login" ? "active" : ""}
            onClick={() => setMode("login")}
          >
            登录
          </button>
          <button
            type="button"
            className={mode === "register" ? "active" : ""}
            onClick={() => setMode("register")}
          >
            注册
          </button>
        </div>

        {mode === "login" ? (
          <form onSubmit={handleLogin} className="auth-form">
            <div className="tabs small auth-methods" aria-label="登录方式">
              <button
                type="button"
                className={method === "passkey" ? "active" : ""}
                onClick={() => setMethod("passkey")}
                disabled={!passkeySupported}
              >
                通行密钥
              </button>
              <button
                type="button"
                className={method === "totp" ? "active" : ""}
                onClick={() => setMethod("totp")}
              >
                动态验证码
              </button>
            </div>
            <label>
              <span className="field-label">用户名</span>
              <input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                autoComplete="username"
                placeholder="请输入用户名"
                required
              />
            </label>
            {!passkeySupported && (
              <p className="hint hint-warn">
                当前浏览器不支持 WebAuthn 通行密钥（例如部分外置磁盘/便携版 Chrome），请使用动态验证码。
              </p>
            )}
            {method === "passkey" && platformAuthenticator === false && (
              <p className="hint hint-warn">
                当前浏览器没有可用的平台认证器（Touch ID / Windows Hello）。通行密钥可能不会弹窗；
                请改用动态验证码，或换成正常安装的 Chrome/Safari/Edge 后再试。
              </p>
            )}
            <p className="hint">
              先填写用户名，再点下方「{method === "passkey" ? "使用通行密钥登录" : "使用动态验证码登录"}」。
              没有绑定过通行密钥，请先用动态验证码登录后在「用户设置」绑定。
            </p>
            {method === "totp" && (
              <label>
                <span className="field-label">6 位验证码</span>
                <input
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  placeholder="6 位数字验证码"
                  required
                />
              </label>
            )}
            <button type="submit" disabled={busy} className="auth-submit">
              {busy
                ? method === "passkey"
                  ? "等待通行密钥窗口…"
                  : "处理中…"
                : method === "passkey"
                  ? "使用通行密钥登录"
                  : "使用动态验证码登录"}
            </button>
            {busy && method === "passkey" && (
              <p className="hint">
                正在等待浏览器弹出通行密钥窗口；如果 60 秒内没有弹窗，可以改用动态验证码。
              </p>
            )}
          </form>
        ) : (
          <form onSubmit={handleRegister} className="auth-form">
            <label>
              <span className="field-label">邀请码</span>
              <input
                value={invite}
                onChange={(e) => setInvite(e.target.value)}
                placeholder="请输入邀请码"
                required
              />
            </label>
            <label>
              <span className="field-label">用户名</span>
              <input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                minLength={3}
                maxLength={32}
                placeholder="3–32 个字符"
                required
              />
            </label>
            <p className="hint">注册后将立即创建通行密钥。</p>
            <button type="submit" disabled={busy} className="auth-submit">
              {busy ? "处理中…" : "注册并绑定通行密钥"}
            </button>
          </form>
        )}

        {error && <p className="error" role="alert">{error}</p>}
      </div>
      <p className="auth-foot">端到端加密 · 通行密钥安全登录</p>
    </div>
  );
}
