import { EditorView } from "@codemirror/view";
import { undo, redo } from "@codemirror/commands";
import type { ContentFormat } from "@admin/api";

export type FormatCommand = {
  id: string;
  label: string;
  title: string;
  run: (view: EditorView) => void;
};

// 包裹选区；无选区时插入占位文本并选中它，方便直接覆盖输入。
function wrap(view: EditorView, before: string, after: string, placeholder: string) {
  const { state } = view;
  const range = state.selection.main;
  const selected = state.sliceDoc(range.from, range.to);
  const text = selected || placeholder;
  view.dispatch({
    changes: { from: range.from, to: range.to, insert: before + text + after },
    selection: {
      anchor: range.from + before.length,
      head: range.from + before.length + text.length,
    },
    scrollIntoView: true,
  });
  view.focus();
}

// 在光标所在行首切换前缀（列表/引用）。已存在则移除。
function toggleLinePrefix(view: EditorView, prefix: string) {
  const { state } = view;
  const range = state.selection.main;
  const line = state.doc.lineAt(range.from);
  const has = line.text.startsWith(prefix);
  const changes = has
    ? { from: line.from, to: line.from + prefix.length, insert: "" }
    : { from: line.from, to: line.from, insert: prefix };
  const delta = has ? -prefix.length : prefix.length;
  view.dispatch({
    changes,
    selection: { anchor: Math.max(line.from, range.from + delta) },
    scrollIntoView: true,
  });
  view.focus();
}

// 标题循环：markdown/typst 用 #，wikidot 用 +；1→2→3→取消。
function cycleHeading(view: EditorView, marker: string) {
  const { state } = view;
  const range = state.selection.main;
  const line = state.doc.lineAt(range.from);
  const match = line.text.match(new RegExp(`^(${marker}{1,6})\\s`));
  const level = match ? match[1].length : 0;
  const next = level >= 3 ? 0 : level + 1;
  const oldPrefix = match ? match[0] : "";
  const newPrefix = next === 0 ? "" : marker.repeat(next) + " ";
  view.dispatch({
    changes: { from: line.from, to: line.from + oldPrefix.length, insert: newPrefix },
    selection: { anchor: line.from + newPrefix.length + Math.max(0, range.from - line.from - oldPrefix.length) },
    scrollIntoView: true,
  });
  view.focus();
}

function insertText(view: EditorView, text: string) {
  const range = view.state.selection.main;
  view.dispatch({
    changes: { from: range.from, to: range.to, insert: text },
    selection: { anchor: range.from + text.length },
    scrollIntoView: true,
  });
  view.focus();
}

function boldTokens(format: ContentFormat): [string, string] {
  switch (format) {
    case "html":
    case "svg": return ["<strong>", "</strong>"];
    case "typst": return ["#strong[", "]"];
    case "css": return ["/* ", " */"];
    default: return ["**", "**"]; // markdown / wikidot
  }
}

function italicTokens(format: ContentFormat): [string, string] {
  switch (format) {
    case "html":
    case "svg": return ["<em>", "</em>"];
    case "typst": return ["#emph[", "]"];
    case "wikidot": return ["//", "//"];
    case "css": return ["/* ", " */"];
    default: return ["*", "*"];
  }
}

function codeTokens(format: ContentFormat): [string, string] {
  switch (format) {
    case "html":
    case "svg": return ["<code>", "</code>"];
    case "typst": return ["#raw(\"", "\")"];
    case "wikidot": return ["{{", "}}"];
    default: return ["`", "`"];
  }
}

export function getCommands(format: ContentFormat): FormatCommand[] {
  const [bBefore, bAfter] = boldTokens(format);
  const [iBefore, iAfter] = italicTokens(format);
  const [cBefore, cAfter] = codeTokens(format);
  const isMarkup = format !== "css";
  const headingMarker = format === "wikidot" ? "+" : "#";

  const commands: FormatCommand[] = [];

  if (isMarkup) {
    commands.push(
      { id: "heading", label: "H", title: "标题循环（H1→H2→H3→正文）", run: (v) => cycleHeading(v, headingMarker) },
      { id: "bold", label: "B", title: "加粗", run: (v) => wrap(v, bBefore, bAfter, "加粗文字") },
      { id: "italic", label: "I", title: "斜体", run: (v) => wrap(v, iBefore, iAfter, "斜体文字") },
    );
  }
  commands.push(
    { id: "code", label: "</>", title: "代码", run: (v) => wrap(v, cBefore, cAfter, "code") },
  );
  if (isMarkup) {
    commands.push(
      { id: "quote", label: "❝", title: "引用", run: (v) => toggleLinePrefix(v, format === "wikidot" ? "> " : "> ") },
      { id: "ul", label: "•≡", title: "无序列表", run: (v) => toggleLinePrefix(v, format === "wikidot" ? "* " : "- ") },
      { id: "ol", label: "1≡", title: "有序列表", run: (v) => toggleLinePrefix(v, format === "wikidot" ? "# " : "1. ") },
      {
        id: "link", label: "🔗", title: "插入链接",
        run: (v) => {
          if (format === "html" || format === "svg") wrap(v, '<a href="', '">链接</a>', "https://");
          else if (format === "wikidot") wrap(v, "[[[", "]]]", "页面名");
          else if (format === "typst") wrap(v, '#link("', '")[链接]', "https://");
          else wrap(v, "[", "](https://)", "链接文字");
        },
      },
      {
        id: "image", label: "🖼", title: "插入图片",
        run: (v) => {
          if (format === "html" || format === "svg") insertText(v, '<img src="" alt="描述">');
          else if (format === "typst") insertText(v, '#image("路径", width: 80%)');
          else if (format === "wikidot") insertText(v, "[[image 路径]]");
          else insertText(v, "![描述](路径)");
        },
      },
    );
  }
  commands.push(
    { id: "undo", label: "↶", title: "撤销", run: (v) => { undo(v); v.focus(); } },
    { id: "redo", label: "↷", title: "重做", run: (v) => { redo(v); v.focus(); } },
  );
  return commands;
}
