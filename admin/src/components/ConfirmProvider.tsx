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

/** 应用内输入弹窗的选项；替代 window.prompt。 */
type PromptOptions = {
  title?: string;
  /** 输入框上方的标签；留空则不显示。 */
  label?: string;
  initial?: string;
  placeholder?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /** 返回错误文案则阻止提交并显示在输入框下方；返回 null 表示通过。 */
  validate?: (value: string) => string | null;
};

type ConfirmRequest = {
  kind: "confirm";
  message: string;
  options: ConfirmOptions;
  resolve: (ok: boolean) => void;
};

type PromptRequest = {
  kind: "prompt";
  message: string;
  options: PromptOptions;
  resolve: (value: string | null) => void;
};

type Request = ConfirmRequest | PromptRequest;

type Ask = (message: string, options?: ConfirmOptions) => Promise<boolean>;
type AskPrompt = (message: string, options?: PromptOptions) => Promise<string | null>;

const ConfirmContext = createContext<Ask | null>(null);
const PromptContext = createContext<AskPrompt | null>(null);

/** 全局主题化确认框。返回 true=确定，false=取消。 */
export function useConfirm(): Ask {
  const ask = useContext(ConfirmContext);
  if (!ask) throw new Error("useConfirm 必须在 ConfirmProvider 内使用");
  return ask;
}

/** 全局主题化输入弹窗。返回输入值；取消返回 null。 */
export function usePrompt(): AskPrompt {
  const ask = useContext(PromptContext);
  if (!ask) throw new Error("usePrompt 必须在 ConfirmProvider 内使用");
  return ask;
}

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "a[href]",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

/** 收集弹窗内当前可聚焦的元素（跳过被隐藏的）。 */
function focusableNodes(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (node) => node.getClientRects().length > 0,
  );
}

