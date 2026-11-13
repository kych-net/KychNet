import { useCallback, useEffect, useRef, useState } from "react";

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

// 防抖自动保存 + localStorage 草稿兜底。返回状态与手动保存函数。
export function useAutosave(
  save: () => Promise<void>,
  draftKey: string | null,
  body: string,
  enabled: boolean,
) {
  const [status, setStatus] = useState<SaveStatus>("idle");
  const timer = useRef<number | null>(null);
  const saveRef = useRef(save);
  saveRef.current = save;

  const flush = useCallback(async () => {
    if (timer.current) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
    setStatus("saving");
    try {
      await saveRef.current();
      setStatus("saved");
      if (draftKey) localStorage.removeItem(draftKey);
    } catch {
      setStatus("error");
    }
  }, [draftKey]);

  // 内容变化：落草稿并排程自动保存。
  useEffect(() => {
    if (!enabled) return;
    if (draftKey) {
      try {
        localStorage.setItem(draftKey, body);
      } catch {
        /* 配额满时忽略，仍走服务端保存 */
      }
    }
    setStatus("dirty");
    if (timer.current) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => void flush(), 1600);
    return () => {
      if (timer.current) window.clearTimeout(timer.current);
    };
    // body 变化即排程；flush 稳定。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [body, enabled, draftKey]);

  return { status, flush };
}
