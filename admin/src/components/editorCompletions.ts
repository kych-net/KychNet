import {
  type Completion,
  type CompletionContext,
  type CompletionSource,
  snippetCompletion,
} from "@codemirror/autocomplete";
import type { ContentFormat } from "../api";

export type PageLink = { label: string; path: string };

// 站内页面链接补全：在 Markdown 的 ]( 、Wikidot 的 [[[ 、HTML 的 href/src、
// CSS 的 url() 等位置触发，提供站内已发布页面的路径。
function linkCompletions(pages: PageLink[]): Completion[] {
  return pages.map((page) => ({
    label: page.path,
    detail: page.label,
    type: "text",
    apply: page.path,
  }));
}

const markdownSnippets: Completion[] = [
  snippetCompletion("# ${标题}", { label: "# 一级标题", type: "text" }),
  snippetCompletion("## ${标题}", { label: "## 二级标题", type: "text" }),
  snippetCompletion("### ${标题}", { label: "### 三级标题", type: "text" }),
  snippetCompletion("**${加粗}**", { label: "**加粗**", type: "text" }),
  snippetCompletion("*${斜体}*", { label: "*斜体*", type: "text" }),
  snippetCompletion("[${文字}](${链接})", { label: "[链接]", type: "text" }),
  snippetCompletion("![${描述}](${图片路径})", { label: "![图片]", type: "text" }),
  snippetCompletion("- ${项}", { label: "- 无序列表", type: "text" }),
  snippetCompletion("1. ${项}", { label: "1. 有序列表", type: "text" }),
  snippetCompletion("> ${引用}", { label: "> 引用", type: "text" }),
  snippetCompletion("```${lang}\n${代码}\n```", { label: "``` 代码块", type: "text" }),
  snippetCompletion("| ${列1} | ${列2} |\n| --- | --- |\n| ${值1} | ${值2} |", { label: "表格", type: "text" }),
  snippetCompletion("---\ncss: /css/${theme}.css\n---\n", { label: "front-matter(css)", type: "text" }),
];

const htmlSnippets: Completion[] = [
  snippetCompletion("<h1>${标题}</h1>", { label: "<h1>", type: "text" }),
  snippetCompletion("<p>${段落}</p>", { label: "<p>", type: "text" }),
  snippetCompletion('<a href="${链接}">${文字}</a>', { label: "<a>", type: "text" }),
  snippetCompletion('<img src="${路径}" alt="${描述}">', { label: "<img>", type: "text" }),
  snippetCompletion("<ul>\n  <li>${项}</li>\n</ul>", { label: "<ul>", type: "text" }),
  snippetCompletion('<div class="${类}">${内容}</div>', { label: "<div>", type: "text" }),
  snippetCompletion("<pre><code>${代码}</code></pre>", { label: "<pre><code>", type: "text" }),
  snippetCompletion("<blockquote>${引用}</blockquote>", { label: "<blockquote>", type: "text" }),
  snippetCompletion('<table>\n  <tr><th>${列}</th></tr>\n  <tr><td>${值}</td></tr>\n</table>', { label: "<table>", type: "text" }),
];

const cssSnippets: Completion[] = [
  snippetCompletion("${选择器} {\n  ${属性}: ${值};\n}", { label: "规则块", type: "text" }),
  snippetCompletion("@media (max-width: ${768}px) {\n  ${规则}\n}", { label: "@media", type: "text" }),
  snippetCompletion(":root {\n  --${变量}: ${值};\n}", { label: ":root 变量", type: "text" }),
  snippetCompletion("display: flex;\nalign-items: ${center};\njustify-content: ${center};", { label: "flex 布局", type: "text" }),
];

const typstKeywords: Completion[] = [
  { label: "#figure", type: "function", detail: "浮动图表" },
  { label: "#table", type: "function", detail: "表格" },
  { label: "#image", type: "function", detail: "插入图片" },
  { label: "#link", type: "function", detail: "超链接" },
  { label: "#cite", type: "function", detail: "引用" },
  { label: "#footnote", type: "function", detail: "脚注" },
  { label: "#outline", type: "function", detail: "目录" },
  { label: "#heading", type: "function", detail: "标题" },
  { label: "#list", type: "function", detail: "列表" },
  { label: "#enum", type: "function", detail: "有序列表" },
  { label: "#grid", type: "function", detail: "网格" },
  { label: "#box", type: "function", detail: "盒" },
  { label: "#block", type: "function", detail: "块" },
  { label: "#align", type: "function", detail: "对齐" },
  { label: "#text", type: "function", detail: "文本样式" },
  { label: "#strong", type: "function", detail: "加粗" },
  { label: "#emph", type: "function", detail: "强调" },
  { label: "#raw", type: "function", detail: "原始代码" },
  { label: "#set", type: "function", detail: "设置规则" },
  { label: "#show", type: "function", detail: "显示规则" },
  { label: "#let", type: "function", detail: "定义变量" },
  { label: "#import", type: "function", detail: "导入" },
];