export function ConfirmProvider({ children }: { children: ReactNode }) {
  const [request, setRequest] = useState<Request | null>(null);
  // 当前未结算的请求。并发调用时用它把上一个请求按「取消」结算掉，
  // 否则旧的 Promise 永远不会 resolve，调用方（useBusy）的 busy 状态会卡死。
  const requestRef = useRef<Request | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // 打开弹窗前聚焦的元素：关闭后把焦点还回去。
  const openerRef = useRef<HTMLElement | null>(null);
  const [value, setValue] = useState("");
  const [fieldError, setFieldError] = useState("");

  /** 把尚未结算的请求按「取消」结算，避免 Promise 悬挂。 */
  const supersede = useCallback(() => {
    const previous = requestRef.current;
    if (!previous) return;
    requestRef.current = null;
    if (previous.kind === "confirm") previous.resolve(false);
    else previous.resolve(null);
  }, []);

  const ask = useCallback<Ask>(
    (message, options = {}) => {
      return new Promise<boolean>((resolve) => {
        supersede();
        const next: ConfirmRequest = { kind: "confirm", message, options, resolve };
        requestRef.current = next;
        setFieldError("");
        setRequest(next);
      });
    },
    [supersede],
  );

  const askPrompt = useCallback<AskPrompt>(
    (message, options = {}) => {
      return new Promise<string | null>((resolve) => {
        supersede();
        const next: PromptRequest = { kind: "prompt", message, options, resolve };
        requestRef.current = next;
        setValue(options.initial ?? "");
        setFieldError("");
        setRequest(next);
      });
    },
    [supersede],
  );

  const settleConfirm = useCallback((ok: boolean) => {
    const current = requestRef.current;
    requestRef.current = null;
    setRequest(null);
    if (current?.kind === "confirm") current.resolve(ok);
  }, []);

  const cancelPrompt = useCallback(() => {
    const current = requestRef.current;
    requestRef.current = null;
    setRequest(null);
    if (current?.kind === "prompt") current.resolve(null);
  }, []);

  const submitPrompt = useCallback(() => {
    const current = requestRef.current;
    if (!current || current.kind !== "prompt") return;
    const message = current.options.validate?.(value) ?? null;
    if (message) {
      setFieldError(message);
      inputRef.current?.focus();
      return;
    }
    requestRef.current = null;
    setRequest(null);
    current.resolve(value);
  }, [value]);

  // 打开时记录来源焦点并聚焦弹窗内的首个可操作元素；
  // Esc 关闭，Tab / Shift+Tab 在弹窗内循环（不穿到背景）。
  useEffect(() => {
    const current = request;
    if (!current) return;
    openerRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;

    if (current.kind === "prompt") {
      inputRef.current?.focus();
      inputRef.current?.select();
    } else if (current.options.danger) {
      cancelRef.current?.focus();
    } else {
      confirmRef.current?.focus();
    }

    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        if (current!.kind === "confirm") settleConfirm(false);
        else cancelPrompt();
        return;
      }
      if (event.key !== "Tab") return;
      const nodes = focusableNodes(dialogRef.current);
      if (nodes.length === 0) {
        event.preventDefault();
        dialogRef.current?.focus();
        return;
      }
      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      const active = document.activeElement as HTMLElement | null;
      if (event.shiftKey) {
        if (active === first || !active || !nodes.includes(active)) {
          event.preventDefault();
          last.focus();
        }
      } else if (active === last || !active || !nodes.includes(active)) {
        event.preventDefault();
        first.focus();
      }
    }

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [request, settleConfirm, cancelPrompt]);

  // 关闭后把焦点还给打开它的元素。
  useEffect(() => {
    if (request) return;
    const opener = openerRef.current;
    openerRef.current = null;
    if (opener && document.contains(opener)) {
      opener.focus();
    }
  }, [request]);

  return (
    <ConfirmContext.Provider value={ask}>
      <PromptContext.Provider value={askPrompt}>
        {children}
        {request && (
          <div
            className="modal-backdrop confirm-backdrop"
            onMouseDown={() =>
              request.kind === "confirm" ? settleConfirm(false) : cancelPrompt()
            }
          >
            <div
              ref={dialogRef}
              className="modal confirm-dialog"
              role="alertdialog"
              aria-modal="true"
              aria-labelledby="confirm-title"
              aria-describedby="confirm-message"
              tabIndex={-1}
              onMouseDown={(event) => event.stopPropagation()}
            >
              {request.kind === "confirm" ? (
                <>
                  <h3 id="confirm-title">{request.options.title ?? "请确认"}</h3>
                  <p id="confirm-message" className="confirm-message">
                    {request.message}
                  </p>
                  <div className="modal-actions">
                    <button
                      ref={cancelRef}
                      type="button"
                      data-testid="confirm-cancel"
                      onClick={() => settleConfirm(false)}
                    >
                      {request.options.cancelLabel ?? "取消"}
                    </button>
                    <button
                      ref={confirmRef}
                      type="button"
                      className={request.options.danger ? "danger" : ""}
                      data-testid="confirm-ok"
                      onClick={() => settleConfirm(true)}
                    >
                      {request.options.confirmLabel ?? "确定"}
                    </button>
                  </div>
                </>
              ) : (
                <form
                  className="confirm-prompt"
                  onSubmit={(event) => {
                    event.preventDefault();
                    submitPrompt();
                  }}
                >
                  <h3 id="confirm-title">{request.options.title ?? "请输入"}</h3>
                  <p id="confirm-message" className="confirm-message">
                    {request.message}
                  </p>
                  <label>
                    {request.options.label ?? "输入"}
                    <input
                      ref={inputRef}
                      value={value}
                      placeholder={request.options.placeholder}
                      aria-invalid={fieldError ? true : undefined}
                      aria-describedby={fieldError ? "confirm-prompt-error" : undefined}
                      onChange={(event) => {
                        setValue(event.target.value);
                        if (fieldError) setFieldError("");
                      }}
                    />
                  </label>
                  {fieldError && (
                    <p id="confirm-prompt-error" className="error" role="alert">
                      {fieldError}
                    </p>
                  )}
                  <div className="modal-actions">
                    <button
                      ref={cancelRef}
                      type="button"
                      data-testid="prompt-cancel"
                      onClick={cancelPrompt}
                    >
                      {request.options.cancelLabel ?? "取消"}
                    </button>
                    <button ref={confirmRef} type="submit" data-testid="prompt-ok">
                      {request.options.confirmLabel ?? "确定"}
                    </button>
                  </div>
                </form>
              )}
            </div>
          </div>
        )}
      </PromptContext.Provider>
    </ConfirmContext.Provider>
  );
}
