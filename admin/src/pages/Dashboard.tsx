import { useEffect, useState } from "react";
import { startRegistration } from "@simplewebauthn/browser";
import { api, Content, ContentFormat, Invite, User } from "../api";

type Props = {
  user: User;
  onLogout: () => void;
};

export function Dashboard({ user, onLogout }: Props) {
  const [invites, setInvites] = useState<Invite[]>([]);
  const [contents, setContents] = useState<Content[]>([]);
  const [users, setUsers] = useState<User[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  const [maxUses, setMaxUses] = useState(1);
  const [expiresInDays, setExpiresInDays] = useState(7);

  const [slug, setSlug] = useState("");
  const [title, setTitle] = useState("");
  const [format, setFormat] = useState<ContentFormat>("markdown");
  const [body, setBody] = useState("");
  const [published, setPublished] = useState(false);

  const [totpSecret, setTotpSecret] = useState("");
  const [totpUrl, setTotpUrl] = useState("");
  const [totpCode, setTotpCode] = useState("");

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    try {
      const [inviteResult, contentResult, userResult] = await Promise.all([
        api.invites(),
        api.contents(),
        api.users(),
      ]);
      setInvites(inviteResult.items);
      setContents(contentResult.items);
      setUsers(userResult.items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function handleLogout() {
    await api.logout().catch(() => undefined);
    onLogout();
  }

  async function createInvite(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      const created = await api.createInvite(maxUses, expiresInDays);
      setNotice(`邀请码: ${created.item.code}`);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function revokeInvite(code: string) {
    try {
      await api.revokeInvite(code);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function saveContent(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      await api.saveContent({ slug, title, format, body, published });
      setNotice("内容已保存");
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function deleteContent(id: string) {
    try {
      await api.deleteContent(id);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function editContent(item: Content) {
    setSlug(item.slug);
    setTitle(item.title);
    setFormat(item.format);
    setBody(item.body);
    setPublished(item.published);
  }

  async function toggleUser(target: User) {
    try {
      await api.setDisabled(target.id, !target.disabled);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function setupTotp() {
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

  async function bindPasskey() {
    try {
      const started = await api.passkeySetup();
      const credential = await startRegistration({ optionsJSON: started.options });
      await api.passkeySetupFinish(started.flow_id, credential);
      setNotice("通行密钥已绑定");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <div className="dashboard">
      <header>
        <h1>KychNet 管理</h1>
        <span>{user.display_name}</span>
        <button onClick={handleLogout}>退出</button>
      </header>

      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <section>
        <h2>邀请码</h2>
        <form onSubmit={createInvite} className="inline">
          <label>
            使用次数
            <input
              type="number"
              min={1}
              max={100}
              value={maxUses}
              onChange={(e) => setMaxUses(Number(e.target.value))}
            />
          </label>
          <label>
            有效期(天)
            <input
              type="number"
              min={0}
              value={expiresInDays}
              onChange={(e) => setExpiresInDays(Number(e.target.value))}
            />
          </label>
          <button type="submit">生成</button>
        </form>
        <table>
          <thead>
            <tr>
              <th>邀请码</th>
              <th>使用</th>
              <th>过期</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {invites.map((invite) => (
              <tr key={invite.code}>
                <td className="mono">{invite.code}</td>
                <td>
                  {invite.used_count}/{invite.max_uses}
                </td>
                <td>{invite.expires_at ?? "never"}</td>
                <td>
                  {invite.revoked_at ? (
                    "已作废"
                  ) : (
                    <button onClick={() => revokeInvite(invite.code)}>作废</button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section>
        <h2>内容</h2>
        <form onSubmit={saveContent}>
          <label>
            Slug
            <input value={slug} onChange={(e) => setSlug(e.target.value)} required />
          </label>
          <label>
            标题
            <input value={title} onChange={(e) => setTitle(e.target.value)} required />
          </label>
          <label>
            格式
            <select value={format} onChange={(e) => setFormat(e.target.value as ContentFormat)}>
              <option value="markdown">Markdown</option>
              <option value="html">HTML</option>
              <option value="typst">Typst</option>
            </select>
          </label>
          <label>
            正文
            <textarea value={body} onChange={(e) => setBody(e.target.value)} rows={14} />
          </label>
          <label className="checkbox">
            <input
              type="checkbox"
              checked={published}
              onChange={(e) => setPublished(e.target.checked)}
            />
            发布
          </label>
          <button type="submit">保存</button>
        </form>

        <table>
          <thead>
            <tr>
              <th>标题</th>
              <th>格式</th>
              <th>状态</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {contents.map((item) => (
              <tr key={item.id}>
                <td>{item.title}</td>
                <td>{item.format}</td>
                <td>{item.published ? "已发布" : "草稿"}</td>
                <td>
                  <button onClick={() => editContent(item)}>编辑</button>
                  <button onClick={() => deleteContent(item.id)}>删除</button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section>
        <h2>用户</h2>
        <table>
          <thead>
            <tr>
              <th>用户名</th>
              <th>显示名称</th>
              <th>状态</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {users.map((item) => (
              <tr key={item.id}>
                <td>{item.username}</td>
                <td>{item.display_name}</td>
                <td>{item.disabled ? "已禁用" : "正常"}</td>
                <td>
                  <button onClick={() => toggleUser(item)}>
                    {item.disabled ? "启用" : "禁用"}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      <section>
        <h2>通行密钥</h2>
        <button onClick={bindPasskey}>绑定新通行密钥</button>
      </section>

      <section>
        <h2>动态验证码</h2>
        {totpSecret ? (
          <>
            <p>密钥: <span className="mono">{totpSecret}</span></p>
            <p className="mono small">{totpUrl}</p>
            <form onSubmit={confirmTotp} className="inline">
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
    </div>
  );
}
