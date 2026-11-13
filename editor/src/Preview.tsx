import { useEffect, useState } from "react";
import { api, type ContentFormat } from "@admin/api";

// 预览正文样式：贴近内容站实际渲染，独立于编辑器深色外壳，白底黑字。
function previewDocument(html: string): string {
  return `<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
  :root { color-scheme: light; }
  * { box-sizing: border-box; }
  body { margin: 0; padding: 1.1rem 1.2rem 3rem; background: #fff; color: #1f2328;
    font-family: "LXGW WenKai Screen", -apple-system, "PingFang SC", "Microsoft YaHei", sans-serif;
    line-height: 1.8; font-size: 16px; overflow-wrap: anywhere; }
  h1,h2,h3,h4 { line-height: 1.35; margin: 1.4em 0 .6em; }
  h1 { font-size: 1.7em; border-bottom: 1px solid #eee; padding-bottom: .3em; }
  h2 { font-size: 1.4em; } h3 { font-size: 1.15em; }
  p { margin: .8em 0; }
  a { color: #2f6feb; }
  img { max-width: 100%; height: auto; border-radius: 6px; }
  table { width: 100%; border-collapse: collapse; margin: 1em 0; }
  th, td { border: 1px solid #ddd; padding: .4rem .55rem; text-align: left; }
  pre { overflow-x: auto; padding: .85rem; background: #f6f8fa; border: 1px solid #e1e4e8; border-radius: 8px; }
  code { font-family: ui-monospace, Menlo, Consolas, monospace; font-size: .9em; }
  :not(pre) > code { background: #f0f1f3; padding: .12em .35em; border-radius: 4px; }
  blockquote { margin: 1em 0; padding: .2em 1em; border-left: 3px solid #d0d7de; color: #57606a; }
  hr { border: none; border-top: 1px solid #eee; margin: 1.6em 0; }
  .wiki-note { padding: .75rem 1rem; border-left: 4px solid #2f7d5c; background: #eef7f1; border-radius: 6px; }
  .wj-code { display: block; margin: 1rem 0; border: 1px solid #ddd; border-radius: 6px; overflow: hidden; }
  .wj-code pre { margin: 0; border: 0; border-radius: 0; }
</style></head><body><article>${html}</article></body></html>`;
}

type PreviewState = "loading" | "ready" | "error";

// 服务端渲染预览（typst/wikidot 必须服务端渲染），防抖 500ms，
// 放进无脚本 sandbox iframe，与编辑器隔离。
export function Preview({
  format,
  body,
  active,
}: {
  format: ContentFormat;
  body: string;
  active: boolean;
}) {
  const [html, setHtml] = useState("");
  const [state, setState] = useState<PreviewState>("loading");

  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setState("loading");
    const timer = window.setTimeout(async () => {
      try {
        const res = await api.previewContent(format, body);
        if (!cancelled) {
          setHtml(res.html);
          setState("ready");
        }
      } catch {
        if (!cancelled) setState("error");
      }
    }, 500);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [format, body, active]);

  return (
    <div className="preview-pane">
      <div className="preview-status" data-state={state}>
        {state === "loading" && "预览渲染中…"}
        {state === "error" && "预览失败，保存后整站重建可查看"}
        {state === "ready" && "实时预览"}
      </div>
      <iframe
        className="preview-frame"
        title="内容预览"
        sandbox=""
        srcDoc={previewDocument(html)}
      />
    </div>
  );
}
