import { useEffect, useState } from "react";
import {
  api,
  RepoBranchStats,
  RepoCommitStats,
  RepoStats,
  SystemResources,
} from "../api";

const POLL_MS = 10_000;

function fmtBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value >= 100 || unit === 0 ? 0 : 1)} ${units[unit]}`;
}

function fmtDuration(secs: number): string {
  const days = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  return days > 0 ? `${days} 天 ${hours} 小时` : `${hours} 小时`;
}

/** 把 ISO 时间显示成简短的本地时间；无效值原样返回。 */
function fmtTime(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/** 简单的用量条：超过 85% 变警示色。 */
function UsageBar({ used, total }: { used: number; total: number }) {
  const percent = total > 0 ? Math.min(100, Math.round((used / total) * 100)) : 0;
  // 数字必须放在条外面：.res-bar 只有 8px 高且 overflow:hidden，
  // 文字塞进去会被裁掉，页面上只剩一条空条。
  return (
    <div className="res-usage">
      <div className="res-bar">
        <div
          className={percent >= 85 ? "res-bar-fill warn" : "res-bar-fill"}
          style={{ width: `${percent}%` }}
        />
      </div>
      <span className="res-bar-label">
        {percent}% · {fmtBytes(used)} / {fmtBytes(total)}
      </span>
    </div>
  );
}

/** 分支列表：标签形式展示分支名与 tip。 */
function BranchList({ branches }: { branches: RepoBranchStats[] }) {
  if (branches.length === 0) return <p className="hint">无分支</p>;
  return (
    <div className="res-branches">
      {branches.map((b) => (
        <span className="res-branch" key={b.name} title={`${b.name} @ ${b.tip_oid}`}>
          <b>{b.name}</b>
          <code>{b.short_oid}</code>
        </span>
      ))}
    </div>
  );
}

/** 最近提交列表。 */
function CommitList({ commits }: { commits: RepoCommitStats[] }) {
  if (commits.length === 0) return <p className="hint">暂无提交</p>;
  return (
    <ul className="res-commits">
      {commits.map((c) => (
        <li key={c.oid} className="res-commit">
          <code className="res-commit-oid" title={c.oid}>
            {c.short_oid}
          </code>
          <span className="res-commit-msg" title={c.message}>
            {c.message || "(无提交信息)"}
          </span>
          <span className="res-commit-size" title="对象原始字节 / PG 存储字节">
            {fmtBytes(c.size)} / {fmtBytes(c.pg_bytes)}
          </span>
          <span className="res-commit-meta">
            {c.author_name} · {fmtTime(c.committed_at)}
          </span>
        </li>
      ))}
    </ul>
  );
}

/** 单个仓库的存储占用卡片，默认折叠明细以免页面过长。 */
function RepoCard({ repo }: { repo: RepoStats }) {
  const hasDisk = repo.disk_bytes > 0 || repo.disk_packs > 0;
  return (
    <details className="res-repo">
      <summary className="res-repo-summary">
        <span className="res-repo-title">{repo.slug}.git</span>
        <span className="res-repo-quick">
          <span title="PG 对象存储字节（压缩后）">PG {fmtBytes(repo.pg_bytes)}</span>
          {hasDisk && (
            <span title="磁盘 packfile + idx 字节">磁盘 {fmtBytes(repo.disk_bytes)}</span>
          )}
          <span title="对象数">{repo.objects} 对象</span>
          <span title="提交数">{repo.commits} 提交</span>
        </span>
      </summary>
      <div className="res-repo-body">
        <div className="res-repo-stats">
          <div><b>{fmtBytes(repo.pg_bytes)}</b><span>PG 占用（压缩后）</span></div>
          <div><b>{fmtBytes(repo.raw_bytes)}</b><span>对象原始大小</span></div>
          <div><b>{fmtBytes(repo.disk_bytes)}</b><span>硬盘占用 {repo.disk_packs ? `· ${repo.disk_packs} packs` : ""}</span></div>
          <div><b>{repo.commits}</b><span>提交数</span></div>
          <div><b>{repo.branches}</b><span>分支</span></div>
          <div><b>{repo.tags}</b><span>标签</span></div>
        </div>
        <p className="res-repo-note">
          默认分支 <code>{repo.default_branch}</code> · 大小列显示「原始 / PG 存储」，对象在分支间共享，勿按提交累加。
        </p>

        <h5 className="res-repo-subtitle">分支（{repo.branches}）</h5>
        <BranchList branches={repo.branch_list} />

        <h5 className="res-repo-subtitle">最近提交（{repo.commits} 条中的前 {repo.commit_list.length} 条）</h5>
        <CommitList commits={repo.commit_list} />
      </div>
    </details>
  );
}

/**
 * 资源监控：负载 / 内存 / 磁盘 / 数据库存储 / 各仓库存储占用，10 秒自动刷新。
 * 指标全部现读，不做历史曲线——要长期趋势应该交给系统级监控。
 */
export function ResourcesView() {
  const [res, setRes] = useState<SystemResources | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let alive = true;
    // 上一次结果的时间戳：标签页切回来时不必立刻重扫（后端那次统计要扫
    // git_objects 与 LFS 目录），短时间内直接用现有数据。
    let lastLoadedAt = 0;
    async function load(force = false) {
      if (!force && Date.now() - lastLoadedAt < POLL_MS / 2) return;
      try {
        const result = await api.systemResources();
        lastLoadedAt = Date.now();
        if (alive) {
          setRes(result);
          setError("");
        }
      } catch (e) {
        if (alive) setError((e as Error).message);
      }
    }
    load(true);
    const timer = window.setInterval(() => void load(), POLL_MS);
    // 页面隐藏时停掉轮询：后台标签页没必要每 10 秒让服务器扫一遍磁盘。
    const onVisible = () => {
      if (document.hidden) {
        window.clearInterval(timer);
      } else {
        void load();
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive = false;
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  if (error) return <p className="hint">加载失败：{error}</p>;
  if (!res) return <p className="hint">加载中…</p>;

  const memUsed = res.mem_total > res.mem_available ? res.mem_total - res.mem_available : 0;

  return (
    <div className="res-wrap">
      <div className="res-grid">
        <div className="res-card">
          <h4>负载</h4>
          <p className="res-big">
            {res.load1.toFixed(2)}
            <small> / {res.load15.toFixed(2)}</small>
          </p>
          <p className="hint">1 分钟 / 15 分钟 · {res.cpu_cores} 核</p>
          <p className="hint">开机 {fmtDuration(res.uptime_secs)}</p>
        </div>
        <div className="res-card">
          <h4>内存</h4>
          <UsageBar used={memUsed} total={res.mem_total} />
          <p className="hint">serve 进程常驻 {fmtBytes(res.process_rss)}</p>
        </div>
        {res.disks.map((disk) => (
          <div className="res-card" key={disk.label}>
            <h4>{disk.label}</h4>
            <p className="res-path" title={disk.path}>
              {disk.path}
            </p>
            {disk.total_bytes > 0 ? (
              <UsageBar used={disk.used_bytes} total={disk.total_bytes} />
            ) : (
              <p className="hint">路径不可读</p>
            )}
          </div>
        ))}
        <div className="res-card">
          <h4>数据库</h4>
          <p className="res-big">{fmtBytes(res.db_bytes)}</p>
          <p className="hint">
            git_objects {res.git_objects} 行 · 原始 {fmtBytes(res.git_objects_bytes)}
          </p>
          <p className="hint">LFS 对象 {fmtBytes(res.lfs_bytes)}</p>
        </div>
      </div>

      <h3 className="res-section-title">仓库存储占用（{res.repos.length} 个仓库）</h3>
      {res.repos.length === 0 ? (
        <p className="hint">暂无仓库数据。</p>
      ) : (
        <div className="res-repos">
          {res.repos.map((repo) => (
            <RepoCard repo={repo} key={repo.id} />
          ))}
        </div>
      )}
    </div>
  );
}
