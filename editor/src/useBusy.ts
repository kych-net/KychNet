import { useCallback, useEffect, useRef, useState } from "react";
import { errText } from "./project/types";

/**
 * 统一忙碌 / 错误状态：项目工作区里所有异步动作都经由 run 执行。
 *
 * - 同一时刻只跑一个动作（合并、重置、快速提交撞在一起时不会互相覆盖）；
 * - 失败的错误存在 hook 内部并由调用方渲染，不依赖动作自身的 try/catch，
 *   避免「请求失败但界面没有任何提示」；
 * - 组件卸载后不再 setState，避免悬空更新。
 */
export function useBusy() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const running = useRef(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const run = useCallback(async (task: () => Promise<void>): Promise<boolean> => {
    if (running.current) return false;
    running.current = true;
    if (mounted.current) {
      setBusy(true);
      setError("");
    }
    try {
      await task();
      return true;
    } catch (e) {
      if (mounted.current) setError(errText(e));
      return false;
    } finally {
      running.current = false;
      if (mounted.current) setBusy(false);
    }
  }, []);

  const clearError = useCallback(() => setError(""), []);

  return { busy, error, setError, clearError, run };
}
