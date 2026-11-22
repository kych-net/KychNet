import { useEffect, useState } from "react";
import { api, Invite } from "../api";
import { useFeedback } from "../hooks/useFeedback";
import { useBusy } from "../hooks/useBusy";
import { useConfirm } from "../components/ConfirmProvider";

export function InvitesView() {
  const [items, setItems] = useState<Invite[]>([]);
  const [maxUses, setMaxUses] = useState(1);
  const [expiresInDays, setExpiresInDays] = useState(7);
  const { error, notice, setError, setNotice } = useFeedback();
  const { busy, run } = useBusy();
  const confirm = useConfirm();
  const [busyCode, setBusyCode] = useState<string | null>(null);
  // 最新生成的邀请码：一次性凭据，单独常驻展示，不被提示的超时清掉。
  const [createdCode, setCreatedCode] = useState("");
  // 首屏加载中：区分「加载中 / 空列表 / 有数据」三态，避免先闪一下「暂无」。
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    setLoading(true);
    try {
      setItems((await api.invites()).items);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  async function create(event: React.FormEvent) {
    event.preventDefault();
    await run(async () => {
      setError("");
      setNotice("");
      try {
        const created = await api.createInvite(maxUses, expiresInDays);
        // 邀请码只显示一次：像执行器注册码一样常驻展示，不吃 notice 的自动消失。
        setCreatedCode(created.item.code);
        // 提示放在拿到结果之后：失败时不能先播报「已生成」。
        setNotice("邀请码已生成");
        await refresh();
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function revoke(invite: Invite) {
    if (
      !(await confirm(`作废邀请码「${invite.code}」？作废后立即失效。`, {
        title: "作废邀请码",
        confirmLabel: "作废",
        danger: true,
      }))
    ) {
      return;
    }
    setError("");
    setBusyCode(invite.code);
    try {
      await api.revokeInvite(invite.code);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusyCode(null);
    }
  }

  return (
    <section>
      <h2>邀请码</h2>
      {error && (
        <p className="error" role="alert">
          {error}
          <button type="button" onClick={() => void refresh()}>
            重试
          </button>
        </p>
      )}
      {notice && <p className="notice" role="status">{notice}</p>}

      <form onSubmit={(e) => void create(e)} className="row align">
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
        <button type="submit" disabled={busy}>
          {busy ? "生成中…" : "生成"}
        </button>
      </form>

      {createdCode && (
        <p className="notice">
          邀请码（只显示一次）：<code>{createdCode}</code>
        </p>
      )}

      <div className="table-wrap">
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
          {/* 已有数据时不再显示「加载中」，避免每次刷新表格闪一下。 */}
          {loading && items.length === 0 ? (
            <tr>
              <td colSpan={4} className="hint">
                加载中…
              </td>
            </tr>
          ) : items.length === 0 ? (
            <tr>
              <td colSpan={4} className="hint">
                还没有邀请码。在上方设置使用次数与有效期后点「生成」，把生成的邀请码发给要邀请的人。
              </td>
            </tr>
          ) : null}
          {items.map((invite) => (
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
                  <button
                    className="danger-text"
                    disabled={busyCode === invite.code}
                    onClick={() => void revoke(invite)}
                  >
                    {busyCode === invite.code ? "作废中…" : "作废"}
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}
