import { useState } from "react";
import { Content, sourcePath } from "../api";

type FileNode = {
  name: string;
  content: Content;
};

type Node = {
  dirs: Record<string, Node>;
  files: FileNode[];
};

function buildTree(items: Content[]): Node {
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

type DirProps = {
  name: string;
  node: Node;
  onEdit: (content: Content) => void;
};

function DirNode({ name, node, onEdit }: DirProps) {
  const [open, setOpen] = useState(false);
  return (
    <li className="dir">
      <button
        className="dir-toggle"
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        {name}
      </button>
      {open && (
        <ul className="dir-children">
          {Object.keys(node.dirs)
            .sort()
            .map((child) => (
              <DirNode key={child} name={child} node={node.dirs[child]} onEdit={onEdit} />
            ))}
          {node.files
            .slice()
            .sort((a, b) => a.name.localeCompare(b.name))
            .map((file) => (
              <li className="file" key={file.content.id}>
                <button
                  className="dir-file"
                  type="button"
                  onClick={() => onEdit(file.content)}
                >
                  {file.name}
                </button>
              </li>
            ))}
        </ul>
      )}
    </li>
  );
}

type Props = {
  items: Content[];
  onEdit: (content: Content) => void;
};

export function DirTree({ items, onEdit }: Props) {
  if (items.length === 0) {
    return <p className="hint">暂无内容</p>;
  }
  const root = buildTree(items);
  return (
    <ul className="dir-tree">
      {Object.keys(root.dirs)
        .sort()
        .map((name) => (
          <DirNode key={name} name={name} node={root.dirs[name]} onEdit={onEdit} />
        ))}
      {root.files
        .slice()
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((file) => (
          <li className="file" key={file.content.id}>
            <button
              className="dir-file"
              type="button"
              onClick={() => onEdit(file.content)}
            >
              {file.name}
            </button>
          </li>
        ))}
    </ul>
  );
}
