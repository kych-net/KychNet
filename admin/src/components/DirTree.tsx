import { useMemo, useState } from "react";
import { ContentSummary, editorPath, sourcePath } from "../api";

type FileNode = {
  name: string;
  content: ContentSummary;
};

type Node = {
  dirs: Record<string, Node>;
  files: FileNode[];
};

function buildTree(items: ContentSummary[]): Node {
  const root: Node = { dirs: {}, files: [] };
  for (const item of items) {
    const parts = item.slug.split("/");
    let node = root;
    for (const part of parts.slice(0, -1)) {
      node.dirs[part] ??= { dirs: {}, files: [] };
      node = node.dirs[part];
    }
    const path = sourcePath(item);
    const name = path.slice(path.lastIndexOf("/") + 1);
    node.files.push({ name, content: item });
  }
  return root;
}

/** 把命中的片段用 <mark> 包起来；query 为空时原样返回文本。 */
function highlight(text: string, query: string) {
  if (!query) return text;
  const lower = text.toLowerCase();
  const needle = query.toLowerCase();
  const parts: React.ReactNode[] = [];
  let from = 0;
  let at = lower.indexOf(needle, from);
  while (at !== -1) {
    if (at > from) parts.push(text.slice(from, at));
    parts.push(<mark key={at}>{text.slice(at, at + needle.length)}</mark>);
    from = at + needle.length;
    at = lower.indexOf(needle, from);
  }
  if (from < text.length) parts.push(text.slice(from));
  return parts;
}

/** 按完整源路径匹配：搜目录名（geography）也能命中其下所有文件。 */
function fileMatches(file: FileNode, query: string): boolean {
  return sourcePath(file.content).toLowerCase().includes(query);
}

/** 子树里是否有文件命中 query（用于决定是否保留/展开目录）。 */
function subtreeMatches(node: Node, query: string): boolean {
  if (!query) return false;
  if (node.files.some((file) => fileMatches(file, query))) return true;
  return Object.values(node.dirs).some((child) => subtreeMatches(child, query));
}

type DirProps = {
  name: string;
  node: Node;
  onEdit: (content: ContentSummary) => void;
  query: string;
  /** 搜索中：有命中的目录强制展开，忽略用户手动的折叠状态。 */
  searching: boolean;
};

function DirNode({ name, node, onEdit, query, searching }: DirProps) {
  const [open, setOpen] = useState(false);
  const matched = searching && subtreeMatches(node, query);
  const expanded = searching ? matched : open;
  return (
    <li className={`dir${matched ? " dir-match" : ""}`}>
      <button
        className="dir-toggle"
        type="button"
        aria-expanded={expanded}
        // 搜索中隐藏手动折叠按钮：此时展开状态由匹配结果决定，点了也不会变，
        // 免得给出「可折叠」的错误暗示。
        disabled={searching}
        onClick={() => setOpen((value) => !value)}
      >
        {highlight(name, query)}
      </button>
      {expanded && (
        <ul className="dir-children">
          {Object.keys(node.dirs)
            .sort()
            .filter((child) => !searching || subtreeMatches(node.dirs[child], query))
            .map((child) => (
              <DirNode
                key={child}
                name={child}
                node={node.dirs[child]}
                onEdit={onEdit}
                query={query}
                searching={searching}
              />
            ))}
          {node.files
            .slice()
            .sort((a, b) => a.name.localeCompare(b.name))
            .filter((file) => !searching || fileMatches(file, query))
            .map((file) => (
              <li className="file" key={file.content.id}>
                <button
                  className="dir-file"
                  type="button"
                  title={editorPath(file.content)}
                  onClick={() => onEdit(file.content)}
                >
                  {highlight(file.name, query)}
                </button>
              </li>
            ))}
        </ul>
      )}
    </li>
  );
}

type Props = {
  items: ContentSummary[];
  onEdit: (content: ContentSummary) => void;
};

export function DirTree({ items, onEdit }: Props) {
  const [rawQuery, setRawQuery] = useState("");
  const query = rawQuery.trim().toLowerCase();
  const searching = query.length > 0;
  const root = useMemo(() => buildTree(items), [items]);

  // 命中数量：与树的过滤口径一致（都按完整源路径匹配），避免计数与显示不符。
  const matchCount = useMemo(() => {
    if (!searching) return items.length;
    return items.filter((item) => sourcePath(item).toLowerCase().includes(query)).length;
  }, [items, query, searching]);

  if (items.length === 0) {
    return <p className="hint">暂无内容</p>;
  }

  return (
    <div className="dir-tree-wrap">
      <div className="dir-search">
        <span className="dir-search-icon" aria-hidden="true">⌕</span>
        <input
          type="search"
          value={rawQuery}
          onChange={(event) => setRawQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              setRawQuery("");
            }
          }}
          placeholder="搜索文件名或路径…"
          aria-label="搜索内容树"
        />
        {rawQuery && (
          <button
            type="button"
            className="dir-search-clear"
            aria-label="清空搜索"
            onClick={() => setRawQuery("")}
          >
            ×
          </button>
        )}
      </div>
      {searching && (
        <p className="dir-search-status" role="status">
          {matchCount > 0 ? `找到 ${matchCount} 项` : "没有匹配的内容"}
        </p>
      )}
      {searching && matchCount === 0 ? (
        <p className="hint dir-search-empty">换个关键词试试，例如目录名或 index.md。</p>
      ) : (
        <ul className="dir-tree">
          {Object.keys(root.dirs)
            .sort()
            .filter((name) => !searching || subtreeMatches(root.dirs[name], query))
            .map((name) => (
              <DirNode
                key={name}
                name={name}
                node={root.dirs[name]}
                onEdit={onEdit}
                query={query}
                searching={searching}
              />
            ))}
          {root.files
            .slice()
            .sort((a, b) => a.name.localeCompare(b.name))
            .filter((file) => !searching || fileMatches(file, query))
            .map((file) => (
              <li className="file" key={file.content.id}>
                <button
                  className="dir-file"
                  type="button"
                  title={editorPath(file.content)}
                  onClick={() => onEdit(file.content)}
                >
                  {highlight(file.name, query)}
                </button>
              </li>
            ))}
        </ul>
      )}
    </div>
  );
}
