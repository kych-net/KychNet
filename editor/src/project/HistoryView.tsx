import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@admin/api";
import { ServerDiffView } from "./DiffView";
import { useConfirm } from "../confirm";
import { useBusy } from "../useBusy";
import type { CommitDetail, CommitSummary } from "./types";
import { copyText, errText, firstLine, formatTime, shortOid } from "./types";

const PAGE = 50;

export function HistoryView({
  projectId,
  refName,
  refreshKey,
  readOnly,
  onOpenRef,
  onChanged,
  onNotify,
}: {
  projectId: string;
  refName: string;
  refreshKey: number;
  readOnly: boolean;
  onOpenRef: (ref: string) => void;
  onChanged: () => void;
  onNotify: (message: string) => void;
}) {
  const [commits, setCommits] = useState<CommitSummary[]>([]);
  const [detail, setDetail] = useState<CommitDetail | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [offset, setOffset] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [branchName, setBranchName] = useState("");
  const epoch = useRef(0);
  // 详情请求单独计数：慢响应不能覆盖用户后点的提交。
  const detailEpoch = useRef(0);
  const { busy: acting, error: actionError, run } = useBusy();
  const { confirm, confirmNode } = useConfirm();

  const load = useCallback(
    async (start: number, token: number) => {
      setBusy(true);
      setError("");
      try {
        const res = await api.commits(projectId, refName, PAGE, start);
        if (epoch.current !== token) return;
        setCommits((prev) => (start === 0 ? res.items : [...prev, ...res.items]));
        setHasMore(res.items.length === PAGE);
        setOffset(start + res.items.length);
      } catch (e) {
        if (epoch.current === token) setError(errText(e));
      } finally {
        if (epoch.current === token) setBusy(false);
      }
    },
    [projectId, refName],
  );

  useEffect(() => {
    const token = ++epoch.current;
    // 列表换了（分支 / 刷新）：作废所有在途详情请求。
    detailEpoch.current += 1;
    setCommits([]);
    setDetail(null);
    setSelected(null);
    setOffset(0);
    void load(0, token);
  }, [load, refreshKey]);

  const select = async (oid: string) => {
    if (selected === oid) {
      detailEpoch.current += 1;
      setSelected(null);
      setDetail(null);
      return;
    }
    const token = ++detailEpoch.current;
    setSelected(oid);
    setDetail(null);
    setMessage("");
    setError("");
    try {
      const res = await api.commitDetail(projectId, oid);
      if (detailEpoch.current !== token) return;
      setDetail(res.item);
    } catch (e) {
      if (detailEpoch.current === token) setError(errText(e));
    }
  };

  const revert = async () => {
    if (!selected) return;
    const target = commits.find((commit) => commit.oid === selected);
    const ok = await confirm({
      title: `还原提交 ${shortOid(selected)}？`,
      body: (
        <>
          <p>
            这会把 {shortOid(selected)}
            {target ? `「${firstLine(target.message)}」` : ""} 的改动反向应用，
            并在当前分支 <code className="pv-path">{refName}</code> 上生成一个新的还原提交。
          </p>
          <p>已有历史不会被改写，但工作区内容会随之变化。</p>
        </>
      ),
      confirmText: "生成还原提交",
      danger: true,
    });
    if (!ok) return;
    await run(async () => {
      await api.revertCommit(projectId, { ref: refName, commit: selected, message: message.trim() || undefined });
      setMessage("");
      onNotify("已生成还原提交");
      onChanged();
      void load(0, ++epoch.current);
    });
  };

  const createBranch = async () => {
    if (!selected || !branchName.trim()) return;
    await run(async () => {
      await api.createBranch(projectId, { name: branchName.trim(), from: selected });
      setBranchName("");
      onNotify(`已从 ${shortOid(selected)} 创建分支`);
      onChanged();
    });
  };

  const shownError = error || actionError;

  return (
    <div className="pv-history">
      <div className="pv-section-head">
        <div>
          <h2>提交历史</h2>
          <p className="pv-hint"><code className="pv-path">{refName}</code> · 点击提交查看差异</p>
        </div>
        <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void load(0, ++epoch.current)} disabled={busy}>刷新</button>
      </div>
      {shownError && <p className="pv-error" role="alert">{shownError}</p>}

      <div className="pv-commit-list">
        {commits.map((commit) => (
          <div className="pv-commit-item" key={commit.oid}>
            <button type="button" className={`pv-commit-row${selected === commit.oid ? " is-active" : ""}`} aria-expanded={selected === commit.oid} onClick={() => void select(commit.oid)}>
              <span className="pv-oid">{shortOid(commit.oid)}</span>
              <span className="pv-commit-msg">{firstLine(commit.message)}</span>
              <span className="pv-commit-meta">{commit.author_name} · {formatTime(commit.committed_at)}</span>
              {commit.second_parent_oid && <span className="pv-chip">合并</span>}
            </button>
            {selected === commit.oid && (
              <div className="pv-commit-detail">
                <div className="pv-inline-actions">
                  <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copyText(commit.oid).then(() => onNotify("提交 OID 已复制")).catch((e) => setError(errText(e)))}>复制 OID</button>
                  <button type="button" className="pv-btn pv-btn-ghost" onClick={() => onOpenRef(commit.oid)}>浏览此版本</button>
                </div>
                {detail ? <ServerDiffView files={detail.files} /> : <p className="pv-hint">加载差异…</p>}

                <div className="pv-detail-forms">
                  <div className="pv-inline-form">
                    <input value={message} onChange={(e) => setMessage(e.target.value)} placeholder="还原提交说明（可留空）" disabled={readOnly || acting} aria-label="还原提交说明" />
                    <button type="button" className="pv-btn" onClick={() => void revert()} disabled={readOnly || acting}>还原此提交</button>
                  </div>
                  <div className="pv-inline-form">
                    <input value={branchName} onChange={(e) => setBranchName(e.target.value)} placeholder="从此提交新建分支名" disabled={acting} aria-label="新分支名" autoCapitalize="off" spellCheck={false} />
                    <button type="button" className="pv-btn" onClick={() => void createBranch()} disabled={acting || !branchName.trim()}>新建分支</button>
                  </div>
                  {readOnly && <p className="pv-hint">当前 ref 不是分支，还原提交需要切换到分支。</p>}
                </div>
              </div>
            )}
          </div>
        ))}
      </div>

      {commits.length === 0 && !busy && !shownError && <p className="pv-hint">暂无提交。</p>}
      {hasMore && (
        <button type="button" className="pv-btn pv-btn-block" onClick={() => void load(offset, epoch.current)} disabled={busy}>加载更多</button>
      )}

      {confirmNode}
    </div>
  );
}
