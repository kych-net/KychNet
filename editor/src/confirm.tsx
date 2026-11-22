import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { Modal } from "./Modal";

export type ConfirmOptions = {
  title: string;
  body?: ReactNode;
  confirmText?: string;
  cancelText?: string;
  /** 危险操作（丢弃 / 重置 / 删除）用红色主按钮。 */
  danger?: boolean;
};

type Pending = { options: ConfirmOptions; resolve: (ok: boolean) => void };

/**
 * 轻量确认弹层：`const ok = await confirm({...})`。
 *
 * 放弃草稿、还原提交、切换分支等不可撤销操作统一走这里，不再依赖
 * window.confirm（无法定制文案里的数量、也没有焦点管理）。
 */
export function useConfirm() {
  const [pending, setPending] = useState<Pending | null>(null);
  const pendingRef = useRef<Pending | null>(null);
  pendingRef.current = pending;

  const confirm = useCallback(
    (options: ConfirmOptions) =>
      new Promise<boolean>((resolve) => {
        // 已有弹层时先取消上一个，避免多个 await 同时挂起。
        pendingRef.current?.resolve(false);
        setPending({ options, resolve });
      }),
    [],
  );

  const settle = useCallback((ok: boolean) => {
    setPending((current) => {
      current?.resolve(ok);
      return null;
    });
  }, []);

  // 宿主组件卸载时把未决的 await 收尾，避免调用方永远等下去。
  useEffect(
    () => () => {
      pendingRef.current?.resolve(false);
    },
    [],
  );

  const node = pending ? (
    <ConfirmDialog options={pending.options} onSettle={settle} />
  ) : null;

  return { confirm, confirmNode: node };
}

function ConfirmDialog({
  options,
  onSettle,
}: {
  options: ConfirmOptions;
  onSettle: (ok: boolean) => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);

  // 默认把焦点放在「取消」上：确认框多为破坏性操作，误按回车不应生效。
  useEffect(() => {
    cancelRef.current?.focus();
  }, []);

  return (
    <Modal title={options.title} onClose={() => onSettle(false)} className="modal-confirm">
      {options.body && <div className="modal-confirm-body">{options.body}</div>}
      <div className="modal-actions">
        <button ref={cancelRef} type="button" className="btn" onClick={() => onSettle(false)}>
          {options.cancelText ?? "取消"}
        </button>
        <button
          type="button"
          className={options.danger ? "btn btn-danger" : "btn btn-accent"}
          onClick={() => onSettle(true)}
        >
          {options.confirmText ?? "确定"}
        </button>
      </div>
    </Modal>
  );
}
