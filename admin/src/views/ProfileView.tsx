import { useState } from "react";
import { startRegistration } from "@simplewebauthn/browser";
import { api, avatarUrl, User } from "../api";

type Props = {
  user: User;
  avatarVersion: number;
  onUserChange: (user: User) => void;
  onAvatarChange: () => void;
};

export function ProfileView({ user, avatarVersion, onUserChange, onAvatarChange }: Props) {
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [avatarPreview, setAvatarPreview] = useState("");
  const [avatarFailed, setAvatarFailed] = useState(false);

  const [totpSecret, setTotpSecret] = useState("");
  const [totpUrl, setTotpUrl] = useState("");
  const [totpCode, setTotpCode] = useState("");

  const initial = user.username.slice(0, 1).toUpperCase();

  async function uploadAvatar(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    if (!file) {
      return;
    }
    setError("");
    if (file.size > 256 * 1024) {
      setError("SVG 不能超过 256 KB");
      return;
    }
    try {
      const updated = await api.uploadAvatar(file);
      onUserChange(updated);
      setAvatarPreview(URL.createObjectURL(file));
      setAvatarFailed(false);
      onAvatarChange();
      setNotice("头像已更新");
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

      <div className="avatar-preview">
        <div className="avatar avatar-large">
          {avatarFailed ? (
            initial
          ) : (
            <img
              className="avatar-img"
              src={avatarPreview || avatarUrl(user.id, avatarVersion)}
              alt=""
              onError={() => {
                if (!avatarPreview) {
                  setAvatarFailed(true);
                }
              }}
            />
          )}
        </div>
        <div>
          <label className="upload">
            上传 SVG 头像
            <input type="file" accept=".svg,image/svg+xml" onChange={uploadAvatar} />
          </label>
          <p className="hint">仅接受纯矢量 SVG,不能包含位图、脚本或外部资源。</p>
        </div>
      </div>

      <label>
        用户名
        <input value={user.username} disabled />
      </label>

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
