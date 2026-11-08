import { useEffect, useState } from "react";
import { api, Project, WorkflowItem, WorkflowRun, WorkflowRunner } from "../api";

function fmt(value: string | null): string {
  return value ? new Date(value).toLocaleString() : "—";
}

export function WorkflowsView() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectId, setProjectId] = useState("");
  const [workflowDir, setWorkflowDir] = useState("");
  const [items, setItems] = useState<WorkflowItem[]>([]);
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [runners, setRunners] = useState<WorkflowRunner[]>([]);
  const [runnerName, setRunnerName] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  useEffect(() => {
    api
      .projects()
      .then((result) => {
        setProjects(result.items);
        if (result.items[0]) setProjectId(result.items[0].id);
      })
      .catch((e) => setError((e as Error).message));
    void loadRunners();
  }, []);

  useEffect(() => {
    if (projectId) void loadProject(projectId);
  }, [projectId]);

  async function loadProject(id: string) {
    setError("");
    try {
      const [wf, runResult] = await Promise.all([api.projectWorkflows(id), api.projectRuns(id)]);
      setWorkflowDir(wf.workflow_dir);
      setItems(wf.items);
      setRuns(runResult.items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function loadRunners() {
    try {
      setRunners((await api.runners()).items);
    } catch {
      // 非管理员或其他加载失败不阻塞工作流列表。
    }
  }

  async function toggle(item: WorkflowItem) {
    if (!projectId) return;
    setNotice("");
    try {
      await api.setProjectWorkflow(projectId, item.path, !item.enabled);
      setNotice(item.enabled ? "已停用工作流" : "已启用工作流");
      await loadProject(projectId);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function dispatch() {
    if (!projectId) return;
    setNotice("");
    try {
      await api.dispatchProjectWorkflow(projectId);
      setNotice("已排入手动执行事件，稍后刷新运行记录");
      setTimeout(() => void loadProject(projectId), 1200);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function registerRunner(event: React.FormEvent) {
    event.preventDefault();
    setCode("");
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
    try {
      await api.setRunnerEnabled(runner.id, !runner.enabled);
      await loadRunners();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>工作流</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}
      <div className="row">
        <label>
          项目
          <select value={projectId} onChange={(e) => setProjectId(e.target.value)}>
            {projects.map((project) => (
              <option key={project.id} value={project.id}>
                {project.slug}
              </option>
            ))}
          </select>
        </label>
        <button type="button" onClick={dispatch} disabled={!projectId}>
          手动触发一次
        </button>
      </div>
      <p className="hint">工作流目录：<code>{workflowDir || "—"}</code></p>
      <table>
        <thead>
          <tr><th>定义</th><th>状态</th><th /></tr>
        </thead>
        <tbody>
          {items.map((item) => (
            <tr key={item.path}>
              <td className="mono small">{item.path}</td>
              <td>{item.enabled ? "已启用" : "未启用"}</td>
              <td><button type="button" onClick={() => toggle(item)}>{item.enabled ? "停用" : "启用"}</button></td>
            </tr>
          ))}
          {items.length === 0 && <tr><td colSpan={3}>默认分支下没有找到 YAML 工作流。</td></tr>}
        </tbody>
      </table>

      <h3>最近运行</h3>
      <table>
        <thead><tr><th>工作流</th><th>事件</th><th>状态</th><th>创建时间</th></tr></thead>
        <tbody>
          {runs.map((run) => (
            <tr key={run.id}>
              <td className="mono small">{run.workflow_path}</td>
              <td>{run.event_kind}</td>
              <td>{run.conclusion || run.status}{run.skipped_reason ? ` (${run.skipped_reason})` : ""}</td>
              <td className="small">{fmt(run.created_at)}</td>
            </tr>
          ))}
          {runs.length === 0 && <tr><td colSpan={4}>暂无运行记录。</td></tr>}
        </tbody>
      </table>

      <h3>执行器</h3>
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