const typstSnippets: Completion[] = [
  snippetCompletion("= ${标题}", { label: "= 一级标题", type: "text" }),
  snippetCompletion("== ${标题}", { label: "== 二级标题", type: "text" }),
  snippetCompletion('#figure(\n  ${内容},\n  caption: [${说明}],\n)', { label: "#figure 模板", type: "text" }),
  snippetCompletion('#table(\n  columns: ${2},\n  [${列1}], [${列2}],\n)', { label: "#table 模板", type: "text" }),
  snippetCompletion('#image("${路径}", width: ${80}%)', { label: "#image 模板", type: "text" }),
  snippetCompletion('$ ${公式} $', { label: "$ 行内公式", type: "text" }),
  snippetCompletion('$ ${公式} $\n', { label: "$$ 块公式", type: "text" }),
];

const wikidotKeywords: Completion[] = [
  { label: "[[include]]", type: "keyword", detail: "包含其它页面" },
  { label: "[[div]]", type: "keyword", detail: "容器块" },
  { label: "[[code]]", type: "keyword", detail: "代码块" },
  { label: "[[table]]", type: "keyword", detail: "表格" },
  { label: "[[row]]", type: "keyword", detail: "表格行" },
  { label: "[[cell]]", type: "keyword", detail: "表格单元" },
  { label: "[[collapsible]]", type: "keyword", detail: "可折叠块" },
  { label: "[[footnote]]", type: "keyword", detail: "脚注" },
  { label: "[[footnoteblock]]", type: "keyword", detail: "脚注列表" },
  { label: "[[image]]", type: "keyword", detail: "图片" },
  { label: "[[iframe]]", type: "keyword", detail: "内嵌框架" },
  { label: "[[note]]", type: "keyword", detail: "提示块" },
];

const wikidotSnippets: Completion[] = [
  snippetCompletion("+ ${标题}", { label: "+ 一级标题", type: "text" }),
  snippetCompletion("++ ${标题}", { label: "++ 二级标题", type: "text" }),
  snippetCompletion("**${加粗}**", { label: "**加粗**", type: "text" }),
  snippetCompletion("//${斜体}//", { label: "//斜体//", type: "text" }),
  snippetCompletion("[[[${页面}]]]", { label: "[[[页面链接]]]", type: "text" }),
  snippetCompletion("[[include ${页面}]]", { label: "[[include]] 模板", type: "text" }),
  snippetCompletion('[[div class="${类}"]]\n${内容}\n[[/div]]', { label: "[[div]] 模板", type: "text" }),
  snippetCompletion('[[code type="${lang}"]]\n${代码}\n[[/code]]', { label: "[[code]] 模板", type: "text" }),
  snippetCompletion("[[note]]\n${提示}\n[[/note]]", { label: "[[note]] 模板", type: "text" }),
];

function snippetsFor(format: ContentFormat): Completion[] {
  switch (format) {
    case "markdown": return markdownSnippets;
    case "html":
    case "svg": return htmlSnippets;
    case "css": return cssSnippets;
    case "typst": return [...typstKeywords, ...typstSnippets];
    case "wikidot": return [...wikidotKeywords, ...wikidotSnippets];
  }
}

// 判断光标是否处于「链接上下文」，返回触发前缀长度以便替换。
function linkContext(context: CompletionContext): { from: number } | null {
  const { state, pos } = context;
  const line = state.doc.lineAt(pos);
  const before = line.text.slice(0, pos - line.from);
  // Markdown ]( 、Wikidot [[[ / [[include 、HTML href=/src= 、CSS url(
  const triggers = ["](", "[[[", "[[include ", 'href="', 'src="', "url("];
  for (const trigger of triggers) {
    const idx = before.lastIndexOf(trigger);
    if (idx >= 0) {
      const typed = before.slice(idx + trigger.length);
      // 触发符之后到光标之间不能有空格或闭合符，否则不算链接输入。
      if (!/[\s"')\]]/.test(typed)) {
        return { from: pos - typed.length };
      }
    }
  }
  return null;
}

/// 构造一个与语言内置补全合并的自定义补全源（经 languageData.autocomplete 注入）。
/// pages 提供站内链接候选；getPages 惰性取值，避免编辑器创建时强依赖网络。
export function buildCompletionSource(
  format: ContentFormat,
  getPages: () => PageLink[],
): CompletionSource {
  const snippets = snippetsFor(format);
  return (context: CompletionContext) => {
    const link = linkContext(context);
    if (link) {
      const pages = getPages();
      if (pages.length === 0) return null;
      const options = linkCompletions(pages);
      return { from: link.from, options, validFor: /^[\w\-/:.]*$/ };
    }
    const word = context.matchBefore(/[\w#*+/[\-]+/);
    if (!word && !context.explicit) return null;
    const from = word ? word.from : context.pos;
    return { from, options: snippets, validFor: /^[\w#*+/[\-]*$/ };
  };
}
