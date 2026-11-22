import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@admin/api";
import { ServerDiffView } from "./DiffView";
import { useBusy } from "../useBusy";
import type { PrDetail, ProjectRefs, PullRequest } from "./types";
import { errText, formatTime } from "./types";

const STATUS_LABEL: Record<string, string> = {
  open: "开放",
  merged: "已合并",
  closed: "已关闭",
};

/** 拉取请求：列表 / 创建 / 详情差异 / 评论 / 合并 / 关闭。 */
export function PrsView({
  projectId,
  refs,
  refreshKey,
  onChanged,
  onNotify,
}: {
  projectId: string;
  refs: ProjectRefs;
  refreshKey: number;
  onChanged: () => void;
  onNotify: (message: string) => void;
}) {
  const [prs, setPrs] = useState<PullRequest[]>([]);
  const [detail, setDetail] = useState<PrDetail | null>(null);
  const [selected, setSelected] = useState<number | null>(null);
  const [creating, setCreating] = useState(false);
  const [title, setTitle] = useState("");
  const [body, setBody] = useState("");
  const [source, setSource] = useState(refs.branches.find((b) => b !== refs.default_branch) ?? refs.default_branch);
  const [target, setTarget] = useState(refs.default_branch);
  const [comment, setComment] = useState("");
  const [error, setError] = useState("");
  const epoch = useRef(0);
  // 详情请求单独计数：慢响应不能覆盖用户后点的 PR。
  const detailEpoch = useRef(0);
  const { busy, error: actionError, run: runAction } = useBusy();

  const load = useCallback(async (token: number) => {
    try {
      const res = await api.prs(projectId);
      if (epoch.current !== token) return;
      setPrs(res.items);
    } catch (e) {
      if (epoch.current === token) setError(errText(e));
    }
  }, [projectId]);

  useEffect(() => {
    const token = ++epoch.current;
    detailEpoch.current += 1;
    void load(token);
  }, [load, refreshKey]);

  const open = async (number: number) => {
    if (selected === number) {
      detailEpoch.current += 1;
      setSelected(null);
      setDetail(null);
      return;
    }
    const token = ++detailEpoch.current;
    setSelected(number);
    setDetail(null);
    setError("");
    try {
      const res = await api.prDetail(projectId, number);
      if (detailEpoch.current !== token) return;
      setDetail(res.item);
    } catch (e) {
      if (detailEpoch.current === token) setError(errText(e));
    }
  };

  const run = async (action: () => Promise<void>, success: string) => {
    const ok = await runAction(async () => {
      await action();
      onNotify(success);
      onChanged();
      const token = ++epoch.current;
      await load(token);
      if (selected !== null) {
        const detailToken = ++detailEpoch.current;
        const res = await api.prDetail(projectId, selected);
        if (detailEpoch.current === detailToken) setDetail(res.item);
      }
    });
    if (ok) setError("");
  };

  const create = () =>
    run(async () => {
      await api.createPr(projectId, {
        title: title.trim(),
        body: body.trim(),
        source_branch: source,
        target_branch: target,
      });
      setTitle("");
      setBody("");
      setCreating(false);
    }, "拉取请求已创建");

  return (
    <div className="pv-prs">
      <div className="pv-section-head">
        <div><h2>拉取请求 <span className="pv-count">{prs.length}</span></h2></div>
        <button type="button" className="pv-btn pv-btn-accent" onClick={() => setCreating((v) => !v)}>＋ 新建</button>
      </div>
      {(error || actionError) && <p className="pv-error" role="alert">{error || actionError}</p>}

      {creating && (
        <section className="pv-card">
          <label className="pv-field"><span>标题</span><input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="这次改了什么" /></label>
          <label className="pv-field"><span>说明</span><textarea value={body} onChange={(e) => setBody(e.target.value)} rows={3} placeholder="补充说明（可留空）" /></label>
          <div className="pv-inline-form">
            <select value={source} onChange={(e) => setSource(e.target.value)} aria-label="来源分支">
              {refs.branches.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
            <span className="pv-arrow">→</span>
            <select value={target} onChange={(e) => setTarget(e.target.value)} aria-label="目标分支">
              {refs.branches.map((name) => <option key={name} value={name}>{name}</option>)}
            </select>
            <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !title.trim() || source === target} onClick={() => void create()}>创建</button>
          </div>
        </section>
      )}

      {prs.length === 0 && !creating && <p className="pv-hint">还没有拉取请求。</p>}

      {prs.map((pr) => (
        <section className="pv-card pv-pr-item" key={pr.id}>
          <button type="button" className="pv-pr-head" onClick={() => void open(pr.number)}>
            <span className={`pv-chip pv-chip-${pr.status === "open" ? "add" : "modify"}`}>{STATUS_LABEL[pr.status]}</span>
            <span className="pv-pr-title">#{pr.number} {pr.title}</span>
            <span className="pv-hint">{pr.source_branch} → {pr.target_branch}</span>
          </button>
          <p className="pv-hint">{pr.author_name} · {formatTime(pr.created_at)}</p>

          {selected === pr.number && (
            <div className="pv-pr-detail">
              {detail ? (
                <>
                  <p className="pv-pr-body">{detail.body || "（无说明）"}</p>
                  {detail.conflicts.length > 0 && (
                    <p className="pv-error">冲突文件：{detail.conflicts.join("、")}。请先把目标分支合并到来源分支。</p>
                  )}
                  <p className="pv-hint">
                    {detail.mergeable ? "可合并" : "暂不可合并"} · {detail.is_fast_forward ? "可快进" : "需生成合并提交"}
                  </p>
                  <ServerDiffView files={detail.files} />

                  <div className="pv-comments">
                    {detail.comments.map((item) => (
                      <div className="pv-comment" key={item.id}>
                        <strong>{item.author_name}</strong>
                        <span className="pv-hint">{formatTime(item.created_at)}</span>
                        <p>{item.body}</p>
                      </div>
                    ))}
                  </div>

                  {pr.status === "open" && (
                    <>
                      <div className="pv-inline-form">
                        <input value={comment} onChange={(e) => setComment(e.target.value)} placeholder="写一条评论…" aria-label="评论内容" />
                        <button type="button" className="pv-btn" disabled={busy || !comment.trim()} onClick={() => void run(async () => { await api.addPrComment(projectId, pr.number, comment.trim()); setComment(""); }, "评论已发布")}>评论</button>
                      </div>
                      <div className="pv-inline-actions">
                        <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !detail.mergeable} onClick={() => void run(async () => { await api.mergePr(projectId, pr.number); }, "拉取请求已合并")}>合并</button>
                        <button type="button" className="pv-btn" disabled={busy} onClick={() => void run(async () => { await api.closePr(projectId, pr.number); }, "拉取请求已关闭")}>关闭</button>
                      </div>
                    </>
                  )}
                </>
              ) : <p className="pv-hint">加载详情…</p>}
            </div>
          )}
        </section>
      ))}
    </div>
  );
}
