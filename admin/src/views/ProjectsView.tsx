import { useEffect, useState } from "react";
import {
  api,
  ContentFormat,
  Discussion,
  DiscussionDetail,
  gitUrl,
  PrDetail,
  Project,
  ProjectDirItem,
  ProjectFile,
  PullRequest,
} from "../api";
import { CodeEditor } from "../components/CodeEditor";

function formatForPath(path: string): ContentFormat {
  const extension = path.split(".").pop()?.toLowerCase();
  switch (extension) {
    case "md":
      return "markdown";
    case "html":
      return "html";
    case "css":
      return "css";
    case "typ":
      return "typst";
    case "wj":
      return "wikidot";
    default:
      return "markdown";
  }
}

function joinPath(dir: string, name: string): string {
  const clean = name.trim().replace(/^\/+/, "");
  return dir ? `${dir}/${clean}` : clean;
}

const PR_STATUS_LABEL: Record<PullRequest["status"], string> = {
  open: "开放",
  merged: "已合并",
  closed: "已关闭",
};

const DIFF_STATUS_LABEL: Record<"add" | "modify" | "delete", string> = {
  add: "新增",
  modify: "修改",
  delete: "删除",
};

function formatTime(value: string): string {
  return new Date(value).toLocaleString();
}

export function ProjectsView() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);

  const [selected, setSelected] = useState<Project | null>(null);
  const [branches, setBranches] = useState<string[]>([]);
  const [branch, setBranch] = useState("main");
  const [dir, setDir] = useState("");
  const [items, setItems] = useState<ProjectDirItem[]>([]);

  const [file, setFile] = useState<ProjectFile | null>(null);
  const [draftPath, setDraftPath] = useState("");
  const [content, setContent] = useState("");
  const [commitMessage, setCommitMessage] = useState("");

  const [creating, setCreating] = useState(false);
  const [newSlug, setNewSlug] = useState("");
  const [newTitle, setNewTitle] = useState("");
  const [newDescription, setNewDescription] = useState("");

  const [tab, setTab] = useState<"files" | "prs" | "discussions">("files");

  const [prs, setPrs] = useState<PullRequest[]>([]);
  const [prDetail, setPrDetail] = useState<PrDetail | null>(null);
  const [selectedPr, setSelectedPr] = useState<number | null>(null);
  const [prTitle, setPrTitle] = useState("");
  const [prBody, setPrBody] = useState("");
  const [prSource, setPrSource] = useState("");
  const [prTarget, setPrTarget] = useState("");
  const [prComment, setPrComment] = useState("");

  const [discussions, setDiscussions] = useState<Discussion[]>([]);
  const [discussionDetail, setDiscussionDetail] = useState<DiscussionDetail | null>(null);
  const [selectedDiscussion, setSelectedDiscussion] = useState<string | null>(null);
  const [discussionTitle, setDiscussionTitle] = useState("");
  const [discussionBody, setDiscussionBody] = useState("");
  const [discussionComment, setDiscussionComment] = useState("");

  useEffect(() => {
    void refreshProjects();
  }, []);

  async function refreshProjects() {
    try {
      const result = await api.projects();
      setProjects(result.items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function selectProject(project: Project) {
    setSelected(project);
    setBranch(project.default_branch);
    setDir("");
    setFile(null);
    setDraftPath("");
    setContent("");
    setCommitMessage("");
    setError("");
    setNotice("");
    setTab("files");
    setPrs([]);
    setPrDetail(null);
    setSelectedPr(null);
    setPrSource(project.default_branch);
    setPrTarget(project.default_branch);
    setDiscussions([]);
    setDiscussionDetail(null);
    setSelectedDiscussion(null);
    try {
      const detail = await api.project(project.id);
      setBranches(detail.branches.length > 0 ? detail.branches : [project.default_branch]);
      await loadTree(project, project.default_branch, "");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function switchTab(next: "files" | "prs" | "discussions") {
    setTab(next);
    setError("");
    setNotice("");
    if (!selected) return;
    if (next === "prs") {
      void loadPrs(selected).catch((e) => setError((e as Error).message));
    }
    if (next === "discussions") {
      void loadDiscussions(selected).catch((e) => setError((e as Error).message));
    }
  }

  async function loadPrs(project: Project) {
    const result = await api.prs(project.id);
    setPrs(result.items);
  }

  async function openPr(number: number) {
    if (!selected) return;
    try {
      const result = await api.prDetail(selected.id, number);
      setPrDetail(result.item);
      setSelectedPr(number);
      setPrComment("");
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function createPr(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.createPr(selected.id, {
        title: prTitle,
        body: prBody,
        source_branch: prSource,
        target_branch: prTarget,
      });
      setPrTitle("");
      setPrBody("");
      setNotice(`PR #${result.item.number} 已创建`);
      await loadPrs(selected);
      await openPr(result.item.number);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function commentPr(event: React.FormEvent) {
    event.preventDefault();
    if (!selected || selectedPr === null) return;
    setBusy(true);
    setError("");
    try {
      await api.addPrComment(selected.id, selectedPr, prComment);
      setPrComment("");
      await openPr(selectedPr);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function mergePr() {
    if (!selected || !prDetail) return;
    if (!window.confirm(`合并 PR #${prDetail.number} 到 ${prDetail.target_branch}？`)) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.mergePr(selected.id, prDetail.number);
      setNotice(
        result.item.target_branch === selected.default_branch
          ? `已合并到 ${result.item.target_branch}，并触发整站编译`
          : `已合并到 ${result.item.target_branch}`,
      );
      await loadPrs(selected);
      await openPr(prDetail.number);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function closePr() {
    if (!selected || !prDetail) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await api.closePr(selected.id, prDetail.number);
      setNotice(`PR #${prDetail.number} 已关闭`);
      await loadPrs(selected);
      await openPr(prDetail.number);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function loadDiscussions(project: Project) {
    const result = await api.discussions(project.id);
    setDiscussions(result.items);
  }

  async function openDiscussion(id: string) {
    if (!selected) return;
    try {
      const result = await api.discussionDetail(selected.id, id);
      setDiscussionDetail(result.item);
      setSelectedDiscussion(id);
      setDiscussionComment("");
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function createDiscussion(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.createDiscussion(selected.id, {
        title: discussionTitle,
        body: discussionBody,
      });
      setDiscussionTitle("");
      setDiscussionBody("");
      setNotice("讨论已创建");
      await loadDiscussions(selected);
      await openDiscussion(result.item.id);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function commentDiscussion(event: React.FormEvent) {
    event.preventDefault();
    if (!selected || !selectedDiscussion) return;
    setBusy(true);
    setError("");
    try {
      await api.addDiscussionComment(selected.id, selectedDiscussion, discussionComment);
      setDiscussionComment("");
      await openDiscussion(selectedDiscussion);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function closeDiscussion() {
    if (!selected || !selectedDiscussion) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await api.closeDiscussion(selected.id, selectedDiscussion);
      setNotice("讨论已关闭");
      await loadDiscussions(selected);
      await openDiscussion(selectedDiscussion);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function loadTree(project: Project, ref: string, path: string) {
    const result = await api.projectTree(project.id, ref, path);
    setItems(result.items);
    setBranches(result.branches.length > 0 ? result.branches : [project.default_branch]);
    setDir(path);
  }

  async function openDir(path: string) {
    if (!selected) return;
    try {
      setFile(null);
      await loadTree(selected, branch, path);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function openFile(path: string) {
    if (!selected) return;
    try {
      const result = await api.projectFile(selected.id, branch, path);
      if (!result.item) {
        setError("文件不存在");
        return;
      }
      setFile(result.item);
      setDraftPath(result.item.path);
      setContent(result.item.content);
      setCommitMessage("");
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function newFile() {
    const name = window.prompt("新文件路径（相对于当前目录）", "untitled.md");
    if (!name) return;
    const path = joinPath(dir, name);
    setFile(null);
    setDraftPath(path);
    setContent("");
    setCommitMessage("");
    setError("");
  }

  async function saveFile(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.saveProjectFile(selected.id, {
        ref: branch,
        path: draftPath,
        content,
        message: commitMessage || undefined,
        base_oid: file?.oid,
      });
      setFile({ path: draftPath, oid: result.item.oid, content });
      setNotice(`已提交 ${result.item.commit.slice(0, 7)}，并触发整站编译`);
      await loadTree(selected, branch, dir);
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function removeFile() {
    if (!selected || !file) return;
    if (!window.confirm(`删除 ${file.path}？`)) return;
    try {
      await api.deleteProjectFile(selected.id, branch, file.path);
      setFile(null);
      setDraftPath("");
      setContent("");
      setNotice("文件已删除，并触发整站编译");
      await loadTree(selected, branch, dir);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function renameFile() {
    if (!selected || !file) return;
    const target = window.prompt("新路径", file.path);
    if (!target || target === file.path) return;
    try {
      await api.moveProjectFile(selected.id, {
        ref: branch,
        from: file.path,
        to: target,
        message: commitMessage || undefined,
      });
      setFile(null);
      setDraftPath("");
      setContent("");
      setNotice("文件已移动，并触发整站编译");
      await loadTree(selected, branch, dir);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function createProject(event: React.FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const result = await api.createProject({
        slug: newSlug,
        title: newTitle,
        description: newDescription,
      });
      setCreating(false);
      setNewSlug("");
      setNewTitle("");
      setNewDescription("");
      setNotice(`项目「${result.item.title}」已创建`);
      await refreshProjects();
      await selectProject(result.item);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function editProject() {
    if (!selected) return;
    const title = window.prompt("项目标题", selected.title);
    if (title === null) return;
    const description = window.prompt("项目描述", selected.description) ?? "";
    try {
      const result = await api.updateProject(selected.id, { title, description });
      setSelected(result.item);
      setNotice("项目信息已更新");
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function archiveProject() {
    if (!selected) return;
    if (!window.confirm(`归档项目「${selected.title}」？归档后不再出现在列表和站点中。`)) return;
    try {
      await api.updateProject(selected.id, { archived: true });
      setSelected(null);
      setNotice("项目已归档");
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  const crumbs = dir ? dir.split("/") : [];

  return (
    <section>
      <h2>项目</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <div className="projects-layout">
        <aside className="project-sidebar">
          <button type="button" onClick={() => setCreating((value) => !value)}>
            {creating ? "取消新建" : "新建项目"}
          </button>
          {creating && (
            <form className="project-create" onSubmit={createProject}>
              <label>
                项目名
                <input
                  value={newSlug}
                  onChange={(e) => setNewSlug(e.target.value)}
                  placeholder="kych-net 或 我的项目"
                  required
                />
              </label>
              <label>
                标题
                <input value={newTitle} onChange={(e) => setNewTitle(e.target.value)} required />
              </label>
              <label>
                描述
                <textarea
                  value={newDescription}
                  onChange={(e) => setNewDescription(e.target.value)}
                  rows={3}
                />
              </label>
              <p className="hint">
                项目会出现在 <code>/项目/{newSlug || "<项目名>"}/</code>，仓库地址为 <code>/git/</code>。
                目前只接受 UTF-8 文本，二进制要等 LFS 实现。
              </p>
              <button type="submit" disabled={busy}>
                创建
              </button>
            </form>
          )}

          <ul className="project-list">
            {projects.map((project) => (
              <li key={project.id}>
                <button
                  type="button"
                  className={selected?.id === project.id ? "active" : ""}
                  onClick={() => void selectProject(project)}
                >
                  <span>{project.title}</span>
                  <small>/{project.slug}</small>
                </button>
              </li>
            ))}
          </ul>
        </aside>

        <div className="project-main">
          {!selected ? (
            <p className="hint">选择或新建一个项目。</p>
          ) : (
            <>
              <div className="project-head">
                <div>
                  <h3>{selected.title}</h3>
                  <p className="hint">
                    内容路径 <code>/项目/{selected.slug}/</code> · 仓库{" "}
                    <code>{gitUrl(selected.slug)}</code>
                  </p>
                </div>
                <div className="project-head-actions">
                  <button type="button" onClick={() => void editProject()}>
                    编辑信息
                  </button>
                  <button type="button" onClick={() => void archiveProject()}>
                    归档
                  </button>
                </div>
              </div>

              <div className="project-tabs">
                <button
                  type="button"
                  className={tab === "files" ? "active" : ""}
                  onClick={() => switchTab("files")}
                >
                  文件
                </button>
                <button
                  type="button"
                  className={tab === "prs" ? "active" : ""}
                  onClick={() => switchTab("prs")}
                >
                  Pull Requests
                </button>
                <button
                  type="button"
                  className={tab === "discussions" ? "active" : ""}
                  onClick={() => switchTab("discussions")}
                >
                  讨论
                </button>
              </div>

              {tab === "files" && (
                <>
              <div className="row align">
                <label>
                  分支
                  <select
                    value={branch}
                    onChange={(e) => {
                      const next = e.target.value;
                      setBranch(next);
                      setFile(null);
                      setDraftPath("");
                      setContent("");
                      void loadTree(selected, next, "").catch((err) =>
                        setError((err as Error).message),
                      );
                    }}
                  >
                    {branches.map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </select>
                </label>
                <button type="button" onClick={newFile}>
                  新建文件
                </button>
              </div>

              <div className="project-browser">
                <div className="project-files">
                  <div className="project-crumbs">
                    <button type="button" onClick={() => void openDir("")}>
                      根目录
                    </button>
                    {crumbs.map((crumb, index) => {
                      const path = crumbs.slice(0, index + 1).join("/");
                      return (
                        <button key={path} type="button" onClick={() => void openDir(path)}>
                          / {crumb}
                        </button>
                      );
                    })}
                  </div>
                  <ul>
                    {items.map((item) => (
                      <li key={item.path}>
                        <button
                          type="button"
                          className={file?.path === item.path ? "active" : ""}
                          onClick={() =>
                            item.is_dir ? void openDir(item.path) : void openFile(item.path)
                          }
                        >
                          <span>
                            {item.is_dir ? "[目录]" : "[文件]"} {item.name}
                          </span>
                          {!item.is_dir && <small>{item.size} B</small>}
                        </button>
                      </li>
                    ))}
                  </ul>
                  {items.length === 0 && <p className="hint">空目录</p>}
                </div>

                <div className="project-editor">
                  {draftPath ? (
                    <form onSubmit={saveFile}>
                      <div className="editor-toolbar">
                        <span className="mono">{draftPath}</span>
                        <span className="editor-toolbar-right">
                          {formatForPath(draftPath)} · {content.length} 字符
                        </span>
                      </div>
                      <CodeEditor
                        value={content}
                        format={formatForPath(draftPath)}
                        onChange={setContent}
                      />
                      <div className="row">
                        <label>
                          提交信息
                          <input
                            value={commitMessage}
                            onChange={(e) => setCommitMessage(e.target.value)}
                            placeholder="留空自动生成"
                          />
                        </label>
                      </div>
                      <div className="row align">
                        <button type="submit" disabled={busy}>
                          提交
                        </button>
                        {file && (
                          <>
                            <button type="button" onClick={() => void renameFile()}>
                              重命名
                            </button>
                            <button type="button" onClick={() => void removeFile()}>
                              删除
                            </button>
                          </>
                        )}
                        <button
                          type="button"
                          onClick={() => {
                            setFile(null);
                            setDraftPath("");
                            setContent("");
                          }}
                        >
                          关闭
                        </button>
                      </div>
                    </form>
                  ) : (
                    <p className="hint">选择文件查看或编辑。</p>
                  )}
                </div>
              </div>
                </>
              )}

              {tab === "prs" && (
                <div className="collab-layout">
                  <div className="collab-list">
                    <form className="collab-create" onSubmit={createPr}>
                      <h4>新建 Pull Request</h4>
                      <label>
                        标题
                        <input
                          value={prTitle}
                          onChange={(e) => setPrTitle(e.target.value)}
                          required
                        />
                      </label>
                      <label>
                        说明
                        <textarea
                          value={prBody}
                          onChange={(e) => setPrBody(e.target.value)}
                          rows={3}
                        />
                      </label>
                      <div className="row">
                        <label>
                          来源分支
                          <select value={prSource} onChange={(e) => setPrSource(e.target.value)}>
                            {branches.map((name) => (
                              <option key={name} value={name}>
                                {name}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          目标分支
                          <select value={prTarget} onChange={(e) => setPrTarget(e.target.value)}>
                            {branches.map((name) => (
                              <option key={name} value={name}>
                                {name}
                              </option>
                            ))}
                          </select>
                        </label>
                      </div>
                      <button
                        type="submit"
                        disabled={busy || !prTitle.trim() || prSource === prTarget}
                      >
                        创建
                      </button>
                    </form>

                    <ul className="collab-items">
                      {prs.map((pr) => (
                        <li key={pr.id}>
                          <button
                            type="button"
                            className={selectedPr === pr.number ? "active" : ""}
                            onClick={() => void openPr(pr.number)}
                          >
                            <span>
                              #{pr.number} {pr.title}
                            </span>
                            <small>
                              {PR_STATUS_LABEL[pr.status]} · {pr.source_branch} → {pr.target_branch}
                            </small>
                            <small>
                              {pr.author_name} · {formatTime(pr.created_at)}
                            </small>
                          </button>
                        </li>
                      ))}
                      {prs.length === 0 && <li className="hint">暂无 Pull Request。</li>}
                    </ul>
                  </div>

                  <div className="collab-detail">
                    {!prDetail ? (
                      <p className="hint">选择一个 PR 查看详情。</p>
                    ) : (
                      <>
                        <div className="collab-detail-head">
                          <h4>
                            #{prDetail.number} {prDetail.title}
                          </h4>
                          <span className={`pill status-${prDetail.status}`}>
                            {PR_STATUS_LABEL[prDetail.status]}
                          </span>
                        </div>
                        <p className="hint">
                          {prDetail.source_branch} → {prDetail.target_branch} ·{" "}
                          {prDetail.author_name} · {formatTime(prDetail.created_at)}
                          {prDetail.merged_at
                            ? ` · 合并于 ${formatTime(prDetail.merged_at)}`
                            : ""}
                        </p>
                        {prDetail.body && <p className="collab-body">{prDetail.body}</p>}

                        <div className="row align">
                          <button
                            type="button"
                            onClick={() => void mergePr()}
                            disabled={
                              busy || prDetail.status !== "open" || !prDetail.mergeable
                            }
                          >
                            合并
                          </button>
                          <button
                            type="button"
                            onClick={() => void closePr()}
                            disabled={busy || prDetail.status !== "open"}
                          >
                            关闭
                          </button>
                        </div>
                        {prDetail.status === "open" && !prDetail.mergeable && (
                          <p className="hint">
                            目标分支包含来源分支没有的提交，需要先把目标分支合并进来源分支。
                          </p>
                        )}

                        <h5>文件差异</h5>
                        {prDetail.files.length === 0 ? (
                          <p className="hint">没有文件差异。</p>
                        ) : (
                          <ul className="diff-list">
                            {prDetail.files.map((file) => (
                              <li key={file.path}>
                                <span className={`diff-status diff-${file.status}`}>
                                  {DIFF_STATUS_LABEL[file.status]}
                                </span>
                                <code>{file.path}</code>
                              </li>
                            ))}
                          </ul>
                        )}

                        <h5>评论</h5>
                        <ul className="comment-list">
                          {prDetail.comments.map((comment) => (
                            <li key={comment.id}>
                              <div className="comment-meta">
                                {comment.author_name} · {formatTime(comment.created_at)}
                              </div>
                              <p className="collab-body">{comment.body}</p>
                            </li>
                          ))}
                          {prDetail.comments.length === 0 && (
                            <li className="hint">还没有评论。</li>
                          )}
                        </ul>
                        <form onSubmit={commentPr} className="comment-form">
                          <textarea
                            value={prComment}
                            onChange={(e) => setPrComment(e.target.value)}
                            rows={3}
                            placeholder="发表评论"
                          />
                          <button type="submit" disabled={busy || !prComment.trim()}>
                            发表评论
                          </button>
                        </form>
                      </>
                    )}
                  </div>
                </div>
              )}

              {tab === "discussions" && (
                <div className="collab-layout">
                  <div className="collab-list">
                    <form className="collab-create" onSubmit={createDiscussion}>
                      <h4>新建讨论</h4>
                      <label>
                        标题
                        <input
                          value={discussionTitle}
                          onChange={(e) => setDiscussionTitle(e.target.value)}
                          required
                        />
                      </label>
                      <label>
                        正文
                        <textarea
                          value={discussionBody}
                          onChange={(e) => setDiscussionBody(e.target.value)}
                          rows={4}
                        />
                      </label>
                      <button
                        type="submit"
                        disabled={busy || !discussionTitle.trim() || !discussionBody.trim()}
                      >
                        创建
                      </button>
                    </form>

                    <ul className="collab-items">
                      {discussions.map((item) => (
                        <li key={item.id}>
                          <button
                            type="button"
                            className={selectedDiscussion === item.id ? "active" : ""}
                            onClick={() => void openDiscussion(item.id)}
                          >
                            <span>{item.title}</span>
                            <small>
                              {item.author_name} · {formatTime(item.created_at)} · {item.comment_count} 条评论
                              {item.closed_at ? " · 已关闭" : ""}
                            </small>
                          </button>
                        </li>
                      ))}
                      {discussions.length === 0 && <li className="hint">暂无讨论。</li>}
                    </ul>
                  </div>

                  <div className="collab-detail">
                    {!discussionDetail ? (
                      <p className="hint">选择一个讨论查看详情。</p>
                    ) : (
                      <>
                        <div className="collab-detail-head">
                          <h4>{discussionDetail.title}</h4>
                          <span className={`pill ${discussionDetail.closed_at ? "status-closed" : "status-open"}`}>
                            {discussionDetail.closed_at ? "已关闭" : "进行中"}
                          </span>
                        </div>
                        <p className="hint">
                          {discussionDetail.author_name} ·{" "}
                          {formatTime(discussionDetail.created_at)}
                          {discussionDetail.closed_at
                            ? ` · 关闭于 ${formatTime(discussionDetail.closed_at)}`
                            : ""}
                        </p>
                        <p className="collab-body">{discussionDetail.body}</p>

                        <div className="row align">
                          <button
                            type="button"
                            onClick={() => void closeDiscussion()}
                            disabled={busy || discussionDetail.closed_at !== null}
                          >
                            关闭讨论
                          </button>
                        </div>

                        <h5>评论</h5>
                        <ul className="comment-list">
                          {discussionDetail.comments.map((comment) => (
                            <li key={comment.id}>
                              <div className="comment-meta">
                                {comment.author_name} · {formatTime(comment.created_at)}
                              </div>
                              <p className="collab-body">{comment.body}</p>
                            </li>
                          ))}
                          {discussionDetail.comments.length === 0 && (
                            <li className="hint">还没有评论。</li>
                          )}
                        </ul>
                        <form onSubmit={commentDiscussion} className="comment-form">
                          <textarea
                            value={discussionComment}
                            onChange={(e) => setDiscussionComment(e.target.value)}
                            rows={3}
                            placeholder={
                              discussionDetail.closed_at ? "讨论已关闭" : "发表评论"
                            }
                            disabled={discussionDetail.closed_at !== null}
                          />
                          <button
                            type="submit"
                            disabled={
                              busy ||
                              discussionDetail.closed_at !== null ||
                              !discussionComment.trim()
                            }
                          >
                            发表评论
                          </button>
                        </form>
                      </>
                    )}
                  </div>
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </section>
  );
}
