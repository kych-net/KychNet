import { useCallback, useRef, useState } from "react";

/**
 * 把异步动作包一层 busy 状态，防止重复提交。
 * 返回 run(fn)：若正在进行则直接返回；结束（成功或失败）后自动复位。
 */
export function useBusy() {
  const [busy, setBusy] = useState(false);
  const running = useRef(false);

  const run = useCallback(async (task: () => Promise<void>) => {
    if (running.current) return;
    running.current = true;
    setBusy(true);
    try {
      await task();
    } finally {
      running.current = false;
      setBusy(false);
    }
  }, []);

  return { busy, run };
}
