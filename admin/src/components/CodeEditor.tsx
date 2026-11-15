import { useEffect, useRef } from "react";
import { basicSetup } from "codemirror";
import { Annotation, Compartment, EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import {
  HighlightStyle,
  StreamLanguage,
  syntaxHighlighting,
  type StreamParser,
} from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { indentWithTab } from "@codemirror/commands";
import { autocompletion, completionKeymap, type CompletionSource } from "@codemirror/autocomplete";
import type { ContentFormat } from "../api";
import { buildCompletionSource, type PageLink } from "./editorCompletions";

type Props = {
  value: string;
  format: ContentFormat;
  onChange: (value: string) => void;
  /** 暴露 EditorView 给外壳（格式工具栏直接 dispatch 命令）；卸载时回传 null。 */
  onViewReady?: (view: EditorView | null) => void;
  /** 正文字号（px）；移动端外壳传更大值提升可读性。 */
  fontSize?: number;
  /** 站内页面链接补全候选。 */
  pages?: PageLink[];
};

// 标记「由外部 setValue 引起的文档变更」。
// 这类变更必须同步进编辑器，但不能回头触发 onChange——否则打开文件
// 本身就会被当成一次编辑，把文件标成「待提交」。
const ExternalSync = Annotation.define<boolean>();

const highlightStyle = HighlightStyle.define([
  { tag: t.heading, color: "var(--accent)", fontWeight: "700" },
  { tag: t.strong, fontWeight: "700" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.monospace, fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace", background: "var(--bg)" },
  { tag: [t.link, t.url], color: "var(--accent)", textDecoration: "underline" },
  { tag: t.keyword, color: "#7c3aed" },
  { tag: t.string, color: "#0f766e" },
  { tag: t.number, color: "#b45309" },
  { tag: t.comment, color: "var(--muted)", fontStyle: "italic" },
  { tag: [t.bracket, t.brace], color: "var(--muted)" },
  { tag: [t.tagName, t.attributeName], color: "#b91c1c" },
  { tag: t.quote, color: "var(--muted)" },
  { tag: t.list, color: "var(--accent)" },
  { tag: t.invalid, color: "#dc2626" },
]);

const tokenTable = {
  heading: t.heading,
  strong: t.strong,
  emphasis: t.emphasis,
  monospace: t.monospace,
  link: t.link,
  url: t.url,
  keyword: t.keyword,
  string: t.string,
  number: t.number,
  comment: t.comment,
  bracket: t.bracket,
  brace: t.brace,
  quote: t.quote,
  list: t.list,
};

type EmptyState = Record<string, never>;

const wikidotParser: StreamParser<EmptyState> = {
  startState: () => ({}),
  token(stream) {
    if (stream.sol()) {
      if (stream.match(/^\+{1,6}\s/)) return "heading";
      if (stream.match(/^[*#]\s/)) return "list";
      if (stream.match(/^>\s?/)) return "quote";
    }
    if (stream.match(/^\[\[\/?[a-zA-Z0-9_-]+/)) return "keyword";
    if (stream.match(/^\[\[|\]\]/)) return "bracket";
    if (stream.match(/^\*\*|^__/)) return "strong";
    if (stream.match(/^\/\//)) return "emphasis";
    if (stream.match(/^--/)) return "comment";
    if (stream.match(/^\{\{/)) return "monospace";
    if (stream.match(/^\[https?:\/\/[^\]\s]+/)) return "link";
    if (stream.match(/^https?:\/\/\S+/)) return "url";
    if (stream.match(/^[{}]/)) return "brace";
    stream.next();
    return null;
  },
  tokenTable,
};

const typstParser: StreamParser<EmptyState> = {
  startState: () => ({}),
  token(stream) {
    if (stream.sol() && stream.match(/^=+\s/)) return "heading";
    if (stream.match(/^\/\/.*/)) return "comment";
    if (stream.match(/^"[^"]*"/)) return "string";
    if (stream.match(/^#[\w-]+/)) return "keyword";
    if (stream.match(/^\d+(?:\.\d+)?/)) return "number";
    if (stream.match(/^[\[\]{}()]/)) return "bracket";
    if (stream.match(/^\$[^$]*\$/)) return "monospace";
    stream.next();
    return null;
  },
  tokenTable,
};

// 语言包按需动态 import，单独分包，不进编辑器主 chunk。
// Wikidot / Typst 用的是本地 StreamLanguage，无需额外加载。
async function loadLanguage(format: ContentFormat): Promise<Extension> {
  switch (format) {
    case "markdown": {
      const { markdown } = await import("@codemirror/lang-markdown");
      return markdown();
    }
    case "html":
    case "svg": {
      const { html } = await import("@codemirror/lang-html");
      return html();
    }
    case "css": {
      const { css } = await import("@codemirror/lang-css");
      return css();
    }
    case "wikidot":
      return StreamLanguage.define(wikidotParser);
    case "typst":
      return StreamLanguage.define(typstParser);
  }
}

const editorTheme = EditorView.theme(
  {
    "&": {
      color: "var(--text)",
      backgroundColor: "var(--panel)",
      fontSize: "0.9rem",
    },
    ".cm-content": {
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
      padding: "0.75rem 0",
      minHeight: "20rem",
      caretColor: "var(--accent)",
    },
    ".cm-scroller": {
      maxHeight: "58vh",
      lineHeight: "1.65",
    },
    ".cm-gutters": {
      backgroundColor: "var(--bg)",
      color: "var(--muted)",
      border: "none",
      paddingRight: "0.3rem",
    },
    ".cm-activeLine": {
      backgroundColor: "color-mix(in srgb, var(--accent) 7%, transparent)",
    },
    ".cm-activeLineGutter": {
      backgroundColor: "color-mix(in srgb, var(--accent) 10%, transparent)",
      color: "var(--text)",
    },
    ".cm-selectionBackground, ::selection": {
      backgroundColor: "color-mix(in srgb, var(--accent) 22%, transparent)",
    },
    "&.cm-focused": {
      outline: "none",
    },
    ".cm-tooltip": {
      backgroundColor: "var(--panel)",
      border: "1px solid var(--border)",
      color: "var(--text)",
    },
    ".cm-panels": {
      backgroundColor: "var(--bg)",
      color: "var(--text)",
    },
  },
  { dark: false },
);

function fontTheme(fontSize: number) {
  return EditorView.theme({
    "&": { fontSize: `${fontSize}px` },
    ".cm-content": { fontSize: `${fontSize}px` },
  });
}

export function CodeEditor({
  value,
  format,
  onChange,
  onViewReady,
  fontSize = 14.4,
  pages = [],
}: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  const onViewReadyRef = useRef(onViewReady);
  const pagesRef = useRef(pages);
  const languageCompartment = useRef(new Compartment()).current;
  const fontCompartment = useRef(new Compartment()).current;
  // 语言包加载的代际守卫：快速切换格式时丢弃过期的加载结果。
  const languageGen = useRef(0);
  // 补全源在挂载时稳定注册（动态 reconfigure 的 languageData 不会被补全扩展拾取），
  // provider 每次查询时按当前格式即时构建源。
  const formatRef = useRef(format);
  formatRef.current = format;
  // 补全源函数必须保持身份稳定：CompletionState 按 `source == source` 复用 active
  // 源，languageData 每次查询都重建函数会让补全状态每个事务重置、异步结果无法绑定。
  // 外层包装只创建一次，格式与页面候选在查询时经 ref 即时读取。
  const completionSource = useRef<CompletionSource>((context) =>
    buildCompletionSource(formatRef.current, () => pagesRef.current)(context),
  ).current;

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);
  useEffect(() => {
    onViewReadyRef.current = onViewReady;
  }, [onViewReady]);
  useEffect(() => {
    pagesRef.current = pages;
  }, [pages]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const state = EditorState.create({
      doc: value,
      extensions: [
        basicSetup,
        keymap.of([...completionKeymap, indentWithTab]),
        // 语言包异步加载，加载完成前先以纯文本可用。
        languageCompartment.of([]),
        EditorState.languageData.of(() => [{ autocomplete: completionSource }]),
        fontCompartment.of(fontTheme(fontSize)),
        autocompletion({ activateOnTyping: true, maxRenderedOptions: 40 }),
        syntaxHighlighting(highlightStyle),
        editorTheme,
        EditorView.lineWrapping,
        // 移动端代码编辑：关闭自动大写/纠错/拼写检查，避免系统改写代码。
        EditorView.contentAttributes.of({
          autocapitalize: "off",
          autocomplete: "off",
          autocorrect: "off",
          spellcheck: "false",
        }),
        EditorView.updateListener.of((update) => {
          if (!update.docChanged) return;
          // 外部同步进来的内容不算用户编辑。
          const external = update.transactions.some((tr) =>
            tr.annotation(ExternalSync),
          );
          if (external) return;
          onChangeRef.current(update.state.doc.toString());
        }),
      ],
    });

    const view = new EditorView({ state, parent: host });
    viewRef.current = view;
    onViewReadyRef.current?.(view);
    return () => {
      onViewReadyRef.current?.(null);
      view.destroy();
      viewRef.current = null;
    };
    // 编辑器只创建一次；value / format / fontSize 通过下面的 effect 更新。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const current = view.state.doc.toString();
    if (current !== value) {
      view.dispatch({
        changes: { from: 0, to: current.length, insert: value },
        annotations: ExternalSync.of(true),
      });
    }
  }, [value]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const gen = ++languageGen.current;
    // 先切回纯文本，避免上一格式的高亮残留；语言包加载完成后再启用。
    // 补全源已稳定注册，按 formatRef 即时取当前格式，无需在此 reconfigure。
    view.dispatch({ effects: languageCompartment.reconfigure([]) });
    void loadLanguage(format)
      .then((language) => {
        // 格式已再次切换或编辑器已销毁时，丢弃这次加载结果。
        if (gen !== languageGen.current || viewRef.current !== view) return;
        view.dispatch({
          effects: languageCompartment.reconfigure(language),
        });
      })
      .catch(() => {
        // 语言包加载失败（如离线）时保持纯文本，编辑不受影响。
      });
  }, [format, languageCompartment]);

  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: fontCompartment.reconfigure(fontTheme(fontSize)) });
  }, [fontSize, fontCompartment]);

  return <div ref={hostRef} className="code-editor" />;
}
