import { useEffect, useState } from "react";
import { api, User } from "../api";

export function UsersView() {
  const [items, setItems] = useState<User[]>([]);
  const [error, setError] = useState("");

  useEffect(() => {
    void refresh();
  }, []);

  async function refresh() {
    try {
      setItems((await api.users()).items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function toggle(target: User) {
    try {
      await api.setDisabled(target.id, !target.disabled);
      await refresh();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>用户</h2>
      {error && <p className="error">{error}</p>}
      <table>
        <thead>
          <tr>
            <th>用户名</th>
            <th>显示名称</th>
            <th>角色</th>
            <th>状态</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {items.map((item) => (
            <tr key={item.id}>
              <td>{item.username}</td>
              <td>{item.display_name}</td>
              <td>{item.is_admin ? "管理员" : "成员"}</td>
              <td>{item.disabled ? "已禁用" : "正常"}</td>
              <td>
                <button onClick={() => toggle(item)}>{item.disabled ? "启用" : "禁用"}</button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
