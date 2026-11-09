import { useEffect, useState } from "react";
import { api, WorkflowRunner } from "../api";

function fmt(value: string | null): string {
  return value ? new Date(value).toLocaleString() : "—";
}

/**
 * 站点级执行器管理。工作流定义与运行记录属于项目（见 ProjectWorkflowsPanel），
 * 但 runner 池是全站共享的，因此单独作为一个管理视图。
 */
export function RunnersView() {
  const [runners, setRunners] = useState<WorkflowRunner[]>([]);
  const [runnerName, setRunnerName] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  async function loadRunners() {
    try {
      setRunners((await api.runners()).items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  useEffect(() => {
    void loadRunners();
  }, []);

  async function registerRunner(event: React.FormEvent) {
    event.preventDefault();
    setCode("");
    setError("");
    setNotice("");
    try {
      const result = await api.createRunner(runnerName, []);
      setCode(result.registration_code);
      setRunnerName("");
      setNotice("注册码只显示一次，请立即交给 kychrunner");
      await loadRunners();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function toggleRunner(runner: WorkflowRunner) {
    setError("");
    try {
      await api.setRunnerEnabled(runner.id, !runner.enabled);
      await loadRunners();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>执行器</h2>
      <p className="hint">执行器由全站共享，用于运行各项目的工作流；项目内的定义与记录请在「项目 → 工作流」中查看。</p>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}
      <form className="row" onSubmit={registerRunner}>
        <label>
          名称
          <input value={runnerName} onChange={(e) => setRunnerName(e.target.value)} required placeholder="runner-1" />
        </label>
        <button type="submit">生成注册码</button>
      </form>
      {code && <p className="notice">注册码（只显示一次）：<code>{code}</code></p>}
      <table>
        <thead><tr><th>名称</th><th>协议</th><th>在线</th><th /></tr></thead>
        <tbody>
          {runners.map((runner) => (
            <tr key={runner.id}>
              <td>{runner.name}</td>
              <td>v{runner.protocol_version}</td>
              <td>{runner.last_seen_at ? fmt(runner.last_seen_at) : "从未"}</td>
              <td><button type="button" onClick={() => toggleRunner(runner)}>{runner.enabled ? "禁用" : "启用"}</button></td>
            </tr>
          ))}
          {runners.length === 0 && <tr><td colSpan={4}>暂无执行器。</td></tr>}
        </tbody>
      </table>
    </section>
  );
}
