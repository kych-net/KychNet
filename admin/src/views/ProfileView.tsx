import { useEffect, useState } from "react";
import { startRegistration } from "@simplewebauthn/browser";
import { api, avatarUrl, GitToken, isStepUpRequired, User } from "../api";
import { StepUpDialog } from "../components/StepUpDialog";
import { SshKeysView } from "./SshKeysView";

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
  const [stepUpOpen, setStepUpOpen] = useState(false);
  const [pendingAction, setPendingAction] = useState<(() => Promise<void>) | null>(null);

  const [tokens, setTokens] = useState<GitToken[]>([]);
  const [tokenName, setTokenName] = useState("");
  const [newToken, setNewToken] = useState("");

  useEffect(() => {
    api
      .gitTokens()
      .then((data) => setTokens(data.items))
      .catch((e) => setError((e as Error).message));
  }, []);

  async function createToken(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      const result = await api.createGitToken(tokenName);
      setTokens((items) => [result.item, ...items]);
      setNewToken(result.token);
      setTokenName("");
      setNotice("令牌已创建，请立即复制保存，之后无法再次查看");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function revokeToken(id: string) {
    setError("");
    try {
      await api.revokeGitToken(id);
      setTokens((items) => items.filter((item) => item.id !== id));
      setNotice("令牌已吊销");
    } catch (e) {
      setError((e as Error).message);
    }
  }

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
      // webauthn-rs 返回 { publicKey: {...} }，SimpleWebAuthn 需要里面的 publicKey 对象
      const options = started.options.publicKey ?? started.options;
      const credential = await startRegistration({ optionsJSON: options });
      await api.passkeySetupFinish(started.flow_id, credential);
      setNotice("通行密钥已绑定");
    } catch (e) {
      if (isStepUpRequired(e)) {
        setError("");
        setPendingAction(() => bindPasskey);
        setStepUpOpen(true);
      } else {
        setError((e as Error).message);
      }
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
      if (isStepUpRequired(e)) {
        setError("");
        setPendingAction(() => setupTotp);
        setStepUpOpen(true);
      } else {
        setError((e as Error).message);
      }
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

      <h3>Git 访问令牌</h3>
      <p className="hint">
        推送到 /git/&lt;项目&gt;.git 时需要令牌；克隆与拉取匿名即可。
      </p>
      <form onSubmit={createToken} className="row align">
        <input
          value={tokenName}
          onChange={(e) => setTokenName(e.target.value)}
          placeholder="令牌名称，如 笔记本"
        />
        <button type="submit">创建令牌</button>
      </form>
      {newToken && (
        <p>
          新令牌: <span className="mono">{newToken}</span>
        </p>
      )}
      {tokens.length > 0 && (
        <ul className="token-list">
          {tokens.map((token) => (
            <li key={token.id}>
              <span>
                {token.name}
                <span className="hint">
                  {" "}
                  {token.last_used_at
                    ? `最近使用 ${new Date(token.last_used_at).toLocaleString()}`
                    : "尚未使用"}
                </span>
              </span>
              <button className="danger" onClick={() => revokeToken(token.id)}>
                吊销
              </button>
            </li>
          ))}
        </ul>
      )}

      <SshKeysView />

      <StepUpDialog
        open={stepUpOpen}
        onClose={() => {
          setStepUpOpen(false);
          setPendingAction(null);
        }}
        onSuccess={() => {
          setStepUpOpen(false);
          const action = pendingAction;
          setPendingAction(null);
          if (action) {
            void action();
          }
        }}
      />
    </section>
  );
}
