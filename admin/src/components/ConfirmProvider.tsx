import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

type ConfirmOptions = {
  title?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
};

type ConfirmRequest = {
  message: string;
  options: ConfirmOptions;
  resolve: (ok: boolean) => void;
};

type Ask = (message: string, options?: ConfirmOptions) => Promise<boolean>;

const ConfirmContext = createContext<Ask | null>(null);

/** 全局主题化确认框。返回 true=确定，false=取消。 */
export function useConfirm(): Ask {
  const ask = useContext(ConfirmContext);
  if (!ask) throw new Error("useConfirm 必须在 ConfirmProvider 内使用");
  return ask;
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<ConfirmRequest | null>(null);
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement | null>(null);

  const ask = useCallback<Ask>((message, options = {}) => {
    return new Promise<boolean>((resolve) => {
      setRequest({ message, options, resolve });
    });
  }, []);

  const settle = useCallback(
    (ok: boolean) => {
      setRequest((current) => {
        current?.resolve(ok);
        return null;
      });
    },
    [],
  );

  // 打开时把焦点放到默认按钮；支持 Esc 取消。
  useEffect(() => {
    if (!request) return;
    const danger = request.options.danger;
    const target = danger ? cancelRef.current : confirmRef.current;
    target?.focus();

    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        settle(false);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, settle]);

  const opts = request?.options;

  return (
    <ConfirmContext.Provider value={ask}>
      {children}
      {request && (
        <div className="modal-backdrop confirm-backdrop" onMouseDown={() => settle(false)}>
          <div
            className="modal confirm-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="confirm-title"
            aria-describedby="confirm-message"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <h3 id="confirm-title">{opts?.title ?? "请确认"}</h3>
            <p id="confirm-message" className="confirm-message">
              {request.message}
            </p>
            <div className="modal-actions">
              <button
                ref={cancelRef}
                type="button"
                data-testid="confirm-cancel"
                onClick={() => settle(false)}
              >
                {opts?.cancelLabel ?? "取消"}
              </button>
              <button
                ref={confirmRef}
                type="button"
                className={opts?.danger ? "danger" : ""}
                data-testid="confirm-ok"
                onClick={() => settle(true)}
              >
                {opts?.confirmLabel ?? "确定"}
              </button>
            </div>
          </div>
        </div>
      )}
    </ConfirmContext.Provider>
  );
}
