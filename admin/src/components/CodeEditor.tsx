import { useEffect, useRef } from "react";
import { basicSetup } from "codemirror";
import { Annotation, Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import {
  HighlightStyle,
  StreamLanguage,
  syntaxHighlighting,
  type StreamParser,
} from "@codemirror/language";
import { tags as t } from "@lezer/highlight";
import { indentWithTab } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import type { ContentFormat } from "../api";

type Props = {
  value: string;
  format: ContentFormat;
  onChange: (value: string) => void;
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

function languageFor(format: ContentFormat) {
  switch (format) {
    case "markdown":
      return markdown();
    case "html":
      return html();
    case "css":
      return css();
    case "svg":
      return html();
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

export function CodeEditor({ value, format, onChange }: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const onChangeRef = useRef(onChange);
  const languageCompartment = useRef(new Compartment()).current;

  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const state = EditorState.create({
      doc: value,
      extensions: [
        basicSetup,
        keymap.of([indentWithTab]),
        languageCompartment.of(languageFor(format)),
        syntaxHighlighting(highlightStyle),
        editorTheme,
        EditorView.lineWrapping,
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
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // 编辑器只创建一次；value / format 通过下面的 effect 更新。
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
    view.dispatch({
      effects: languageCompartment.reconfigure(languageFor(format)),
    });
  }, [format, languageCompartment]);

  return <div ref={hostRef} className="code-editor" />;
}
