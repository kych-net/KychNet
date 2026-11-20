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

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    try {
      setItems((await api.invites()).items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function create(event: React.FormEvent) {
    event.preventDefault();
    await run(async () => {
      setError("");
      try {
        const created = await api.createInvite(maxUses, expiresInDays);
        setNotice(`邀请码: ${created.item.code}`);
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
      {error && <p className="error" role="alert">{error}</p>}
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
