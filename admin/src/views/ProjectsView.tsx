import { useEffect, useState } from "react";
import {
  api,
  CommitDetail,
  CommitSummary,
  ContentFormat,
  Discussion,
  DiscussionDetail,
  gitUrl,
  sshGitUrl,
  PrDetail,
  Project,
  ProjectDirItem,
  ProjectFile,
  PullRequest,
} from "../api";
import { CodeEditor } from "../components/CodeEditor";
import { ProjectWorkflowsPanel } from "./WorkflowsView";

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

/** 从仓库地址里推断项目名：取最后一段路径，去掉 .git 后缀。 */
function slugFromUrl(value: string): string {
  const cleaned = value.trim().replace(/\/+$/, "").replace(/\.git$/i, "");
  const name = cleaned.split(/[/:]/).filter(Boolean).pop() ?? "";
  return name.replace(/[^\p{L}\p{N}\-_]/gu, "-").slice(0, 60);
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

function firstLine(value: string): string {
  return value.split("\n")[0] || "(无提交信息)";
}

/** PR 与提交详情共用的行级差异展示。 */
function DiffList({ files }: { files: PrDetail["files"] }) {
  if (files.length === 0) {
    return <p className="hint">没有文件差异。</p>;
  }
  return (
    <ul className="diff-list">
      {files.map((file) => (
        <li key={file.path} className="diff-file">
          <div className="diff-file-head">
            <span className={`diff-status diff-${file.status}`}>
              {DIFF_STATUS_LABEL[file.status]}
            </span>
            <code>{file.path}</code>
            <span className="diff-stat">
              <span className="diff-plus">+{file.additions}</span>
              <span className="diff-minus">-{file.deletions}</span>
            </span>
          </div>
          {file.truncated ? (
            <p className="hint">文件过大或不是文本，未显示行级差异。</p>
          ) : file.hunks.length === 0 ? (
            <p className="hint">没有可见的行级变化。</p>
          ) : (
            file.hunks.map((hunk, hunkIndex) => (
              <div className="diff-hunk" key={hunkIndex}>
                <div className="diff-hunk-head">{hunk.header}</div>
                <table className="diff-table">
                  <tbody>
                    {hunk.lines.map((line, lineIndex) => (
                      <tr key={lineIndex} className={`diff-row diff-${line.kind}`}>
                        <td className="diff-no">{line.old_line ?? ""}</td>
                        <td className="diff-no">{line.new_line ?? ""}</td>
                        <td className="diff-sign">
                          {line.kind === "add" ? "+" : line.kind === "del" ? "-" : " "}
                        </td>
                        <td className="diff-text">
                          <code>{line.text || " "}</code>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            ))
          )}
        </li>
      ))}
    </ul>
  );
}

export function ProjectsView({ isAdmin }: { isAdmin: boolean }) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [showArchived, setShowArchived] = useState(false);
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
  // 提交面板内的就地反馈：全局 error/notice 在 section 顶部，滚到工作区时看不见。
  const [commitError, setCommitError] = useState("");
  const [commitNotice, setCommitNotice] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [deletions, setDeletions] = useState<string[]>([]);

  const [creating, setCreating] = useState(false);
  const [newSlug, setNewSlug] = useState("");
  const [newDescription, setNewDescription] = useState("");
  // 站内模板项目 slug；空串表示空白项目。
  const [templateSlug, setTemplateSlug] = useState("");

  // 从外部 Git 仓库导入（克隆）项目。
  const [importing, setImporting] = useState(false);
  const [importUrl, setImportUrl] = useState("");
  const [importBranch, setImportBranch] = useState("");

  const [middleTab, setMiddleTab] = useState<"files" | "discussions" | "workflows">("files");
  const [rightTab, setRightTab] = useState<"history" | "prs">("history");

  const [commits, setCommits] = useState<CommitSummary[]>([]);
  const [commitDetail, setCommitDetail] = useState<CommitDetail | null>(null);
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const [newBranch, setNewBranch] = useState("");

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
  const stagedPaths = Object.keys(drafts).filter((path) => !deletions.includes(path));
  const pendingCount = stagedPaths.length + deletions.length;

  useEffect(() => {
    void refreshProjects();
    // refreshProjects 每次渲染重建,showArchived 变化时重新拉取列表
  }, [showArchived]);

  async function refreshProjects() {
    try {
      const result = await api.projects(showArchived);
      setProjects(result.items);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function selectProject(project: Project) {
    if (
      selected &&
      selected.id !== project.id &&
      pendingCount > 0 &&
      !window.confirm("切换项目会丢弃当前待提交的更改，继续？")
    ) {
      return;
    }
    setSelected(project);
    setBranch(project.default_branch);
    setDir("");
    setFile(null);
    setDraftPath("");
    setContent("");
    setCommitMessage("");
    setDrafts({});
    setDeletions([]);
    setError("");
    setNotice("");
    setCommitError("");
    setCommitNotice("");
    setMiddleTab("files");
    setRightTab("history");
    setCommits([]);
    setCommitDetail(null);
    setSelectedCommit(null);
    setNewBranch("");
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
      void loadCommits(project, project.default_branch).catch((e) =>
        setError((e as Error).message),
      );
    } catch (e) {
      setError((e as Error).message);
    }
  }

  function switchMiddleTab(next: "files" | "discussions" | "workflows") {
    setMiddleTab(next);
    setError("");
    setNotice("");
    // 工作流面板按 projectId 自行加载，这里只负责切换。
    if (!selected || next !== "discussions") return;
    void loadDiscussions(selected).catch((e) => setError((e as Error).message));
  }

  function switchRightTab(next: "history" | "prs") {
    setRightTab(next);
    setError("");
    setNotice("");
    if (!selected) return;
    if (next === "prs") {
      void loadPrs(selected).catch((e) => setError((e as Error).message));
    } else {
      void loadCommits(selected, branch).catch((e) => setError((e as Error).message));
    }
  }

  async function loadCommits(project: Project, ref: string) {
    const result = await api.commits(project.id, ref, 100, 0);
    setCommits(result.items);
    setCommitDetail(null);
    setSelectedCommit(null);
  }

  async function openCommit(oid: string) {
    if (!selected) return;
    try {
      const result = await api.commitDetail(selected.id, oid);
      setCommitDetail(result.item);
      setSelectedCommit(oid);
      setError("");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function revertCommit() {
    if (!selected || !commitDetail) return;
    const short = commitDetail.oid.slice(0, 7);
    if (!window.confirm(`在 ${branch} 上反向应用提交 ${short}？`)) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.revertCommit(selected.id, {
        ref: branch,
        commit: commitDetail.oid,
      });
      setNotice(`已生成还原提交 ${result.item.commit.slice(0, 7)}`);
      await loadCommits(selected, branch);
      await loadTree(selected, branch, dir);
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function createBranch(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.createBranch(selected.id, {
        name: newBranch.trim(),
        from: branch,
      });
      setBranches(result.branches.length > 0 ? result.branches : [selected.default_branch]);
      setNotice(`分支 ${newBranch.trim()} 已创建`);
      setNewBranch("");
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function removeBranch(name: string) {
    if (!selected || name === selected.default_branch) return;
    if (!window.confirm(`删除分支 ${name}？`)) return;
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.deleteBranch(selected.id, name);
      const next = result.branches.length > 0 ? result.branches : [selected.default_branch];
      setBranches(next);
      setNotice(`分支 ${name} 已删除`);
      if (branch === name) {
        const fallback = selected.default_branch;
        setBranch(fallback);
        await loadTree(selected, fallback, "");
        await loadCommits(selected, fallback);
      }
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
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
    // 服务端按 merge-base 做三方合并，冲突时直接拒绝；前端先拦下，
    // 别让用户点一个注定失败、还以为会丢数据的按钮。
    if (prDetail.conflicts?.length) {
      setError(
        `无法合并：以下文件在两侧都有不同改动，请先把 ${prDetail.target_branch} 合并到 ${prDetail.source_branch} 后重试：${prDetail.conflicts.join("、")}`,
      );
      return;
    }
    const warning = prDetail.is_fast_forward
      ? ""
      : "\n\n这不是快进合并：服务端会生成双亲 merge commit，并逐文件三方合并，两侧改动都会保留。";
    if (!window.confirm(`合并 PR #${prDetail.number} 到 ${prDetail.target_branch}？${warning}`)) {
      return;
    }
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
      const item = result.item;
      setFile(item);
      setDraftPath(item.path);
      setContent(
        Object.prototype.hasOwnProperty.call(drafts, item.path)
          ? drafts[item.path]
          : item.content,
      );
      setDeletions((current) => current.filter((path) => path !== item.path));
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
    const next = drafts[path] ?? "";
    setContent(next);
    setDrafts((current) => ({ ...current, [path]: next }));
    setDeletions((current) => current.filter((item) => item !== path));
    setError("");
  }

  function clearStaged() {
    setDrafts({});
    setDeletions([]);
    setCommitMessage("");
    setCommitError("");
    setCommitNotice("");
  }

  async function commitProject() {
    if (!selected) return;
    const staged = Object.entries(drafts).filter(([path]) => !deletions.includes(path));
    const changes = [
      ...deletions.map((path) => ({ path, delete: true })),
      ...staged.map(([path, value]) => ({ path, content: value })),
    ];
    if (changes.length === 0) return;
    setBusy(true);
    setError("");
    setNotice("");
    setCommitError("");
    setCommitNotice("");
    try {
      const result = await api.commitProjectChanges(selected.id, {
        ref: branch,
        message: commitMessage || undefined,
        changes,
      });
      setDrafts({});
      setDeletions([]);
      setCommitMessage("");
      setFile(null);
      setDraftPath("");
      setContent("");
      setCommitNotice(
        `已提交 ${result.item.commit.slice(0, 7)}，共 ${result.item.paths.length} 个文件`,
      );
      // 提交已经成功，后续刷新失败只写顶部全局错误，不覆盖面板里的成功提示。
      await loadTree(selected, branch, dir).catch((e) =>
        setError((e as Error).message),
      );
      await loadCommits(selected, branch).catch((e) =>
        setError((e as Error).message),
      );
      await refreshProjects();
    } catch (e) {
      // 错误必须显示在工作区面板内，全局 error 在页面顶部，用户看不到。
      setCommitError((e as Error).message || "提交失败，请重试");
    } finally {
      setBusy(false);
    }
  }

  function removeFile() {
    if (!file) return;
    if (!window.confirm(`将 ${file.path} 标记为删除？提交项目时生效。`)) return;
    setDeletions((current) => Array.from(new Set([...current, file.path])));
    setDrafts((current) => {
      const next = { ...current };
      delete next[file.path];
      return next;
    });
    setFile(null);
    setDraftPath("");
    setContent("");
    setCommitError("");
    setCommitNotice("文件已标记删除，等待项目提交");
  }

  function renameFile() {
    if (!file) return;
    const target = window.prompt("新路径", file.path);
    if (!target || target === file.path) return;
    setDrafts((current) => ({ ...current, [target]: content }));
    setDeletions((current) => Array.from(new Set([...current, file.path])));
    setFile(null);
    setDraftPath(target);
    setContent(content);
    setCommitError("");
    setCommitNotice("重命名已加入待提交，提交项目时生效");
  }

  async function createProject(event: React.FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    try {
      const result = await api.createProject({
        slug: newSlug,
        description: newDescription,
        template: templateSlug || undefined,
      });
      setCreating(false);
      setNewSlug("");
      setNewDescription("");
      setTemplateSlug("");
      setNotice(
        result.template
          ? `项目「${result.item.slug}」已从模板「${result.template}」浅克隆创建`
          : `项目「${result.item.slug}」已创建`,
      );
      await refreshProjects();
      await selectProject(result.item);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function importProject(event: React.FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    setNotice("");
    try {
      const result = await api.importProject({
        slug: newSlug.trim(),
        description: newDescription,
        url: importUrl.trim(),
        branch: importBranch.trim() || undefined,
      });
      setImporting(false);
      setImportUrl("");
      setImportBranch("");
      setNewSlug("");
      setNewDescription("");
      setNotice(
        `项目「${result.item.slug}」已导入：${result.branches.length} 个分支、` +
          `${result.commits} 个提交、${result.objects} 个对象`,
      );
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
    const description = window.prompt("项目描述", selected.description);
    if (description === null) return;
    try {
      const result = await api.updateProject(selected.id, { description });
      setSelected(result.item);
      setNotice("项目信息已更新");
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function copyCloneUrl(kind: "https" | "ssh") {
    if (!selected) return;
    const url = kind === "https" ? gitUrl(selected.slug) : sshGitUrl(selected.slug);
    try {
      await navigator.clipboard.writeText(url);
      setNotice(`${kind.toUpperCase()} 仓库地址已复制`);
    } catch {
      setError(`无法访问剪贴板，请手动复制 ${kind.toUpperCase()} 地址`);
    }
  }

  async function archiveProject() {
    if (!selected) return;
    if (!window.confirm(`归档项目「${selected.slug}」？归档后不再出现在列表和站点中。`)) return;
    try {
      await api.updateProject(selected.id, { archived: true });
      setSelected(null);
      setNotice("项目已归档");
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function unarchiveProject() {
    if (!selected) return;
    try {
      const result = await api.updateProject(selected.id, { archived: false });
      setSelected(result.item);
      setNotice("已取消归档，项目重新出现在站点中");
      await refreshProjects();
    } catch (e) {
      setError((e as Error).message);
    }
  }

  async function deleteProject() {
    if (!selected) return;
    const slug = selected.slug;
    const typed = window.prompt(
      `永久删除项目「${slug}」？全部提交、文件、PR 与 LFS 对象都会被删除，且不可恢复。\n请输入项目名 ${slug} 确认：`,
    );
    if (typed !== slug) return;
    try {
      await api.deleteProject(selected.id);
      setSelected(null);
      setItems([]);
      setFile(null);
      setNotice(`项目「${slug}」已删除`);
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

      {selected && (
        <div className="project-topbar">
          <div className="project-topbar-main">
            <div className="project-topbar-title">
              <h3>{selected.slug}</h3>
              <span className="pill pill-branch">{selected.default_branch}</span>
              <code className="project-topbar-path">/项目/{selected.slug}/</code>
            </div>
            {selected.description && (
              <p className="project-topbar-desc">{selected.description}</p>
            )}
            <div className="project-clone-row">
              <span className="clone-label">HTTPS</span>
              <code title={gitUrl(selected.slug)}>{gitUrl(selected.slug)}</code>
              <span className="clone-label clone-label-ssh">SSH</span>
              <code title={sshGitUrl(selected.slug)}>{sshGitUrl(selected.slug)}</code>
            </div>
          </div>
          <div className="project-head-actions">
            <button type="button" onClick={() => void copyCloneUrl("https")}>
              复制 HTTPS
            </button>
            <button type="button" onClick={() => void copyCloneUrl("ssh")}>
              复制 SSH
            </button>
            <button type="button" onClick={() => void editProject()}>
              编辑信息
            </button>
            {selected.archived_at ? (
              <>
                <button type="button" onClick={() => void unarchiveProject()}>
                  取消归档
                </button>
                <button type="button" className="danger-text" onClick={() => void deleteProject()}>
                  删除
                </button>
              </>
            ) : (
              <button type="button" className="danger-text" onClick={() => void archiveProject()}>
                归档
              </button>
            )}
          </div>
        </div>
      )}

      <div className="projects-layout">
        <aside className="project-sidebar">
          <div className="project-sidebar-head">
            <div>
              <span className="project-sidebar-kicker">Workspace</span>
              <strong>项目仓库</strong>
            </div>
            <span className="project-count">{projects.length}</span>
          </div>
          <label className="project-archive-toggle">
            <input
              type="checkbox"
              checked={showArchived}
              onChange={(e) => setShowArchived(e.target.checked)}
            />
            显示已归档项目
          </label>
          <div className="project-new-row">
            <button
              type="button"
              className="project-new-button"
              onClick={() => {
                setCreating((value) => !value);
                setImporting(false);
              }}
            >
              {creating ? "取消新建" : "+ 新建项目"}
            </button>
            <button
              type="button"
              className="project-new-button"
              onClick={() => {
                setImporting((value) => !value);
                setCreating(false);
              }}
            >
              {importing ? "取消导入" : "⇣ 从 Git 导入"}
            </button>
          </div>
          {importing && (
            <form className="project-create" onSubmit={importProject}>
              <label>
                仓库地址
                <input
                  value={importUrl}
                  onChange={(e) => {
                    const value = e.target.value;
                    setImportUrl(value);
                    // 项目名还空着时按仓库地址自动填一个，用户可以再改。
                    if (!newSlug.trim()) setNewSlug(slugFromUrl(value));
                  }}
                  placeholder="https://github.com/owner/repo.git"
                  required
                />
              </label>
              <label>
                项目名
                <input
                  value={newSlug}
                  onChange={(e) => setNewSlug(e.target.value)}
                  placeholder="repo"
                  required
                />
              </label>
              <label>
                描述
                <textarea
                  value={newDescription}
                  onChange={(e) => setNewDescription(e.target.value)}
                  rows={2}
                />
              </label>
              <label>
                分支（可选）
                <input
                  value={importBranch}
                  onChange={(e) => setImportBranch(e.target.value)}
                  placeholder="留空导入全部分支"
                />
              </label>
              <p className="hint">
                服务端会克隆该公开仓库的分支与完整提交历史，导入后可直接用站内 Git 地址继续推送。
                只支持 http/https 公开仓库，且历史里不能包含二进制文件（站内 Git 仅存 UTF-8 文本）。
              </p>
              <button type="submit" disabled={busy || !importUrl.trim() || !newSlug.trim()}>
                {busy ? "克隆中…" : "开始导入"}
              </button>
            </form>
          )}
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
                描述
                <textarea
                  value={newDescription}
                  onChange={(e) => setNewDescription(e.target.value)}
                  rows={3}
                />
              </label>
              <label>
                模板（可选）
                <select
                  value={templateSlug}
                  onChange={(e) => setTemplateSlug(e.target.value)}
                >
                  <option value="">空白项目</option>
                  {projects
                    .filter((project) => !project.archived_at)
                    .map((project) => (
                      <option key={project.id} value={project.slug}>
                        {project.slug}
                        {project.description ? ` — ${project.description}` : ""}
                      </option>
                    ))}
                </select>
              </label>
              <p className="hint">
                项目会出现在 <code>/项目/{newSlug || "<项目名>"}/</code>，仓库地址为 <code>/git/</code>。
                源码请使用 UTF-8 文本，二进制文件请通过 Git LFS 推送。
              </p>
              {templateSlug && (
                <p className="hint">
                  将以浅克隆方式复制模板「{templateSlug}」默认分支的最新文件快照，
                  不带模板的提交历史；新项目的第一次提交记录为「从模板创建」。
                </p>
              )}
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
                  <span className="project-avatar">{project.slug.slice(0, 1).toUpperCase()}</span>
                  <span className="project-list-copy">
                    <strong>{project.slug}</strong>
                    {project.description && <small>{project.description}</small>}
                  </span>
                  {project.archived_at && <span className="pill pill-archived">已归档</span>}
                  <span className="project-chevron" aria-hidden="true">›</span>
                </button>
              </li>
            ))}
          </ul>
        </aside>

        {!selected ? (
          <div className="project-empty-shell">
            <div className="project-empty">
              <span className="project-empty-mark">⌘</span>
              <h3>选择一个项目</h3>
              <p>左侧选择已有仓库，或新建一个项目开始。</p>
              <button type="button" onClick={() => setCreating(true)}>
                新建项目
              </button>
            </div>
          </div>
        ) : (
          <>
            <div className="project-workbench">
              <div className="project-tabs project-middle-tabs">
                <button
                  type="button"
                  className={middleTab === "files" ? "active" : ""}
                  onClick={() => switchMiddleTab("files")}
                >
                  文件
                </button>
                <button
                  type="button"
                  className={middleTab === "discussions" ? "active" : ""}
                  onClick={() => switchMiddleTab("discussions")}
                >
                  讨论
                </button>
                {isAdmin && (
                  <button
                    type="button"
                    className={middleTab === "workflows" ? "active" : ""}
                    onClick={() => switchMiddleTab("workflows")}
                  >
                    工作流
                  </button>
                )}
              </div>
              {middleTab === "files" && (
                <div className="project-files-panel">
              <div className="row align file-toolbar">
                <label>
                  分支
                  <select
                    value={branch}
                    onChange={(e) => {
                      const next = e.target.value;
                      if (
                        pendingCount > 0 &&
                        !window.confirm("切换分支会丢弃当前待提交的更改，继续？")
                      ) {
                        return;
                      }
                      clearStaged();
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
                          <span className={`project-file-name ${item.is_dir ? "is-dir" : "is-file"}`}>
                            <span className="file-glyph" aria-hidden="true">
                              {item.is_dir ? "▸" : "·"}
                            </span>
                            {item.name}
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
                    <div>
                      <div className="editor-toolbar">
                        <span className="mono">{draftPath}</span>
                        <span className="editor-toolbar-right">
                          {Object.prototype.hasOwnProperty.call(drafts, draftPath) && (
                            <span className="staged-badge">待提交</span>
                          )}
                          {formatForPath(draftPath)} · {content.length} 字符
                        </span>
                      </div>
                      <CodeEditor
                        value={content}
                        format={formatForPath(draftPath)}
                        onChange={(value) => {
                          setContent(value);
                          setDrafts((current) => {
                            // 内容改回磁盘上的原样时把草稿删掉，
                            // 否则「敲一个字再删掉」会一直挂在待提交列表里。
                            // file 为 null 表示这是新建的还没落盘的文件，
                            // 没有「原样」可比，保持留痕。
                            const original = file?.content;
                            if (original !== undefined && value === original) {
                              if (!Object.prototype.hasOwnProperty.call(current, draftPath)) {
                                return current;
                              }
                              const rest = { ...current };
                              delete rest[draftPath];
                              return rest;
                            }
                            return { ...current, [draftPath]: value };
                          });
                        }}
                      />
                      <div className="row align editor-actions">
                        {file && (
                          <>
                            <button type="button" onClick={renameFile}>
                              重命名
                            </button>
                            <button type="button" onClick={removeFile}>
                              标记删除
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
                        <span className="hint">
                          文件修改不会立即提交；在右侧「项目提交」统一提交。
                        </span>
                      </div>
                    </div>
                  ) : (
                    <div className="editor-empty">
                      <span className="editor-empty-mark">⌁</span>
                      <h4>选择一个文件</h4>
                      <p>从左侧文件树打开文件，或新建一个文件开始编辑。</p>
                    </div>
                  )}
                </div>
              </div>
                </div>
              )}
              {middleTab === "discussions" && (
                <div className="project-discussion-panel">
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
                </div>
              )}
              {middleTab === "workflows" && isAdmin && selected && (
                <ProjectWorkflowsPanel
                  key={selected.id}
                  projectId={selected.id}
                  projectSlug={selected.slug}
                />
              )}
            </div>
            <div className="project-git">
              <div className="project-git-head">
                <div>
                  <span className="project-git-kicker">Git</span>
                  <strong>工作区</strong>
                </div>
                <span className="project-git-meta">
                  {selected.slug} · {branch}
                </span>
              </div>
              <div className="project-commit-panel">
                <div className="project-commit-head">
                  <div>
                    <span className="project-commit-kicker">Commit</span>
                    <strong>项目提交</strong>
                  </div>
                  <span className="project-commit-count">{pendingCount}</span>
                </div>
                <textarea
                  value={commitMessage}
                  onChange={(e) => setCommitMessage(e.target.value)}
                  rows={3}
                  placeholder="提交说明（可选，留空自动生成）"
                />
                <ul className="project-pending-list">
                  {deletions.map((path) => (
                    <li key={`delete-${path}`}>
                      <span className="pending-badge pending-delete">删除</span>
                      <code>{path}</code>
                    </li>
                  ))}
                  {stagedPaths.map((path) => (
                    <li key={`edit-${path}`}>
                      <span className="pending-badge">修改</span>
                      <code>{path}</code>
                    </li>
                  ))}
                  {pendingCount === 0 && (
                    <li className="hint">编辑文件、删除或重命名后会出现在这里。</li>
                  )}
                </ul>
                <button
                  type="button"
                  className="project-commit-button"
                  onClick={() => void commitProject()}
                  disabled={busy || pendingCount === 0}
                >
                  {busy
                    ? "提交中…"
                    : `提交 ${pendingCount > 0 ? `${pendingCount} 个文件` : "项目更改"}`}
                </button>
                {commitError && (
                  <p className="project-commit-feedback error" role="alert">
                    {commitError}
                  </p>
                )}
                {commitNotice && (
                  <p className="project-commit-feedback notice" role="status">
                    {commitNotice}
                  </p>
                )}
              </div>
              <div className="project-tabs">
                <button
                  type="button"
                  className={rightTab === "history" ? "active" : ""}
                  onClick={() => switchRightTab("history")}
                >
                  提交历史
                </button>
                <button
                  type="button"
                  className={rightTab === "prs" ? "active" : ""}
                  onClick={() => switchRightTab("prs")}
                >
                  Pull Requests
                </button>
              </div>



              {rightTab === "history" && (
                <div className="collab-layout">
                  <div className="collab-list">
                    <div className="collab-create">
                      <h4>分支</h4>
                      <div className="row align">
                        <select
                          value={branch}
                          onChange={(e) => {
                            const next = e.target.value;
                            if (
                              pendingCount > 0 &&
                              !window.confirm("切换分支会丢弃当前待提交的更改，继续？")
                            ) {
                              return;
                            }
                            clearStaged();
                            setBranch(next);
                            void loadCommits(selected, next).catch((err) =>
                              setError((err as Error).message),
                            );
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
                        {branch !== selected.default_branch && (
                          <button
                            type="button"
                            onClick={() => void removeBranch(branch)}
                            disabled={busy}
                          >
                            删除分支
                          </button>
                        )}
                      </div>
                      <form className="row" onSubmit={createBranch}>
                        <input
                          value={newBranch}
                          onChange={(e) => setNewBranch(e.target.value)}
                          placeholder="新分支名"
                        />
                        <button type="submit" disabled={busy || !newBranch.trim()}>
                          基于 {branch} 创建
                        </button>
                      </form>
                      <p className="hint">默认分支为 {selected.default_branch}，不能删除。</p>
                    </div>

                    <ul className="collab-items">
                      {commits.map((commit) => (
                        <li key={commit.oid}>
                          <button
                            type="button"
                            className={selectedCommit === commit.oid ? "active" : ""}
                            onClick={() => void openCommit(commit.oid)}
                          >
                            <span>{firstLine(commit.message)}</span>
                            <small>
                              {commit.oid.slice(0, 8)} · {commit.author_name} ·{" "}
                              {formatTime(commit.committed_at)}
                            </small>
                          </button>
                        </li>
                      ))}
                      {commits.length === 0 && <li className="hint">暂无提交。</li>}
                    </ul>
                  </div>

                  <div className="collab-detail">
                    {!commitDetail ? (
                      <p className="hint">选择一个提交查看详情。</p>
                    ) : (
                      <>
                        <div className="collab-detail-head">
                          <h4>{firstLine(commitDetail.message)}</h4>
                          <span className="pill">{commitDetail.oid.slice(0, 8)}</span>
                        </div>
                        <p className="hint">
                          {commitDetail.author_name} · {formatTime(commitDetail.committed_at)} ·
                          父提交{" "}
                          {commitDetail.parent_oid
                            ? commitDetail.parent_oid.slice(0, 8)
                            : "无（根提交）"}
                        </p>
                        <pre className="commit-message">{commitDetail.message}</pre>
                        <div className="row align">
                          <button
                            type="button"
                            onClick={() => void revertCommit()}
                            disabled={busy}
                          >
                            还原此提交
                          </button>
                        </div>

                        <h5>文件差异</h5>
                        <DiffList files={commitDetail.files} />
                      </>
                    )}
                  </div>
                </div>
              )}

              {rightTab === "prs" && (
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
                            {prDetail.conflicts?.length
                              ? `存在冲突，无法合并：${prDetail.conflicts.join("、")}。请先把 ${prDetail.target_branch} 合并到 ${prDetail.source_branch}。`
                              : "来源分支或目标分支不存在，无法合并。"}
                          </p>
                        )}
                        {prDetail.status === "open" && prDetail.mergeable && (
                          <p className="hint">
                            {prDetail.is_fast_forward
                              ? "可以快进合并，目标分支不会丢失改动。"
                              : "不是快进合并：会生成双亲 merge commit，并逐文件三方合并，两侧改动都会保留。"}
                            {prDetail.merge_base
                              ? ` 合并基点 ${prDetail.merge_base.slice(0, 8)}。`
                              : " 两个分支没有共同历史。"}
                          </p>
                        )}

                        <h5>文件差异</h5>
                        <DiffList files={prDetail.files} />

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


            </div>
          </>
        )}
      </div>
    </section>
  );
}
