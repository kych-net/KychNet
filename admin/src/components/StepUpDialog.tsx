import { useEffect, useRef, useState } from "react";
import { startAuthentication } from "@simplewebauthn/browser";
import { api } from "../api";

type Props = {
  open: boolean;
  onClose: () => void;
  onSuccess: () => void;
};

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "a[href]",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function focusableNodes(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
    (node) => node.getClientRects().length > 0,
  );
}

export function StepUpDialog({ open, onClose, onSuccess }: Props) {
  const [method, setMethod] = useState<"totp" | "passkey">("totp");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // 打开前的焦点元素：关闭后还回去，避免焦点掉到 body 上。
  const openerRef = useRef<HTMLElement | null>(null);
  // 父组件每次渲染都会传新的内联 onClose；用 ref 保持 effect 只随 open 变化。
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    if (open) {
      setCode("");
      setError("");
      setBusy(false);
    }
  }, [open]);

  // 模态行为：记录来源焦点、聚焦首个输入框、Esc 关闭、Tab 在弹窗内循环。
  useEffect(() => {
    if (!open) return;
    openerRef.current =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    inputRef.current?.focus();

    function onKey(event: KeyboardEvent) {
      if (event.key === "Escape") {
        event.preventDefault();
        onCloseRef.current();
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
  }, [open]);

  // 关闭后恢复焦点。
  useEffect(() => {
    if (open) return;
    const opener = openerRef.current;
    openerRef.current = null;
    if (opener && document.contains(opener)) opener.focus();
  }, [open]);

  if (!open) return null;

  async function confirmTotp(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      await api.stepUpTotp(code);
      onSuccess();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function confirmPasskey() {
    setError("");
    setBusy(true);
    try {
      const started = await api.stepUpPasskeyStart();
      const options = started.options.publicKey ?? started.options;
      const credential = await startAuthentication({ optionsJSON: options });
      await api.stepUpPasskeyFinish(started.flow_id, credential);
      onSuccess();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop">
      <div
        ref={dialogRef}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="step-up-title"
        tabIndex={-1}
      >
        <h3 id="step-up-title">重新验证身份</h3>
        <p className="hint">
          修改二次验证或通行密钥前需要确认是你本人；验证成功后 5 分钟内有效。
        </p>
        <div className="tabs">
          <button
            type="button"
            className={method === "totp" ? "active" : ""}
            onClick={() => setMethod("totp")}
          >
            动态验证码
          </button>
          <button
            type="button"
            className={method === "passkey" ? "active" : ""}
            onClick={() => setMethod("passkey")}
          >
            通行密钥
          </button>
        </div>

        {method === "totp" ? (
          <form onSubmit={confirmTotp}>
            <label>
              6 位验证码
              <input
                ref={inputRef}
                value={code}
                onChange={(e) => setCode(e.target.value)}
                inputMode="numeric"
                autoComplete="one-time-code"
                required
              />
            </label>
            <div className="modal-actions">
              <button type="button" onClick={onClose} disabled={busy}>
                取消
              </button>
              <button type="submit" disabled={busy}>
                {busy ? "验证中…" : "确认"}
              </button>
            </div>
          </form>
        ) : (
          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              取消
            </button>
            <button type="button" onClick={confirmPasskey} disabled={busy}>
              {busy ? "验证中…" : "使用通行密钥确认"}
            </button>
          </div>
        )}

        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </div>
    </div>
  );
}
