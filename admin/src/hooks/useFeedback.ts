import { useCallback, useEffect, useRef, useState } from "react";

const NOTICE_MS = 7000;
const ERROR_MS = 12000;

/**
 * 统一的成功/错误提示状态。
 * - setNotice / setError 接受字符串，调用点写法与原来的 useState setter 一致。
 * - 两者互斥：显示其一会清掉另一个。
 * - 自动消失：notice 7 秒、error 12 秒；组件卸载时清理定时器。
 */
export function useFeedback() {
  const [error, setErrorText] = useState("");
  const [notice, setNoticeText] = useState("");
  const errorTimer = useRef<number | undefined>(undefined);
  const noticeTimer = useRef<number | undefined>(undefined);

  const clearError = useCallback(() => {
    if (errorTimer.current !== undefined) window.clearTimeout(errorTimer.current);
    errorTimer.current = undefined;
    setErrorText("");
  }, []);

  const clearNotice = useCallback(() => {
    if (noticeTimer.current !== undefined) window.clearTimeout(noticeTimer.current);
    noticeTimer.current = undefined;
    setNoticeText("");
  }, []);

  const setError = useCallback(
    (message: string) => {
      clearNotice();
      if (errorTimer.current !== undefined) window.clearTimeout(errorTimer.current);
      setErrorText(message);
      if (message) {
        errorTimer.current = window.setTimeout(() => {
          errorTimer.current = undefined;
          setErrorText("");
        }, ERROR_MS);
      }
    },
    [clearNotice],
  );

  const setNotice = useCallback(
    (message: string) => {
      clearError();
      if (noticeTimer.current !== undefined) window.clearTimeout(noticeTimer.current);
      setNoticeText(message);
      if (message) {
        noticeTimer.current = window.setTimeout(() => {
          noticeTimer.current = undefined;
          setNoticeText("");
        }, NOTICE_MS);
      }
    },
    [clearError],
  );

  useEffect(
    () => () => {
      if (errorTimer.current !== undefined) window.clearTimeout(errorTimer.current);
      if (noticeTimer.current !== undefined) window.clearTimeout(noticeTimer.current);
    },
    [],
  );

  return { error, notice, setError, setNotice, clearError, clearNotice };
}
