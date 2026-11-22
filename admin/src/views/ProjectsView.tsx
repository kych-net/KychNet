import { useEffect, useRef, useState } from "react";
import {
  api,
  BranchCompare,
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
import { useFeedback } from "../hooks/useFeedback";
import { useBusy } from "../hooks/useBusy";
import { useConfirm, usePrompt } from "../components/ConfirmProvider";

/** 提交历史每次加载的页大小。 */
const COMMIT_PAGE = 100;

function formatForPath(path: string): ContentFormat {
  const extension = path.split(".").pop()?.toLowerCase();
  switch (extension) {
    case "md":
    case "markdown":
      return "markdown";
    case "html":
    case "htm":
    case "xml":
      return "html";
    case "svg":
      return "svg";
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

/** 项目内 SVG 预览：沙箱 iframe 里白底居中展示，禁用脚本，避免仓库里的 SVG 执行代码。 */
function svgPreviewDocument(svg: string): string {
  return `<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: light; }
  html, body { margin: 0; padding: 0; min-height: 100%; }
  body { display: grid; place-items: center; min-height: 100vh; padding: 1rem;
    background-color: #fff;
    background-image:
      linear-gradient(45deg, #eee 25%, transparent 25%),
      linear-gradient(-45deg, #eee 25%, transparent 25%),
      linear-gradient(45deg, transparent 75%, #eee 75%),
      linear-gradient(-45deg, transparent 75%, #eee 75%);
    background-size: 16px 16px;
    background-position: 0 0, 0 8px, 8px -8px, -8px 0; }
  svg { max-width: 100%; max-height: 85vh; width: auto; height: auto; }
</style></head><body>${svg}</body></html>`;
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
  const { error, notice, setError, setNotice } = useFeedback();
  const confirm = useConfirm();
  const promptText = usePrompt();
  // 动作按面板拆分 busy：某个面板在跑时不再让其他面板的按钮一起变灰、文案错乱。
  // useBusy 在 await confirm 之前就已上锁，确认框期间也不会重复提交。
  const formBusy = useBusy(); // 新建 / 导入项目
  const projectBusy = useBusy(); // 编辑信息 / 归档 / 删除项目
  const branchBusy = useBusy(); // 创建 / 删除分支
  const commitBusy = useBusy(); // 提交项目更改
  const loadMoreBusy = useBusy(); // 加载更多提交
  const revertBusy = useBusy(); // 还原提交
  const prCreateBusy = useBusy(); // 新建 PR
  const prCommentBusy = useBusy(); // PR 评论
  const prActionBusy = useBusy(); // 合并 / 关闭 PR
  const discussionCreateBusy = useBusy(); // 新建讨论
  const discussionCommentBusy = useBusy(); // 讨论评论
  const discussionActionBusy = useBusy(); // 关闭讨论

  const [selected, setSelected] = useState<Project | null>(null);
  const [branches, setBranches] = useState<string[]>([]);
  const [branch, setBranch] = useState("main");
  const [dir, setDir] = useState("");
  const [items, setItems] = useState<ProjectDirItem[]>([]);

  const [file, setFile] = useState<ProjectFile | null>(null);
  const [draftPath, setDraftPath] = useState("");
  const [content, setContent] = useState("");
  // SVG 文件默认展开预览；用户对单个文件切换后保持到下次打开别的类型。
  const [svgPreviewOn, setSvgPreviewOn] = useState(true);
  const [commitMessage, setCommitMessage] = useState("");
  // 提交面板内的就地反馈：全局 error/notice 在 section 顶部，滚到工作区时看不见。
  const [commitError, setCommitError] = useState("");
  const [commitNotice, setCommitNotice] = useState("");
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [deletions, setDeletions] = useState<string[]>([]);

  // 代际守卫：快速打开文件 / 切换分支时递增代际号，响应回来比对，
  // 不一致就丢弃，避免慢响应覆盖新选择。文件与目录树各自独立计数。
  const fileGen = useRef(0);
  const treeGen = useRef(0);
  // 详情（PR / 提交 / 讨论）共用一个代际号：切换项目或打开另一条详情都会
  // 立即作废在途响应，避免慢响应把旧详情盖回新选择上。
  const detailGen = useRef(0);
  // 列表请求的独立代际：提交列表与 PR 列表各自只受自己的新请求影响。
  const commitListGen = useRef(0);
  const prListGen = useRef(0);
  const discussionListGen = useRef(0);
  // 渲染期同步的最新值，供异步回调判断「发起时的项目是否仍是当前项目」。
  const selectedRef = useRef<Project | null>(null);
  selectedRef.current = selected;
  const branchRef = useRef(branch);
  branchRef.current = branch;

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
  const [rightTab, setRightTab] = useState<"history" | "prs" | "compare">("history");

  const [commits, setCommits] = useState<CommitSummary[]>([]);
  const [commitDetail, setCommitDetail] = useState<CommitDetail | null>(null);
  const [selectedCommit, setSelectedCommit] = useState<string | null>(null);
  const [newBranch, setNewBranch] = useState("");
  // 分支下拉是受控的：用户在确认框里点「取消」时 DOM 值已是新分支，
  // 递增这个 key 让 select 重建，值回到真正的当前分支。
  const [branchSelectKey, setBranchSelectKey] = useState(0);
  // 提交历史分页：每次拉一页，往下追加。
  const [commitsOffset, setCommitsOffset] = useState(0);

  const [prs, setPrs] = useState<PullRequest[]>([]);
  const [prDetail, setPrDetail] = useState<PrDetail | null>(null);
  const [selectedPr, setSelectedPr] = useState<number | null>(null);
  const [prTitle, setPrTitle] = useState("");
  const [prBody, setPrBody] = useState("");
  const [prSource, setPrSource] = useState("");
  const [prTarget, setPrTarget] = useState("");
  const [prComment, setPrComment] = useState("");

  // 分支对比：source 相对 target 的领先提交与文件差异。
  const [cmpSource, setCmpSource] = useState("");
  const [cmpTarget, setCmpTarget] = useState("");
  const [cmpResult, setCmpResult] = useState<BranchCompare | null>(null);
  const [cmpBusy, setCmpBusy] = useState(false);

  const [discussions, setDiscussions] = useState<Discussion[]>([]);
  const [discussionDetail, setDiscussionDetail] = useState<DiscussionDetail | null>(null);
  const [selectedDiscussion, setSelectedDiscussion] = useState<string | null>(null);
  // 详情面板内的就地错误：全局 error 在页面顶部，滚到详情后看不到。
  const [commitDetailError, setCommitDetailError] = useState("");
  const [prError, setPrError] = useState("");
  const [discussionError, setDiscussionError] = useState("");
  // 打开当前提交详情时所属的项目：切换项目后据此拒绝误操作。
  const [commitDetailProject, setCommitDetailProject] = useState<string | null>(null);
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
      !(
        await confirm("切换项目会丢弃当前待提交的更改，继续？", {
          title: "未提交的更改",
          confirmLabel: "继续切换",
          danger: true,
        })
      )
    ) {
      return;
    }
    setSelected(project);
    setBranch(project.default_branch);
    setDir("");
    // 切换项目作废仍在途的文件、详情与列表请求。
    fileGen.current += 1;
    detailGen.current += 1;
    commitListGen.current += 1;
    prListGen.current += 1;
    discussionListGen.current += 1;
    setCommitDetailError("");
    setPrError("");
    setDiscussionError("");
    setCommitDetailProject(null);
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
    setCommitsOffset(0);
    setNewBranch("");
    setPrs([]);
    setPrDetail(null);
    setSelectedPr(null);
    setPrSource(project.default_branch);
    setCmpSource(project.default_branch);
    setCmpTarget(project.default_branch);
    setCmpResult(null);
    setPrTarget(project.default_branch);
    setDiscussions([]);
    setDiscussionDetail(null);
    setSelectedDiscussion(null);
    try {
      const detail = await api.project(project.id);
      // 快速连点两个项目时，先发起的那次响应回来不能再覆盖当前项目的分支列表。
      if (selectedRef.current?.id !== project.id) return;
      setBranches(detail.branches.length > 0 ? detail.branches : [project.default_branch]);
      await loadTree(project, project.default_branch, "");
      void loadCommits(project, project.default_branch).catch((e) =>
        setError((e as Error).message),
      );
    } catch (e) {
      if (selectedRef.current?.id !== project.id) return;
      setError((e as Error).message);
    }
  }

  /**
   * 切换分支的统一入口：两个分支下拉（文件面板 / 提交历史面板）都走这里。
   * 未提交改动先确认、清空待提交与所有详情、再同时刷新文件树与提交历史，
   * 避免出现「切了分支但提交历史还停在旧分支」的半更新状态。
   */
  async function switchBranch(next: string) {
    if (!selected || next === branch) return;
    if (
      pendingCount > 0 &&
      !(await confirm("切换分支会丢弃当前待提交的更改，继续？", {
        title: "未提交的更改",
        confirmLabel: "继续切换",
        danger: true,
      }))
    ) {
      setBranchSelectKey((value) => value + 1);
      return;
    }
    const project = selected;
    clearStaged();
    setBranch(next);
    // 作废仍在途的文件 / 详情 / 列表请求。
    fileGen.current += 1;
    detailGen.current += 1;
    setFile(null);
    setDraftPath("");
    setContent("");
    setCommitDetail(null);
    setSelectedCommit(null);
    setCommitDetailError("");
    setCommitDetailProject(null);
    setPrDetail(null);
    setSelectedPr(null);
    setPrError("");
    setDiscussionDetail(null);
    setSelectedDiscussion(null);
    setDiscussionError("");
    setError("");
    setNotice("");
    void loadTree(project, next, "").catch((err) => setError((err as Error).message));
    void loadCommits(project, next).catch((err) => setError((err as Error).message));
  }

  function switchMiddleTab(next: "files" | "discussions" | "workflows") {
    setMiddleTab(next);
    setError("");
    setNotice("");
    // 工作流面板按 projectId 自行加载，这里只负责切换。
    if (!selected || next !== "discussions") return;
    void loadDiscussions(selected).catch((e) => setError((e as Error).message));
  }

  function switchRightTab(next: "history" | "prs" | "compare") {
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

  async function loadCommits(project: Project, ref: string, append = false) {
    const gen = ++commitListGen.current;
    const offset = append ? commitsOffset : 0;
    const result = await api.commits(project.id, ref, COMMIT_PAGE, offset);
    // 项目/分支已切换，或已有更新的加载发起：丢弃这次陈旧响应。
    if (gen !== commitListGen.current || selectedRef.current?.id !== project.id) return;
    setCommits((prev) => (append ? [...prev, ...result.items] : result.items));
    setCommitsOffset(offset + result.items.length);
    setCommitDetail(null);
    setSelectedCommit(null);
  }

  async function loadMoreCommits() {
    if (!selected) return;
    await loadMoreBusy.run(async () => {
      try {
        await loadCommits(selected!, branch, true);
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function runCompare() {
    if (!selected || !cmpSource || !cmpTarget) return;
    setCmpBusy(true);
    setError("");
    try {
      const result = await api.compare(selected.id, cmpSource, cmpTarget);
      setCmpResult(result.item);
    } catch (e) {
      setCmpResult(null);
      setError((e as Error).message);
    } finally {
      setCmpBusy(false);
    }
  }

  async function openCommit(oid: string) {
    if (!selected) return;
    const project = selected;
    const gen = ++detailGen.current;
    try {
      const result = await api.commitDetail(project.id, oid);
      // 已切走（切项目 / 打开了另一条详情）：丢弃这次响应。
      if (gen !== detailGen.current || selectedRef.current?.id !== project.id) return;
      setCommitDetail(result.item);
      setSelectedCommit(oid);
      setCommitDetailProject(project.id);
      setCommitDetailError("");
      setError("");
    } catch (e) {
      if (gen !== detailGen.current) return;
      setCommitDetailError((e as Error).message);
    }
  }

  async function revertCommit() {
    if (!selected || !commitDetail) return;
    // 详情可能来自上一个项目：先确认它仍属于当前项目，再动手。
    if (commitDetailProject !== selected.id) {
      setError("项目已切换，请重新打开该详情");
      return;
    }
    const project = selected;
    const targetBranch = branchRef.current;
    const detail = commitDetail;
    await revertBusy.run(async () => {
      const short = detail.oid.slice(0, 7);
      if (
        !(await confirm(`在 ${targetBranch} 上反向应用提交 ${short}？`, {
          title: "反向应用提交",
          confirmLabel: "反向应用",
          danger: true,
        }))
      ) return;
      setError("");
      setNotice("");
      setCommitDetailError("");
      try {
        const result = await api.revertCommit(project.id, {
          ref: targetBranch,
          commit: detail.oid,
        });
        setNotice(`已生成还原提交 ${result.item.commit.slice(0, 7)}`);
        // 还原期间已切换项目：只提示结果，不再刷新上一个项目的工作区。
        if (selectedRef.current?.id !== project.id) return;
        await loadCommits(project, targetBranch);
        await loadTree(project, targetBranch, dir);
        await refreshProjects();
      } catch (e) {
        setCommitDetailError((e as Error).message || "还原失败，请重试");
      }
    });
  }

  async function createBranch(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    const project = selected;
    const name = newBranch.trim();
    await branchBusy.run(async () => {
      setError("");
      setNotice("");
      try {
        const result = await api.createBranch(project.id, { name, from: branchRef.current });
        if (selectedRef.current?.id !== project.id) return;
        setBranches(result.branches.length > 0 ? result.branches : [project.default_branch]);
        setNotice(`分支 ${name} 已创建`);
        setNewBranch("");
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function removeBranch(name: string) {
    if (!selected || name === selected.default_branch) return;
    const project = selected;
    await branchBusy.run(async () => {
      if (
        !(await confirm(`删除分支 ${name}？`, {
          title: "删除分支",
          confirmLabel: "删除",
          danger: true,
        }))
      ) return;
      setError("");
      setNotice("");
      try {
        const result = await api.deleteBranch(project.id, name);
        if (selectedRef.current?.id !== project.id) return;
        const next = result.branches.length > 0 ? result.branches : [project.default_branch];
        setBranches(next);
        setNotice(`分支 ${name} 已删除`);
        if (branchRef.current === name) {
          const fallback = project.default_branch;
          setBranch(fallback);
          detailGen.current += 1;
          fileGen.current += 1;
          setFile(null);
          setDraftPath("");
          setContent("");
          await loadTree(project, fallback, "");
          await loadCommits(project, fallback);
        }
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function loadPrs(project: Project) {
    const gen = ++prListGen.current;
    const result = await api.prs(project.id);
    // 项目已切换或已有更新的 PR 列表请求：丢弃陈旧响应。
    if (gen !== prListGen.current || selectedRef.current?.id !== project.id) return;
    setPrs(result.items);
  }

  async function openPr(number: number) {
    if (!selected) return;
    const project = selected;
    const gen = ++detailGen.current;
    try {
      const result = await api.prDetail(project.id, number);
      if (gen !== detailGen.current || selectedRef.current?.id !== project.id) return;
      setPrDetail(result.item);
      setSelectedPr(number);
      setPrComment("");
      setPrError("");
      setError("");
    } catch (e) {
      if (gen !== detailGen.current) return;
      setPrError((e as Error).message);
    }
  }

  async function createPr(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    const project = selected;
    await prCreateBusy.run(async () => {
      setError("");
      setNotice("");
      try {
        const result = await api.createPr(project.id, {
          title: prTitle,
          body: prBody,
          source_branch: prSource,
          target_branch: prTarget,
        });
        if (selectedRef.current?.id !== project.id) return;
        setPrTitle("");
        setPrBody("");
        setNotice(`PR #${result.item.number} 已创建`);
        await loadPrs(project);
        await openPr(result.item.number);
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function commentPr(event: React.FormEvent) {
    event.preventDefault();
    if (!selected || selectedPr === null) return;
    const project = selected;
    const number = selectedPr;
    await prCommentBusy.run(async () => {
      setError("");
      try {
        await api.addPrComment(project.id, number, prComment);
        if (selectedRef.current?.id !== project.id) return;
        setPrComment("");
        await openPr(number);
      } catch (e) {
        setPrError((e as Error).message);
      }
    });
  }

  async function mergePr() {
    if (!selected || !prDetail) return;
    // 详情可能属于上一个项目：先确认它仍属于当前项目，再动手。
    if (prDetail.project_id !== selected.id) {
      setError("项目已切换，请重新打开该详情");
      return;
    }
    // 服务端按 merge-base 做三方合并，冲突时直接拒绝；前端先拦下，
    // 别让用户点一个注定失败、还以为会丢数据的按钮。
    if (prDetail.conflicts?.length) {
      setError(
        `无法合并：以下文件在两侧都有不同改动，请先把 ${prDetail.target_branch} 合并到 ${prDetail.source_branch} 后重试：${prDetail.conflicts.join("、")}`,
      );
      return;
    }
    const project = selected;
    const detail = prDetail;
    const warning = detail.is_fast_forward
      ? ""
      : "\n\n这不是快进合并：服务端会生成双亲 merge commit，并逐文件三方合并，两侧改动都会保留。";
    await prActionBusy.run(async () => {
      if (
        !(await confirm(`合并 PR #${detail.number} 到 ${detail.target_branch}？${warning}`, {
          title: "合并 Pull Request",
          confirmLabel: "合并",
        }))
      ) {
        return;
      }
      setError("");
      setNotice("");
      setPrError("");
      try {
        const result = await api.mergePr(project.id, detail.number);
        const message =
          result.item.target_branch === project.default_branch
            ? `已合并到 ${result.item.target_branch}，并触发整站编译`
            : `已合并到 ${result.item.target_branch}`;
        setNotice(message);
        // 合并期间已切换项目：只提示结果，不再刷新上一个项目的 PR 列表。
        if (selectedRef.current?.id !== project.id) return;
        await loadPrs(project);
        await openPr(detail.number);
      } catch (e) {
        setPrError((e as Error).message || "合并失败，请重试");
      }
    });
  }

  async function closePr() {
    if (!selected || !prDetail) return;
    if (prDetail.project_id !== selected.id) {
      setError("项目已切换，请重新打开该详情");
      return;
    }
    const project = selected;
    const detail = prDetail;
    await prActionBusy.run(async () => {
      setError("");
      setNotice("");
      setPrError("");
      try {
        await api.closePr(project.id, detail.number);
        setNotice(`PR #${detail.number} 已关闭`);
        if (selectedRef.current?.id !== project.id) return;
        await loadPrs(project);
        await openPr(detail.number);
      } catch (e) {
        setPrError((e as Error).message || "关闭失败，请重试");
      }
    });
  }

  async function loadDiscussions(project: Project) {
    const gen = ++discussionListGen.current;
    const result = await api.discussions(project.id);
    if (gen !== discussionListGen.current || selectedRef.current?.id !== project.id) return;
    setDiscussions(result.items);
  }

  async function openDiscussion(id: string) {
    if (!selected) return;
    const project = selected;
    const gen = ++detailGen.current;
    try {
      const result = await api.discussionDetail(project.id, id);
      if (gen !== detailGen.current || selectedRef.current?.id !== project.id) return;
      setDiscussionDetail(result.item);
      setSelectedDiscussion(id);
      setDiscussionComment("");
      setDiscussionError("");
      setError("");
    } catch (e) {
      if (gen !== detailGen.current) return;
      setDiscussionError((e as Error).message);
    }
  }

  async function createDiscussion(event: React.FormEvent) {
    event.preventDefault();
    if (!selected) return;
    const project = selected;
    await discussionCreateBusy.run(async () => {
      setError("");
      setNotice("");
      try {
        const result = await api.createDiscussion(project.id, {
          title: discussionTitle,
          body: discussionBody,
        });
        if (selectedRef.current?.id !== project.id) return;
        setDiscussionTitle("");
        setDiscussionBody("");
        setNotice("讨论已创建");
        await loadDiscussions(project);
        await openDiscussion(result.item.id);
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function commentDiscussion(event: React.FormEvent) {
    event.preventDefault();
    if (!selected || !selectedDiscussion) return;
    const project = selected;
    const id = selectedDiscussion;
    await discussionCommentBusy.run(async () => {
      setError("");
      try {
        await api.addDiscussionComment(project.id, id, discussionComment);
        if (selectedRef.current?.id !== project.id) return;
        setDiscussionComment("");
        await openDiscussion(id);
      } catch (e) {
        setDiscussionError((e as Error).message);
      }
    });
  }

  async function closeDiscussion() {
    if (!selected || !selectedDiscussion) return;
    if (discussionDetail && discussionDetail.project_id !== selected.id) {
      setError("项目已切换，请重新打开该详情");
      return;
    }
    const project = selected;
    const id = selectedDiscussion;
    await discussionActionBusy.run(async () => {
      setError("");
      setNotice("");
      setDiscussionError("");
      try {
        await api.closeDiscussion(project.id, id);
        if (selectedRef.current?.id !== project.id) return;
        setNotice("讨论已关闭");
        await loadDiscussions(project);
        await openDiscussion(id);
      } catch (e) {
        setDiscussionError((e as Error).message || "关闭讨论失败，请重试");
      }
    });
  }

  async function loadTree(project: Project, ref: string, path: string) {
    const gen = ++treeGen.current;
    try {
      const result = await api.projectTree(project.id, ref, path);
      // 代际或项目已变：丢弃陈旧响应（含快速切换项目时的慢响应）。
      if (gen !== treeGen.current || selectedRef.current?.id !== project.id) return;
      setItems(result.items);
      setBranches(result.branches.length > 0 ? result.branches : [project.default_branch]);
      setDir(path);
    } catch (e) {
      // 已被更新的加载取代时，陈旧请求的错误也一并丢弃。
      if (gen === treeGen.current && selectedRef.current?.id === project.id) throw e;
    }
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
    const gen = ++fileGen.current;
    try {
      const result = await api.projectFile(selected.id, branch, path);
      if (gen !== fileGen.current) return;
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
      if (gen !== fileGen.current) return;
      setError((e as Error).message);
    }
  }

  async function newFile() {
    const name = await promptText("新建文件（路径相对于当前目录）", {
      title: "新建文件",
      label: "文件路径",
      initial: "untitled.md",
      confirmLabel: "创建",
      validate: (value) => (value.trim() ? null : "请填写文件路径"),
    });
    if (name === null) return;
    const path = joinPath(dir, name);
    fileGen.current += 1;
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
    const project = selected;
    const targetBranch = branchRef.current;
    const targetDir = dir;
    await commitBusy.run(async () => {
      setError("");
      setNotice("");
      setCommitError("");
      setCommitNotice("");
      try {
        const result = await api.commitProjectChanges(project.id, {
          ref: targetBranch,
          message: commitMessage || undefined,
          changes,
        });
        if (selectedRef.current?.id !== project.id) {
          // 提交期间已切换项目：只提示结果，不再改动上一个项目的工作区状态。
          setCommitNotice(
            `已提交 ${result.item.commit.slice(0, 7)}，共 ${result.item.paths.length} 个文件`,
          );
          return;
        }
        setDrafts({});
        setDeletions([]);
        setCommitMessage("");
        fileGen.current += 1;
        setFile(null);
        setDraftPath("");
        setContent("");
        setCommitNotice(
          `已提交 ${result.item.commit.slice(0, 7)}，共 ${result.item.paths.length} 个文件`,
        );
        // 提交已经成功，后续刷新失败只写顶部全局错误，不覆盖面板里的成功提示。
        await loadTree(project, targetBranch, targetDir).catch((e) =>
          setError((e as Error).message),
        );
        await loadCommits(project, targetBranch).catch((e) =>
          setError((e as Error).message),
        );
        await refreshProjects();
      } catch (e) {
        // 错误必须显示在工作区面板内，全局 error 在页面顶部，用户看不到。
        setCommitError((e as Error).message || "提交失败，请重试");
      }
    });
  }

  async function removeFile() {
    if (!file) return;
    if (
      !(await confirm(`将 ${file.path} 标记为删除？提交项目时生效。`, {
        title: "删除文件",
        confirmLabel: "标记删除",
        danger: true,
      }))
    ) return;
    fileGen.current += 1;
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

  async function renameFile() {
    if (!file) return;
    const source = file.path;
    const target = await promptText("重命名文件", {
      title: "重命名文件",
      label: "新路径",
      initial: source,
      confirmLabel: "重命名",
      validate: (value) => {
        const next = value.trim();
        if (!next) return "请填写新路径";
        if (next === source) return "路径没有变化";
        // 目标已存在于待提交草稿或当前目录的文件列表时，不能覆盖。
        if (Object.prototype.hasOwnProperty.call(drafts, next)) {
          return `已存在待提交的文件：${next}`;
        }
        if (items.some((item) => !item.is_dir && item.path === next)) {
          return `目标路径已存在：${next}`;
        }
        if (deletions.includes(next)) return `目标路径已被标记删除：${next}`;
        return null;
      },
    });
    if (target === null) return;
    const next = target.trim();
    if (next === source) return;
    fileGen.current += 1;
    setDrafts((current) => ({ ...current, [next]: content }));
    setDeletions((current) => Array.from(new Set([...current, source])));
    setFile(null);
    setDraftPath(next);
    setContent(content);
    setCommitError("");
    setCommitNotice("重命名已加入待提交，提交项目时生效");
  }

  async function createProject(event: React.FormEvent) {
    event.preventDefault();
    await formBusy.run(async () => {
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
      }
    });
  }

  async function importProject(event: React.FormEvent) {
    event.preventDefault();
    await formBusy.run(async () => {
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
      }
    });
  }

  async function editProject() {
    if (!selected) return;
    const project = selected;
    await projectBusy.run(async () => {
      const description = await promptText("修改项目描述", {
        title: "编辑项目信息",
        label: "项目描述",
        initial: project.description,
        placeholder: "留空表示没有描述",
        confirmLabel: "保存",
      });
      if (description === null) return;
      try {
        const result = await api.updateProject(project.id, { description });
        if (selectedRef.current?.id === project.id) setSelected(result.item);
        setNotice("项目信息已更新");
        await refreshProjects();
      } catch (e) {
        setError((e as Error).message);
      }
    });
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
    const project = selected;
    await projectBusy.run(async () => {
      if (
        !(await confirm(`归档项目「${project.slug}」？归档后不再出现在列表和站点中。`, {
          title: "归档项目",
          confirmLabel: "归档",
        }))
      ) return;
      try {
        await api.updateProject(project.id, { archived: true });
        if (selectedRef.current?.id === project.id) setSelected(null);
        setNotice("项目已归档");
        await refreshProjects();
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function unarchiveProject() {
    if (!selected) return;
    const project = selected;
    await projectBusy.run(async () => {
      try {
        const result = await api.updateProject(project.id, { archived: false });
        if (selectedRef.current?.id === project.id) setSelected(result.item);
        setNotice("已取消归档，项目重新出现在站点中");
        await refreshProjects();
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  async function deleteProject() {
    if (!selected) return;
    const project = selected;
    const slug = project.slug;
    await projectBusy.run(async () => {
      const typed = await promptText(
        `永久删除项目「${slug}」？全部提交、文件、PR 与 LFS 对象都会被删除，且不可恢复。请输入项目名 ${slug} 确认：`,
        {
          title: "删除项目",
          label: "项目名",
          placeholder: slug,
          confirmLabel: "永久删除",
        },
      );
      if (typed === null) return;
      if (typed.trim() !== slug) {
        setError(`项目名不匹配：需要输入「${slug}」才能删除，本次未执行任何操作。`);
        return;
      }
      try {
        await api.deleteProject(project.id);
        if (selectedRef.current?.id === project.id) {
          setSelected(null);
          setItems([]);
          setFile(null);
        }
        setNotice(`项目「${slug}」已删除`);
        await refreshProjects();
      } catch (e) {
        setError((e as Error).message);
      }
    });
  }

  const crumbs = dir ? dir.split("/") : [];
  const currentFormat = draftPath ? formatForPath(draftPath) : null;
  const isSvg = currentFormat === "svg";
  const workbenchClasses = `project-editor-workbench${isSvg && svgPreviewOn ? " with-preview" : ""}`;

  return (
    <section>
      <h2>项目</h2>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notice && (
        <p className="notice" role="status">
          {notice}
        </p>
      )}

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
              <button
                type="submit"
                disabled={formBusy.busy || !importUrl.trim() || !newSlug.trim()}
              >
                {formBusy.busy ? "克隆中…" : "开始导入"}
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
              <button type="submit" disabled={formBusy.busy}>
                {formBusy.busy ? "创建中…" : "创建"}
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
              {/* 就近提示：文件 / 讨论操作出错时不必滚动到页面顶部才能看到。 */}
              <div className="project-inline-feedback" aria-live="polite">
                {error && <p className="project-commit-feedback error">{error}</p>}
                {notice && <p className="project-commit-feedback notice">{notice}</p>}
              </div>
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
                    key={`files-branch-${branchSelectKey}`}
                    value={branch}
                    onChange={(e) => void switchBranch(e.target.value)}
                  >
                    {branches.map((name) => (
                      <option key={name} value={name}>
                        {name}
                      </option>
                    ))}
                  </select>
                </label>
                <button type="button" onClick={() => void newFile()}>
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
                          {currentFormat} · {content.length} 字符
                          {isSvg && (
                            <button
                              type="button"
                              className="preview-toggle"
                              onClick={() => setSvgPreviewOn((value) => !value)}
                            >
                              {svgPreviewOn ? "隐藏预览" : "预览"}
                            </button>
                          )}
                        </span>
                      </div>
                      <div className={workbenchClasses}>
                        <div className="project-editor-code">
                          <CodeEditor
                            value={content}
                            format={currentFormat ?? "markdown"}
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
                        </div>
                        {isSvg && svgPreviewOn && (
                          <div className="preview-pane svg-preview-pane">
                            <div className="preview-head">
                              <span>SVG 预览</span>
                            </div>
                            <iframe
                              className="preview-frame"
                              title="SVG 预览"
                              sandbox=""
                              srcDoc={svgPreviewDocument(content)}
                            />
                          </div>
                        )}
                      </div>
                      <div className="row align editor-actions">
                        {file && (
                          <>
                            <button type="button" onClick={() => void renameFile()}>
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
                            fileGen.current += 1;
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
                        disabled={
                          discussionCreateBusy.busy ||
                          !discussionTitle.trim() ||
                          !discussionBody.trim()
                        }
                      >
                        {discussionCreateBusy.busy ? "创建中…" : "创建"}
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
                    {discussionError && (
                      <p className="error" role="alert">
                        {discussionError}
                      </p>
                    )}
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
                            disabled={
                              discussionActionBusy.busy || discussionDetail.closed_at !== null
                            }
                          >
                            {discussionActionBusy.busy ? "关闭中…" : "关闭讨论"}
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
                              discussionCommentBusy.busy ||
                              discussionDetail.closed_at !== null ||
                              !discussionComment.trim()
                            }
                          >
                            {discussionCommentBusy.busy ? "发表中…" : "发表评论"}
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
              {/* 分支 / 提交 / PR 操作的就近提示，同样用 aria-live 播报。 */}
              <div className="project-inline-feedback" aria-live="polite">
                {error && <p className="project-commit-feedback error">{error}</p>}
                {notice && <p className="project-commit-feedback notice">{notice}</p>}
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
                  disabled={commitBusy.busy || pendingCount === 0}
                >
                  {commitBusy.busy
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
                  className={rightTab === "compare" ? "active" : ""}
                  onClick={() => switchRightTab("compare")}
                >
                  分支对比
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
                          key={`history-branch-${branchSelectKey}`}
                          value={branch}
                          onChange={(e) => void switchBranch(e.target.value)}
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
                            disabled={branchBusy.busy}
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
                        <button type="submit" disabled={branchBusy.busy || !newBranch.trim()}>
                          {branchBusy.busy ? "创建中…" : `基于 ${branch} 创建`}
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
                    {commits.length > 0 &&
                      commits.length === commitsOffset &&
                      commits.length % COMMIT_PAGE === 0 && (
                        <button
                          type="button"
                          className="collab-load-more"
                          onClick={() => void loadMoreCommits()}
                          disabled={loadMoreBusy.busy}
                        >
                          加载更多
                        </button>
                      )}
                  </div>

                  <div className="collab-detail">
                    {commitDetailError && (
                      <p className="error" role="alert">
                        {commitDetailError}
                      </p>
                    )}
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
                            disabled={revertBusy.busy}
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

              {rightTab === "compare" && (
                <div className="collab-layout">
                  <div className="collab-list">
                    <div className="collab-create">
                      <h4>对比分支</h4>
                      <div className="row">
                        <label>
                          来源（领先方）
                          <select value={cmpSource} onChange={(e) => setCmpSource(e.target.value)}>
                            {branches.map((name) => (
                              <option key={name} value={name}>
                                {name}
                              </option>
                            ))}
                          </select>
                        </label>
                        <label>
                          目标（基准方）
                          <select value={cmpTarget} onChange={(e) => setCmpTarget(e.target.value)}>
                            {branches.map((name) => (
                              <option key={name} value={name}>
                                {name}
                              </option>
                            ))}
                          </select>
                        </label>
                      </div>
                      <button
                        type="button"
                        disabled={cmpBusy || !cmpSource || !cmpTarget || cmpSource === cmpTarget}
                        onClick={() => void runCompare()}
                      >
                        {cmpBusy ? "对比中…" : "开始对比"}
                      </button>
                      {cmpSource === cmpTarget && (
                        <p className="hint">来源与目标相同，请选择不同分支。</p>
                      )}
                    </div>

                    {cmpResult && (
                      <div className="collab-create">
                        <h4>领先提交（{cmpResult.commits.length}）</h4>
                        {cmpResult.merge_base && (
                          <p className="hint">
                            合并基点 {cmpResult.merge_base.slice(0, 8)}。
                          </p>
                        )}
                        <ul className="collab-items">
                          {cmpResult.commits.map((commit) => (
                            <li key={commit.oid}>
                              <span>{firstLine(commit.message)}</span>
                              <small>
                                {commit.oid.slice(0, 8)} · {commit.author_name} ·{" "}
                                {formatTime(commit.committed_at)}
                              </small>
                            </li>
                          ))}
                          {cmpResult.commits.length === 0 && (
                            <li className="hint">{cmpSource} 没有领先 {cmpTarget} 的提交。</li>
                          )}
                        </ul>
                      </div>
                    )}
                  </div>

                  <div className="collab-detail">
                    {!cmpResult ? (
                      <p className="hint">选择两个分支后开始对比。</p>
                    ) : cmpResult.files.length === 0 ? (
                      <p className="hint">
                        两个分支的文件内容完全一致（可能只有空的快进关系）。
                      </p>
                    ) : (
                      <>
                        <div className="row align">
                          <span className="project-commit-kicker">Diff</span>
                          <strong>文件差异（{cmpResult.files.length}）</strong>
                        </div>
                        <DiffList files={cmpResult.files} />
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
                        disabled={prCreateBusy.busy || !prTitle.trim() || prSource === prTarget}
                      >
                        {prCreateBusy.busy ? "创建中…" : "创建"}
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
                    {prError && (
                      <p className="error" role="alert">
                        {prError}
                      </p>
                    )}
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
                              prActionBusy.busy ||
                              prDetail.status !== "open" ||
                              !prDetail.mergeable
                            }
                          >
                            {prActionBusy.busy ? "处理中…" : "合并"}
                          </button>
                          <button
                            type="button"
                            onClick={() => void closePr()}
                            disabled={prActionBusy.busy || prDetail.status !== "open"}
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
                          <button type="submit" disabled={prCommentBusy.busy || !prComment.trim()}>
                            {prCommentBusy.busy ? "发表中…" : "发表评论"}
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
