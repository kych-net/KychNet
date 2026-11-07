import { useEffect, useState } from "react";
import { startAuthentication } from "@simplewebauthn/browser";
import { api } from "../api";

type Props = {
  open: boolean;
  onClose: () => void;
  onSuccess: () => void;
};

export function StepUpDialog({ open, onClose, onSuccess }: Props) {
  const [method, setMethod] = useState<"totp" | "passkey">("totp");
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (open) {
      setCode("");
      setError("");
      setBusy(false);
    }
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
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="step-up-title">
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

        {error && <p className="error">{error}</p>}
      </div>
    </div>
  );
}
