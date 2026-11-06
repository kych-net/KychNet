import { useState } from "react";
import { startAuthentication, startRegistration } from "@simplewebauthn/browser";
import { api, User } from "../api";

type Props = {
  onLogin: (user: User) => void;
};

export function AuthPage({ onLogin }: Props) {
  const [mode, setMode] = useState<"login" | "register">("login");
  const [method, setMethod] = useState<"passkey" | "totp">("passkey");
  const [username, setUsername] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [code, setCode] = useState("");
  const [invite, setInvite] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  async function handleLogin(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      if (method === "totp") {
        onLogin(await api.totpLogin(username, code));
      } else {
        const started = await api.loginStart(username);
        const credential = await startAuthentication({ optionsJSON: started.options });
        onLogin(await api.loginFinish(started.flow_id, credential));
      }
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function handleRegister(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      const started = await api.register(invite, username, displayName || username);
      const credential = await startRegistration({ optionsJSON: started.options });
      onLogin(await api.finishRegister(started.flow_id, credential));
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="auth">
      <h1>KychNet</h1>
      <div className="tabs">
        <button className={mode === "login" ? "active" : ""} onClick={() => setMode("login")}>
          登录
        </button>
        <button className={mode === "register" ? "active" : ""} onClick={() => setMode("register")}>
          注册
        </button>
      </div>

      {mode === "login" ? (
        <form onSubmit={handleLogin}>
          <label>
            用户名
            <input value={username} onChange={(e) => setUsername(e.target.value)} required />
          </label>
          <div className="tabs small">
            <button
              type="button"
              className={method === "passkey" ? "active" : ""}
              onClick={() => setMethod("passkey")}
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
          {method === "totp" && (
            <label>
              6 位验证码
              <input
                value={code}
                onChange={(e) => setCode(e.target.value)}
                inputMode="numeric"
                autoComplete="one-time-code"
                required
              />
            </label>
          )}
          <button type="submit" disabled={busy}>
            {busy ? "处理中…" : "登录"}
          </button>
        </form>
      ) : (
        <form onSubmit={handleRegister}>
          <label>
            邀请码
            <input value={invite} onChange={(e) => setInvite(e.target.value)} required />
          </label>
          <label>
            用户名
            <input
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              pattern="[A-Za-z0-9_-]{3,32}"
              required
            />
          </label>
          <label>
            显示名称
            <input value={displayName} onChange={(e) => setDisplayName(e.target.value)} />
          </label>
          <p className="hint">注册后将立即创建通行密钥。</p>
          <button type="submit" disabled={busy}>
            {busy ? "处理中…" : "注册并绑定通行密钥"}
          </button>
        </form>
      )}

      {error && <p className="error">{error}</p>}
    </div>
  );
}
