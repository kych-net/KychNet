import { useState } from "react";
import { api } from "@admin/api";
import type { ProjectRefs } from "./types";
import { copyText, errText, formatTime, shortOid } from "./types";

/** 标签：列表 / 新建（轻量或附注）/ 删除 / 按标签浏览。 */
export function TagsView({
  projectId,
  refs,
  currentRef,
  onSwitch,
  onChanged,
  onNotify,
}: {
  projectId: string;
  refs: ProjectRefs;
  currentRef: string;
  onSwitch: (ref: string) => void;
  onChanged: () => void;
  onNotify: (message: string) => void;
}) {
  const [name, setName] = useState("");
  const [from, setFrom] = useState(currentRef);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const create = async () => {
    setBusy(true);
    setError("");
    try {
      await api.createProjectTag(projectId, {
        name: name.trim(),
        ref: from,
        message: message.trim() || undefined,
      });
      setName("");
      setMessage("");
      onNotify("标签已创建");
      onChanged();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (tag: string) => {
    if (!window.confirm(`确定删除标签 ${tag}？`)) return;
    setBusy(true);
    setError("");
    try {
      await api.deleteProjectTag(projectId, tag);
      onNotify("标签已删除");
      onChanged();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="pv-git">
      {error && <p className="pv-error">{error}</p>}
      <section className="pv-card">
        <h2>新建标签</h2>
        <div className="pv-inline-form">
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="标签名，如 v1.0.0" aria-label="标签名" autoCapitalize="off" spellCheck={false} />
          <select value={from} onChange={(e) => setFrom(e.target.value)} aria-label="标签指向">
            {[...refs.branches, ...refs.tags.map((tag) => tag.name)].map((value) => (
              <option key={value} value={value}>{value}</option>
            ))}
            {!refs.branches.includes(currentRef) && !refs.tags.some((tag) => tag.name === currentRef) && (
              <option value={currentRef}>当前提交 {shortOid(currentRef)}</option>
            )}
          </select>
          <button type="button" className="pv-btn pv-btn-accent" onClick={() => void create()} disabled={busy || !name.trim()}>打标签</button>
        </div>
        <label className="pv-field">
          <span>附注说明（留空为轻量标签）</span>
          <textarea value={message} onChange={(e) => setMessage(e.target.value)} rows={2} placeholder="版本说明…" disabled={busy} />
        </label>
      </section>

      <section className="pv-card">
        <h2>标签 <span className="pv-count">{refs.tags.length}</span></h2>
        {refs.tags.length === 0 && <p className="pv-hint">还没有标签。</p>}
        <div className="pv-tag-list">
          {refs.tags.map((tag) => (
            <div className={`pv-tag-row${currentRef === tag.name ? " is-active" : ""}`} key={tag.name}>
              <div className="pv-tag-main">
                <span className="pv-tag-name">🏷 {tag.name}</span>
                {tag.tag_oid && <span className="pv-chip">附注</span>}
                <span className="pv-oid">{shortOid(tag.oid)}</span>
              </div>
              <p className="pv-tag-msg">{tag.message || "轻量标签"}</p>
              <p className="pv-hint">{tag.author_name} · {formatTime(tag.created_at)}</p>
              <div className="pv-inline-actions">
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => onSwitch(tag.name)} disabled={currentRef === tag.name}>浏览</button>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void copyText(tag.oid).then(() => onNotify("OID 已复制")).catch((e) => setError(errText(e)))}>复制 OID</button>
                <button type="button" className="pv-btn pv-btn-ghost" onClick={() => void remove(tag.name)} disabled={busy}>删除</button>
              </div>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
