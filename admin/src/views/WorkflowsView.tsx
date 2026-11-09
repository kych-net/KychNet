import { useEffect, useState } from "react";
import { api, WorkflowItem, WorkflowRun } from "../api";

function fmt(value: string | null): string {
  return value ? new Date(value).toLocaleString() : "—";
}

type Props = {
  projectId: string;
  projectSlug: string;
};

/**
 * 项目级工作流面板。工作流定义与运行记录都属于项目，因此由 ProjectsView
 * 传入当前选中的项目，不再自带项目选择器，也不会一次加载全部项目的数据。
 * 执行器是站点级资源，放在独立的 RunnersView 中管理。
 */
export function ProjectWorkflowsPanel({ projectId, projectSlug }: Props) {
  const [workflowDir, setWorkflowDir] = useState("");
  const [items, setItems] = useState<WorkflowItem[]>([]);
  const [runs, setRuns] = useState<WorkflowRun[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [refreshKey, setRefreshKey] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setError("");
    setNotice("");
    setWorkflowDir("");
    setItems([]);
    setRuns([]);
    if (!projectId) return;
    (async () => {
      try {
        // 只请求当前项目的工作流与运行记录。
        const [wf, runResult] = await Promise.all([
          api.projectWorkflows(projectId),
          api.projectRuns(projectId),
        ]);
        if (cancelled) return;
        setWorkflowDir(wf.workflow_dir);
        setItems(wf.items);
        setRuns(runResult.items);
      } catch (e) {
        if (!cancelled) setError((e as Error).message);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId, refreshKey]);

  async function toggle(item: WorkflowItem) {
    if (!projectId) return;
    setError("");
    setNotice("");
    try {
      await api.setProjectWorkflow(projectId, item.path, !item.enabled);
      setNotice(item.enabled ? "已停用工作流" : "已启用工作流");
      setRefreshKey((key) => key + 1);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function dispatch() {
    if (!projectId) return;
    setError("");
    setNotice("");
    try {
      await api.dispatchProjectWorkflow(projectId);
      setNotice("已排入手动执行事件，稍后刷新运行记录");
      window.setTimeout(() => setRefreshKey((key) => key + 1), 1200);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <div className="project-workflow-panel">
      <div className="row">
        <p className="hint">
          项目 <strong>{projectSlug}</strong> 的工作流目录：
          <code>{workflowDir || "—"}</code>
        </p>
        <button type="button" onClick={dispatch} disabled={!projectId}>
          手动触发一次
        </button>
        <button type="button" onClick={() => setRefreshKey((key) => key + 1)} disabled={!projectId}>
          刷新
        </button>
      </div>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}
      <h4>工作流定义</h4>
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

      <h4>最近运行</h4>
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
    </div>
  );
}
