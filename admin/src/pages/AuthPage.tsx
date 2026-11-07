import { useState } from "react";
import { startAuthentication, startRegistration } from "@simplewebauthn/browser";
import { api, User } from "../api";

type Props = {
  onLogin: (user: User) => void;
};

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

  async function handleLogin(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      if (method === "totp") {
        onLogin(await api.totpLogin(username, code));
      } else {
        const started = await api.loginStart(username);
        // webauthn-rs 返回 { publicKey: {...} }，SimpleWebAuthn 需要里面的 publicKey 对象
        const options = started.options.publicKey ?? started.options;
        const credential = await startAuthentication({ optionsJSON: options });
        onLogin(await api.loginFinish(started.flow_id, credential));
      }
    } catch (e) {
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
          {!passkeySupported && (
            <p className="hint">
              当前浏览器不支持 WebAuthn 通行密钥（例如部分外置磁盘/便携版 Chrome），请使用动态验证码。
            </p>
          )}
          <p className="hint">
            通行密钥需要先填写用户名再点登录；没有绑定过通行密钥，请先用动态验证码登录后在「用户设置」绑定。
          </p>
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
              minLength={3}
              maxLength={32}
              required
            />
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
