import { useEffect, useState } from "react";
import { api, BuildStatus, NavLink } from "../api";

function formatTime(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}

export function SettingsView() {
  const [siteName, setSiteName] = useState("");
  const [logoUrl, setLogoUrl] = useState("");
  const [nav, setNav] = useState<NavLink[]>([]);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [build, setBuild] = useState<BuildStatus | null>(null);
  const [buildError, setBuildError] = useState("");

  useEffect(() => {
    api
      .getSettings()
      .then((result) => {
        setSiteName(result.site.site_name);
        setLogoUrl(result.site.logo_url);
        setNav(result.site.nav);
      })
      .catch((e) => setError((e as Error).message));
    api.buildStatus().then(setBuild).catch(() => {
      // 构建状态只是辅助信息，加载失败不挡设置页
    });
  }, []);

  // 构建期间每 2 秒轮询一次状态，结束后再拉一次拿到最终结果。
  useEffect(() => {
    if (!build?.running) return;
    const timer = setInterval(() => {
      api
        .buildStatus()
        .then(setBuild)
        .catch((e) => setBuildError((e as Error).message));
    }, 2000);
    return () => clearInterval(timer);
  }, [build?.running]);

  async function triggerRebuild() {
    setBuildError("");
    try {
      setBuild(await api.rebuild());
    } catch (e) {
      setBuildError((e as Error).message);
    }
  }

  function updateLink(index: number, field: keyof NavLink, value: string) {
    setNav((links) => links.map((link, i) => (i === index ? { ...link, [field]: value } : link)));
  }

  async function save(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    try {
      await api.saveSettings({ site_name: siteName, logo_url: logoUrl, nav });
      setNotice("站点设置已保存,重新构建后生效");
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <section>
      <h2>站点设置</h2>
      {error && <p className="error">{error}</p>}
      {notice && <p className="notice">{notice}</p>}

      <form onSubmit={save}>
        <div className="row">
          <label>
            站点名称
            <input value={siteName} onChange={(e) => setSiteName(e.target.value)} required />
          </label>
          <label>
            图标 URL
            <input
              value={logoUrl}
              onChange={(e) => setLogoUrl(e.target.value)}
              placeholder="留空则显示站点名首字"
            />
          </label>
        </div>

        <h3>顶栏链接</h3>
        {nav.map((link, index) => (
          <div className="row align" key={index}>
            <label>
              标签
              <input value={link.label} onChange={(e) => updateLink(index, "label", e.target.value)} />
            </label>
            <label>
              地址
              <input value={link.href} onChange={(e) => updateLink(index, "href", e.target.value)} />
            </label>
            <button type="button" onClick={() => setNav(nav.filter((_, i) => i !== index))}>
              删除
            </button>
          </div>
        ))}
        <button type="button" onClick={() => setNav([...nav, { label: "", href: "" }])}>
          新增链接
        </button>
        <div className="row align">
          <button type="submit">保存</button>
        </div>
      </form>

      <h3>整站重建</h3>
      {buildError && <p className="error">{buildError}</p>}
      <div className="row align">
        <button type="button" onClick={triggerRebuild} disabled={!!build?.running}>
          {build?.running ? "构建中…" : "重建站点"}
        </button>
        {build?.running && <span>正在生成静态页面，完成后本页自动刷新状态。</span>}
      </div>
      {build?.last && !build.running && (
        <p>
          上次构建：
          {build.last.exit_code === null
            ? "启动失败"
            : build.last.exit_code === 0
              ? "成功"
              : `失败（退出码 ${build.last.exit_code}）`}
          {build.last.finished_at !== null && <>，{formatTime(build.last.finished_at)} 结束</>}
        </p>
      )}
      {build?.last?.exit_code != null && build.last.exit_code !== 0 && (
        <pre className="build-log">{build.last.tail || "（无输出）"}</pre>
      )}
    </section>
  );
}
