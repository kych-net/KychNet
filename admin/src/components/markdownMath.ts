import temml from "temml";

/** 找一个行内公式的闭合位置：不跨行，且闭合美元符前不能是空白。 */
function findInlineEnd(source: string, start: number): number {
  let i = start;
  while (i < source.length) {
    const c = source[i];
    if (c === "\\") {
      i += 2;
      continue;
    }
    if (c === "\n") return -1;
    if (c === "$") {
      const closed = i > start && !/\s/.test(source[i - 1]);
      return closed ? i : -1;
    }
    i += 1;
  }
  return -1;
}

/** 找一个块级公式的闭合 `$$`（允许跨行）。 */
function findBlockEnd(source: string, start: number): number {
  let i = start;
  while (i < source.length - 1) {
    if (source[i] === "\\") {
      i += 2;
      continue;
    }
    if (source[i] === "$" && source[i + 1] === "$") {
      return i > start ? i : -1;
    }
    i += 1;
  }
  return -1;
}

/**
 * 把 Markdown 里的 LaTeX 公式转成 MathML，输出仍可交给 marked 解析。
 *
 * 判定规则与服务端 `src/render.rs` 的 `embed_latex_math` 保持一致：
 * - 行内公式：`$…$` 或 `\(…\)`，不跨行且首尾非空白
 * - 块级公式：`$$…$$` 或 `\[…\]`
 * - `\$` 视为转义，价格之类的孤立美元符号不会被误判
 * - 单个公式解析失败只保留原文，不影响其余内容
 *
 * 用 MathML 而不是 KaTeX：浏览器原生渲染，预览 iframe 不需要额外脚本与字体，
 * 与线上页面（服务端 latex2mathml）的渲染路径一致。
 */
export function embedLatexMath(source: string): string {
  const out: string[] = [];
  let i = 0;

  const render = (tex: string, block: boolean): string | null => {
    try {
      return temml.renderToString(tex.trim(), { displayMode: block, throwOnError: true });
    } catch {
      return null;
    }
  };

  while (i < source.length) {
    const c = source[i];

    if (c === "\\") {
      const next = source[i + 1];
      if (next === "(" || next === "[") {
        const close = next === "(" ? ")" : "]";
        const end = source.indexOf(`\\${close}`, i + 2);
        if (end >= 0) {
          const mathml = render(source.slice(i + 2, end), next === "[");
          if (mathml) {
            out.push(next === "[" ? `\n\n${mathml}\n\n` : mathml);
            i = end + 2;
            continue;
          }
        }
      }
      out.push(source.slice(i, i + 2));
      i += 2;
      continue;
    }

    if (c === "$") {
      const block = source[i + 1] === "$";
      const open = block ? 2 : 1;
      const end = block ? findBlockEnd(source, i + open) : findInlineEnd(source, i + open);
      if (end >= 0) {
        const mathml = render(source.slice(i + open, end), block);
        if (mathml) {
          out.push(block ? `\n\n${mathml}\n\n` : mathml);
          i = end + open;
          continue;
        }
      }
      out.push(c);
      i += 1;
      continue;
    }

    out.push(c);
    i += 1;
  }

  return out.join("");
}
