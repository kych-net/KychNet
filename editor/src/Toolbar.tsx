import type { EditorView } from "@codemirror/view";
import type { ContentFormat } from "@admin/api";
import { getCommands } from "./formatCommands";

// 底部格式工具栏：触控目标 ≥44px，横向可滚。onPointerDown 阻止默认，
// 避免点按工具栏时编辑器选区塌陷（移动端关键细节）。
export function Toolbar({
  format,
  view,
}: {
  format: ContentFormat;
  view: EditorView | null;
}) {
  const commands = getCommands(format);
  return (
    <div className="toolbar" role="toolbar" aria-label="格式工具栏">
      {commands.map((c) => (
        <button
          key={c.id}
          type="button"
          className={`tool-btn tool-${c.id}`}
          title={c.title}
          aria-label={c.title}
          disabled={!view}
          onPointerDown={(e) => e.preventDefault()}
          onClick={() => view && c.run(view)}
        >
          {c.label}
        </button>
      ))}
    </div>
  );
}
