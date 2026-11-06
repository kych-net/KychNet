import { useState } from "react";
import { startRegistration } from "@simplewebauthn/browser";
import { api, User } from "../api";

type Props = {
  user: User;
  onUserChange: (user: User) => void;
};

export function ProfileView({ user, onUserChange }: Props) {
  const [displayName, setDisplayName] = useState(user.display_name);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [totpSecret, setTotpSecret] = useState("");
  const [totpUrl, setTotpUrl] = useState("");
  const [totpCode, setTotpCode] = useState("");

  async function saveProfile(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      const updated = await api.updateProfile(displayName);
      onUserChange(updated);
      setNotice("用户设置已保存");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function bindPasskey() {
    setError("");
    try {
      const started = await api.passkeySetup();
      const credential = await startRegistration({ optionsJSON: started.options });
      await api.passkeySetupFinish(started.flow_id, credential);
      setNotice("通行密钥已绑定");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function setupTotp() {
    setError("");
    try {
      const result = await api.totpSetup();
      setTotpSecret(result.secret);
      setTotpUrl(result.otpauth_url);
      setNotice("请用认证器 App 添加该密钥,然后输入动态码确认");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function confirmTotp(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      await api.totpConfirm(totpCode);
      setNotice("动态验证码已启用");
      setTotpSecret("");
      setTotpUrl("");
      setTotpCode("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>用户设置</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form onSubmit={saveProfile}>
        <label>
          用户名
          <input value={user.username} disabled />
        </label>
        <label>
          显示名称
          <input value={displayName} onChange={(e) => setDisplayName(e.target.value)} required />
        </label>
        <button type="submit">保存</button>
      </form>

      <h3>通行密钥</h3>
      <button onClick={bindPasskey}>绑定新通行密钥</button>

      <h3>动态验证码</h3>
      {totpSecret ? (
        <>
          <p>
            密钥: <span className="mono">{totpSecret}</span>
          </p>
          <p className="mono small">{totpUrl}</p>
          <form onSubmit={confirmTotp} className="row align">
            <input
              value={totpCode}
              onChange={(e) => setTotpCode(e.target.value)}
              placeholder="6 位验证码"
              inputMode="numeric"
            />
            <button type="submit">确认启用</button>
          </form>
        </>
      ) : (
        <button onClick={setupTotp}>生成 TOTP 密钥</button>
      )}
    </section>
  );
}
