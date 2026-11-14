import { useEffect, useState } from "react";
import { api, SystemResources } from "../api";

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

/**
 * 资源监控：负载 / 内存 / 磁盘 / 数据库存储，10 秒自动刷新。
 * 指标全部现读，不做历史曲线——要长期趋势应该交给系统级监控。
 */
export function ResourcesView() {
  const [res, setRes] = useState<SystemResources | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let alive = true;
    async function load() {
      try {
        const result = await api.systemResources();
        if (alive) {
          setRes(result);
          setError("");
        }
      } catch (e) {
        if (alive) setError((e as Error).message);
      }
    }
    load();
    const timer = window.setInterval(load, POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  if (error) return <p className="hint">加载失败：{error}</p>;
  if (!res) return <p className="hint">加载中…</p>;

  const memUsed = res.mem_total > res.mem_available ? res.mem_total - res.mem_available : 0;

  return (
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
  );
}
