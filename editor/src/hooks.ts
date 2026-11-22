import { useCallback, useEffect, useRef, useState } from "react";
import { errText } from "./project/types";

// 可视视口：移动端软键盘弹出时 visualViewport 收缩，据此把编辑区与工具栏
// 顶到键盘上方，避免遮挡。不支持 visualViewport 的环境退化为 innerHeight。
export function useVisualViewport() {
  const [vp, setVp] = useState(() => ({
    height: typeof window !== "undefined" ? window.innerHeight : 0,
    offsetTop: 0,
  }));
  useEffect(() => {
    const vv = window.visualViewport;
    if (!vv) {
      const onResize = () => setVp({ height: window.innerHeight, offsetTop: 0 });
      window.addEventListener("resize", onResize);
      return () => window.removeEventListener("resize", onResize);
    }
    const update = () => setVp({ height: vv.height, offsetTop: vv.offsetTop });
    update();
    vv.addEventListener("resize", update);
    vv.addEventListener("scroll", update);
    return () => {
      vv.removeEventListener("resize", update);
      vv.removeEventListener("scroll", update);
    };
  }, []);
  return vp;
}

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() =>
    typeof window !== "undefined" ? window.matchMedia(query).matches : false,
  );
  useEffect(() => {
    const mql = window.matchMedia(query);
    const onChange = () => setMatches(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

export type SaveStatus = "idle" | "dirty" | "saving" | "saved" | "error";

const AUTOSAVE_DELAY = 1600;

/**
 * 防抖自动保存 + localStorage 草稿兜底。
 *
 * `dirty` 由调用方按「路径 / 格式 / 发布状态 / 正文」整体快照与上次保存结果
 * 比较得出，所以这些字段任一变化都会触发保存，状态点也会显示「待保存」。
 * 返回状态、最近一次失败信息与手动保存函数。
 */
export function useAutosave(
  save: () => Promise<void>,
  draftKey: string | null,
  body: string,
  dirty: boolean,
) {
  const [status, setStatus] = useState<SaveStatus>("idle");
  const [error, setError] = useState("");
  const timer = useRef<number | null>(null);
  const saveRef = useRef(save);
  saveRef.current = save;
  // 串行保存：同一时刻只允许一个请求在飞。保存期间又有新编辑时只记一个
  // 「待保存」标记，当前请求完成后再存最新一次——并发保存会让慢请求把
  // 较新的内容覆盖回旧版本。
  const inFlight = useRef(false);
  const pending = useRef(false);

  const clearTimer = useCallback(() => {
    if (timer.current) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  const flush = useCallback(async () => {
    clearTimer();
    if (inFlight.current) {
      pending.current = true;
      return;
    }
    inFlight.current = true;
    setStatus("saving");
    try {
      do {
        pending.current = false;
        await saveRef.current();
        // 保存期间用户又改了：立刻再存一次，直到没有新的待保存标记。
      } while (pending.current);
      setStatus("saved");
      setError("");
      if (draftKey) localStorage.removeItem(draftKey);
    } catch (e) {
      setStatus("error");
      setError(errText(e));
    } finally {
      inFlight.current = false;
    }
  }, [clearTimer, draftKey]);

  // 草稿兜底：只在有未保存改动时写本地，刷新/崩溃后还能恢复。
  // 保存成功后不再写，避免下次打开时把已保存内容当成「恢复的草稿」。
  useEffect(() => {
    if (!draftKey || !dirty) return;
    try {
      localStorage.setItem(draftKey, body);
    } catch {
      /* 配额满时忽略，仍走服务端保存 */
    }
  }, [body, dirty, draftKey]);

  // 有待保存内容时排程自动保存；dirty 归零（刚保存完）则取消计时器。
  useEffect(() => {
    if (!dirty) {
      clearTimer();
      return;
    }
    setStatus((current) => (current === "saving" ? current : "dirty"));
    clearTimer();
    timer.current = window.setTimeout(() => {
      timer.current = null;
      void flush();
    }, AUTOSAVE_DELAY);
    return clearTimer;
  }, [body, dirty, flush, clearTimer]);

  return { status, error, flush, clearError: useCallback(() => setError(""), []) };
}
