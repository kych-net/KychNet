import { forwardRef, useEffect, useState } from "react";
import { marked } from "marked";
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
  math { font-size: 1.06em; }
  math[display="block"] { display: block; margin: 1.3em 0; overflow-x: auto; text-align: center; }
  .wiki-note { padding: .75rem 1rem; border-left: 4px solid #2f7d5c; background: #eef7f1; border-radius: 6px; }
  .wj-code { display: block; margin: 1rem 0; border: 1px solid #ddd; border-radius: 6px; overflow: hidden; }
  .wj-code pre { margin: 0; border: 0; border-radius: 0; }
</style></head><body><article>${html}</article></body></html>`;
}

type PreviewState = "loading" | "ready" | "error";

/// wikidot 依赖 ftml、typst 依赖 Typst 编译器，只能在服务端渲染；
/// 其余格式直接在浏览器里渲染，输入到预览不需要网络往返。
const SERVER_FORMATS: ContentFormat[] = ["wikidot", "typst"];

async function renderInBrowser(format: ContentFormat, body: string): Promise<string> {
  if (format !== "markdown") {
    // html / svg 直接原样嵌入预览文档
    return body;
  }
  const parse = (source: string) =>
    marked.parse(source, { async: false, gfm: true, breaks: false }) as string;
  // 只有出现公式标记时才加载 Temml（约 200 KB，独立 chunk）。
  if (!/\$|\\\(|\\\[/.test(body)) return parse(body);
  // 与服务端一致：先把 LaTeX 转成 MathML，再交给 marked。
  const { embedLatexMath } = await import("@admin/components/markdownMath");
  return parse(embedLatexMath(body));
}

/**
 * 内容预览。浏览器端格式（markdown/html/svg）本地渲染，wikidot/typst 仍走服务端。
 *
 * 暴露 iframe 的 ref：外壳按编辑器滚动比例同步预览滚动（需要 sandbox 允许同源，
 * 但依然不执行 iframe 内的脚本）。
 */
export const Preview = forwardRef<
  HTMLIFrameElement,
  { format: ContentFormat; body: string; active: boolean }
>(function Preview({ format, body, active }, ref) {
  const [html, setHtml] = useState("");
  const [state, setState] = useState<PreviewState>("loading");

  useEffect(() => {
    if (!active) return;
    let cancelled = false;

    if (!SERVER_FORMATS.includes(format)) {
      // Markdown 逐字符解析会拖慢输入：延迟一小段时间再渲染，
      // 期间用户继续打字就取消上一次。html/svg 没有必要等待。
      const delay = format === "markdown" ? 120 : 0;
      const timer = window.setTimeout(() => {
        void renderInBrowser(format, body)
          .then((next) => {
            if (!cancelled) {
              setHtml(next);
              setState("ready");
            }
          })
          .catch(() => {
            if (!cancelled) setState("error");
          });
      }, delay);
      return () => {
        cancelled = true;
        window.clearTimeout(timer);
      };
    }

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
        ref={ref}
        className="preview-frame"
        title="内容预览"
        sandbox="allow-same-origin"
        srcDoc={previewDocument(html)}
      />
    </div>
  );
});
