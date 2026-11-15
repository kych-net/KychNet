import type { PrFileDiff } from "./types";
import type { DiffRow, LocalDiff } from "./diff";

const STATUS_LABEL: Record<string, string> = {
  add: "新增",
  modify: "修改",
  delete: "删除",
};

function DiffTable({ rows }: { rows: DiffRow[] }) {
  return (
    <table className="pv-diff-table">
      <tbody>
        {rows.map((row, index) => (
          <tr key={index} className={`pv-diff-row pv-diff-${row.kind}`}>
            <td className="pv-diff-no">{row.oldLine ?? ""}</td>
            <td className="pv-diff-no">{row.newLine ?? ""}</td>
            <td className="pv-diff-sign">
              {row.kind === "add" ? "+" : row.kind === "del" ? "-" : " "}
            </td>
            <td className="pv-diff-text"><code>{row.text || " "}</code></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** 本地草稿差异（浏览器端算）。 */
export function LocalDiffView({ diff, empty }: { diff: LocalDiff; empty?: string }) {
  if (diff.truncated) {
    return <p className="pv-hint">文件过大，未生成行级差异。</p>;
  }
  if (diff.hunks.length === 0) {
    return <p className="pv-hint">{empty ?? "没有可见的行级变化。"}</p>;
  }
  return (
    <div className="pv-diff">
      {diff.hunks.map((hunk, index) => (
        <div className="pv-diff-hunk" key={index}>
          <div className="pv-diff-hunk-head">{hunk.header}</div>
          <DiffTable rows={hunk.rows} />
        </div>
      ))}
    </div>
  );
}

/** 服务端返回的提交 / 分支对比差异。 */
export function ServerDiffView({ files }: { files: PrFileDiff[] }) {
  if (files.length === 0) return <p className="pv-hint">没有文件差异。</p>;
  return (
    <div className="pv-file-diffs">
      {files.map((file) => (
        <section className="pv-file-diff" key={file.path}>
          <div className="pv-file-diff-head">
            <span className={`pv-chip pv-chip-${file.status}`}>
              {STATUS_LABEL[file.status] ?? file.status}
            </span>
            <code className="pv-path">{file.path}</code>
            <span className="pv-diff-stat">
              <span className="pv-plus">+{file.additions}</span>
              <span className="pv-minus">-{file.deletions}</span>
            </span>
          </div>
          {file.truncated ? (
            <p className="pv-hint">文件过大或不是文本，未显示行级差异。</p>
          ) : file.hunks.length === 0 ? (
            <p className="pv-hint">没有可见的行级变化。</p>
          ) : (
            file.hunks.map((hunk, index) => {
              const rows: DiffRow[] = hunk.lines.map((line) => ({
                kind: line.kind,
                oldLine: line.old_line,
                newLine: line.new_line,
                text: line.text,
              }));
              return (
                <div className="pv-diff-hunk" key={index}>
                  <div className="pv-diff-hunk-head">{hunk.header}</div>
                  <DiffTable rows={rows} />
                </div>
              );
            })
          )}
        </section>
      ))}
    </div>
  );
}
