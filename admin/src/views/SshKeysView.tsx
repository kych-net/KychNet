import { useEffect, useState } from "react";
import { api, GitSshKey } from "../api";

function fmt(value: string | null): string {
  return value ? new Date(value).toLocaleString() : "从未使用";
}

export function SshKeysView() {
  const [items, setItems] = useState<GitSshKey[]>([]);
  const [name, setName] = useState("");
  const [publicKey, setPublicKey] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void load();
  }, []);

  async function load() {
    try {
      setItems((await api.sshKeys()).items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function add(event: React.FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await api.createSshKey(name.trim(), publicKey.trim());
      setName("");
      setPublicKey("");
      setNotice("公钥已添加，可以用 SSH 推送或拉取仓库");
      await load();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function remove(item: GitSshKey) {
    if (!window.confirm(`撤销公钥「${item.name}」？使用该私钥的 SSH 客户端将立即失效。`)) {
      return;
    }
    setError("");
    try {
      await api.revokeSshKey(item.id);
      setNotice("公钥已撤销");
      await load();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <div className="ssh-header">
        <div>
          <h2>SSH 公钥</h2>
          <p className="hint">
            用于 <code>git clone ssh://git@kych.net:2222/&lt;项目&gt;.git</code>。
            私钥只留在你的电脑，服务端只保存公钥和指纹。
          </p>
        </div>
        <span className="ssh-count">{items.length}</span>
      </div>

      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form className="ssh-add" onSubmit={add}>
        <label>
          名称
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如 MacBook 或 工作电脑"
            required
            maxLength={100}
          />
        </label>
        <label>
          OpenSSH 公钥
          <textarea
            value={publicKey}
            onChange={(e) => setPublicKey(e.target.value)}
            placeholder="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5... 你的备注"
            rows={4}
            required
          />
        </label>
        <p className="hint">
          只接受一行 OpenSSH 公钥，不要粘贴 <code>id_ed25519</code> 私钥。
        </p>
        <button type="submit" disabled={busy || !name.trim() || !publicKey.trim()}>
          添加公钥
        </button>
      </form>

      <div className="ssh-list">
        {items.length === 0 ? (
          <p className="hint">还没有登记公钥。</p>
        ) : (
          items.map((item) => (
            <article className="ssh-card" key={item.id}>
              <div className="ssh-card-head">
                <div>
                  <strong>{item.name}</strong>
                  <span className="mono small">{item.fingerprint}</span>
                </div>
                <button type="button" className="danger-text" onClick={() => remove(item)}>
                  撤销
                </button>
              </div>
              <code className="ssh-public-key">{item.public_key}</code>
              <div className="ssh-meta">
                <span>创建于 {new Date(item.created_at).toLocaleString()}</span>
                <span>最近使用 {fmt(item.last_used_at)}</span>
              </div>
            </article>
          ))
        )}
      </div>
    </section>
  );
}
