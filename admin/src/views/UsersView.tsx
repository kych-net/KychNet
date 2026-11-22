import { useEffect, useState } from "react";
import { api, User } from "../api";
import { useFeedback } from "../hooks/useFeedback";
import { useConfirm } from "../components/ConfirmProvider";

export function UsersView() {
  const [items, setItems] = useState<User[]>([]);
  const { error, setError } = useFeedback();
  const confirm = useConfirm();
  const [busyId, setBusyId] = useState<string | null>(null);
  // 首屏加载中：区分「加载中 / 空列表 / 有数据」，避免先闪一下空态。
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    setLoading(true);
    try {
      setItems((await api.users()).items);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }

  async function toggle(target: User) {
    // 禁用是可逆的，但会影响用户立即无法登录；给一次明确确认。启用则直接执行。
    if (
      !target.disabled &&
      !(await confirm(`禁用用户「${target.username}」？该用户将立即无法登录。`, {
        title: "禁用用户",
        confirmLabel: "禁用",
        danger: true,
      }))
    ) {
      return;
    }
    setError("");
    setBusyId(target.id);
    try {
      await api.setDisabled(target.id, !target.disabled);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusyId(null);
    }
  }

  return (
    <section>
      <h2>用户</h2>
      {error && (
        <p className="error" role="alert">
          {error}
          <button type="button" onClick={() => void refresh()}>
            重试
          </button>
        </p>
      )}
      <div className="table-wrap">
      <table>
        <thead>
          <tr>
            <th>用户名</th>
            <th>角色</th>
            <th>状态</th>
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
                还没有其他用户。用户注册后会出现在这里，可在此启用或禁用账号。
              </td>
            </tr>
          ) : null}
          {items.map((item) => (
            <tr key={item.id}>
              <td>{item.username}</td>
              <td>{item.is_admin ? "管理员" : "成员"}</td>
              <td>{item.disabled ? "已禁用" : "正常"}</td>
              <td>
                <button
                  onClick={() => void toggle(item)}
                  disabled={busyId === item.id}
                >
                  {busyId === item.id ? "处理中…" : item.disabled ? "启用" : "禁用"}
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      </div>
    </section>
  );
}
