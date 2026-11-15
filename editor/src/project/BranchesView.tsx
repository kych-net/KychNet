import { useMemo, useState } from "react";
import { api, gitUrl, sshGitUrl } from "@admin/api";
import { ServerDiffView } from "./DiffView";
import type { BranchCompare, Project, ProjectRefs } from "./types";
import { copyText, errText, firstLine, formatTime, shortOid } from "./types";

/** 分支 / 比较 / 合并 / 重置 / 克隆地址。 */
export function BranchesView({
  project,
  refs,
  currentRef,
  onSwitch,
  onChanged,
  onOpenRef,
  onNotify,
}: {
  project: Project;
  refs: ProjectRefs;
  currentRef: string;
  onSwitch: (ref: string) => void;
  onChanged: () => void;
  onOpenRef: (ref: string) => void;
  onNotify: (message: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [newName, setNewName] = useState("");
  const [newFrom, setNewFrom] = useState(currentRef);
  const [cmpSource, setCmpSource] = useState(currentRef);
  const [cmpTarget, setCmpTarget] = useState(refs.default_branch);
  const [compare, setCompare] = useState<BranchCompare | null>(null);
  const [mergeSource, setMergeSource] = useState(currentRef === refs.default_branch ? "" : currentRef);
  const [mergeTarget, setMergeTarget] = useState(refs.default_branch);
  const [mergeMessage, setMergeMessage] = useState("");
  const [resetCommit, setResetCommit] = useState("");
  const [resetForce, setResetForce] = useState(false);

  const refOptions = useMemo(
    () => [
      ...refs.branches.map((name) => ({ value: name, label: `分支 · ${name}` })),
      ...refs.tags.map((tag) => ({ value: tag.name, label: `标签 · ${tag.name}` })),
    ],
    [refs],
  );

  const run = async (action: () => Promise<void>, success: string) => {
    setBusy(true);
    setError("");
    try {
      await action();
      if (success) onNotify(success);
      onChanged();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const create = () =>
    run(async () => {
      await api.createBranch(project.id, { name: newName.trim(), from: newFrom });
      setNewName("");
      onSwitch(newName.trim());
    }, "分支已创建");

  const remove = (name: string) => {
    if (!window.confirm(`确定删除分支 ${name}？此操作不可撤销。`)) return;
    return run(async () => {
      await api.deleteBranch(project.id, name);
      if (currentRef === name) onSwitch(refs.default_branch);
    }, "分支已删除");
  };

  const doCompare = async () => {
    setBusy(true);
    setError("");
    try {
      const res = await api.compare(project.id, cmpSource, cmpTarget);
      setCompare(res.item);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const doMerge = () =>
    run(async () => {
      const res = await api.mergeProjectBranches(project.id, {
        source: mergeSource,
        target: mergeTarget,
        message: mergeMessage.trim() || undefined,
      });
      const item = res.item;
      onNotify(
        item.already_up_to_date
          ? "目标分支已包含来源分支的全部提交"
          : item.fast_forward
            ? "已快进合并"
            : `已生成合并提交 ${shortOid(item.commit)}`,
      );
      setMergeMessage("");
    }, "");

  const doReset = () => {
    if (!window.confirm("重置会移动分支指针，历史不会删除但工作区内容会变化。确定继续？")) return;
    return run(async () => {
      await api.resetProjectBranch(project.id, {
        ref: currentRef,
        commit: resetCommit.trim(),
        force: resetForce,
      });
      setResetCommit("");
      setResetForce(false);
    }, "分支已重置");
  };

  const copy = async (value: string) => {
    try {
      await copyText(value);
      onNotify("已复制到剪贴板");
    } catch (e) {
      setError(errText(e));
    }
  };

  const https = gitUrl(project.slug);
  const ssh = sshGitUrl(project.slug);

  return (
    <div className="pv-git">
      {error && <p className="pv-error">{error}</p>}

      <section className="pv-card">
        <h2>当前引用</h2>
        <div className="pv-inline-form">
          <select value={currentRef} onChange={(e) => onSwitch(e.target.value)} aria-label="切换分支或标签">
            {!refOptions.some((option) => option.value === currentRef) && (
              <option value={currentRef}>提交 · {shortOid(currentRef)}</option>
            )}
            {refOptions.map((option) => (
              <option key={option.value} value={option.value}>{option.label}</option>
            ))}
          </select>
          {refs.head && <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copy(refs.head!)}>{shortOid(refs.head)}</button>}
        </div>
        <p className="pv-hint">默认分支：<code className="pv-path">{refs.default_branch}</code></p>
      </section>

      <section className="pv-card">
        <h2>克隆地址</h2>
        <div className="pv-copy-row"><code className="pv-path">{https}</code><button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copy(https)}>复制</button></div>
        <div className="pv-copy-row"><code className="pv-path">{ssh}</code><button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copy(ssh)}>复制</button></div>
        <p className="pv-hint">推送需要 Git 令牌或已登录会话；令牌在管理端「用户设置」里创建。</p>
      </section>

      <section className="pv-card">
        <h2>新建分支</h2>
        <div className="pv-inline-form">
          <input value={newName} onChange={(e) => setNewName(e.target.value)} placeholder="分支名，如 feat/editor" aria-label="新分支名" autoCapitalize="off" spellCheck={false} />
          <select value={newFrom} onChange={(e) => setNewFrom(e.target.value)} aria-label="来源引用">
            {refOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
            {!refOptions.some((option) => option.value === currentRef) && <option value={currentRef}>当前提交</option>}
          </select>
          <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !newName.trim()} onClick={() => void create()}>创建</button>
        </div>
      </section>

      <section className="pv-card">
        <h2>分支 <span className="pv-count">{refs.branches.length}</span></h2>
        <div className="pv-branch-list">
          {refs.branches.map((name) => (
            <div className={`pv-branch-row${name === currentRef ? " is-active" : ""}`} key={name}>
              <span className="pv-branch-name">{name}</span>
              {name === refs.default_branch && <span className="pv-chip">默认</span>}
              <div className="pv-inline-actions">
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => onSwitch(name)} disabled={name === currentRef}>切换</button>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => onOpenRef(name)}>浏览</button>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void remove(name)} disabled={busy || name === refs.default_branch}>删除</button>
              </div>
            </div>
          ))}
        </div>
      </section>

      <section className="pv-card">
        <h2>版本比较</h2>
        <div className="pv-inline-form">
          <select value={cmpSource} onChange={(e) => setCmpSource(e.target.value)} aria-label="来源引用">
            {refOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
          </select>
          <span className="pv-arrow">→</span>
          <select value={cmpTarget} onChange={(e) => setCmpTarget(e.target.value)} aria-label="目标引用">
            {refOptions.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
          </select>
          <button type="button" className="pv-btn" onClick={() => void doCompare()} disabled={busy}>比较</button>
        </div>
        {compare && (
          <div className="pv-compare-result">
            <p className="pv-hint">
              合并基点 {compare.merge_base ? shortOid(compare.merge_base) : "无"} · 领先 {compare.commits.length} 个提交
            </p>
            {compare.commits.length > 0 && (
              <div className="pv-commit-list compact">
                {compare.commits.slice(0, 20).map((commit) => (
                  <div className="pv-commit-row" key={commit.oid}>
                    <span className="pv-oid">{shortOid(commit.oid)}</span>
                    <span className="pv-commit-msg">{firstLine(commit.message)}</span>
                    <span className="pv-commit-meta">{formatTime(commit.committed_at)}</span>
                  </div>
                ))}
              </div>
            )}
            <ServerDiffView files={compare.files} />
          </div>
        )}
      </section>

      <section className="pv-card">
        <h2>合并分支</h2>
        <div className="pv-inline-form">
          <select value={mergeSource} onChange={(e) => setMergeSource(e.target.value)} aria-label="来源分支">
            <option value="">选择来源分支</option>
            {refs.branches.map((name) => <option key={name} value={name}>{name}</option>)}
          </select>
          <span className="pv-arrow">→</span>
          <select value={mergeTarget} onChange={(e) => setMergeTarget(e.target.value)} aria-label="目标分支">
            {refs.branches.map((name) => <option key={name} value={name}>{name}</option>)}
          </select>
        </div>
        <label className="pv-field">
          <span>合并说明（可留空）</span>
          <input value={mergeMessage} onChange={(e) => setMergeMessage(e.target.value)} placeholder="合并提交说明" disabled={busy} />
        </label>
        <button type="button" className="pv-btn pv-btn-accent" disabled={busy || !mergeSource || mergeSource === mergeTarget} onClick={() => void doMerge()}>合并</button>
        <p className="pv-hint">能快进就只移动 ref；不能快进时按文件三方合并，冲突会拒绝而不是覆盖。</p>
      </section>

      <section className="pv-card pv-danger">
        <h2>重置当前分支</h2>
        <div className="pv-inline-form">
          <input value={resetCommit} onChange={(e) => setResetCommit(e.target.value)} placeholder="目标 commit OID / 分支 / 标签" aria-label="重置目标" autoCapitalize="off" spellCheck={false} />
          <button type="button" className="pv-btn" onClick={() => void doReset()} disabled={busy || !resetCommit.trim()}>重置</button>
        </div>
        <label className="pv-check">
          <input type="checkbox" checked={resetForce} onChange={(e) => setResetForce(e.target.checked)} />
          <span>强制重置到当前历史之外的提交</span>
        </label>
        <p className="pv-hint">只移动分支指针，不删除任何提交对象；默认只允许退回当前分支已有的提交。</p>
      </section>
    </div>
  );
}
