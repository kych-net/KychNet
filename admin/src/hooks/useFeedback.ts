import { useCallback, useEffect, useRef, useState } from "react";

const NOTICE_MS = 7000;
const ERROR_MS = 12000;

/**
 * 统一的成功/错误提示状态。
 * - setNotice / setError 接受字符串，调用点写法与原来的 useState setter 一致。
 * - 两者互斥：显示其一会清掉另一个。
 * - 自动消失：notice 7 秒、error 12 秒；鼠标悬停任意提示条时暂停倒计时，
 *   移开后按剩余时间继续，避免长消息（尤其错误详情）读到一半被清掉。
 * - 组件卸载时清理定时器与订阅。
 */

// 模块级：所有活着的 hook 实例把「暂停/恢复」回调注册到这里。
// 提示条是本 hook 驱动的 <p class="error|notice">，用事件委托统一监听，
// 这样各视图无需改任何调用点即可获得 hover 暂停能力。
type Controller = { pause: () => void; resume: () => void };
const controllers = new Set<Controller>();
let listening = false;

// 当前被悬停的提示条元素集合。所有实例共用一个集合，任一为空才恢复。
const hoveredBars = new Set<Element>();
const BAR_SELECTOR = "p.error, p.notice";

function onOver(event: Event) {
  const bar = (event.target as HTMLElement | null)?.closest?.(BAR_SELECTOR);
  if (!bar) return;
  hoveredBars.add(bar);
  controllers.forEach((c) => c.pause());
}

function onOut(event: Event) {
  const bar = (event.target as HTMLElement | null)?.closest?.(BAR_SELECTOR);
  if (!bar) return;
  const next = (event as MouseEvent).relatedTarget as Node | null;
  // 在同一条提示内部移动不算离开。
  if (next && bar.contains(next)) return;
  hoveredBars.delete(bar);
  if (hoveredBars.size === 0) controllers.forEach((c) => c.resume());
}

export function useFeedback() {
  const [error, setErrorText] = useState("");
  const [notice, setNoticeText] = useState("");
  const errorTimer = useRef<number | undefined>(undefined);
  const noticeTimer = useRef<number | undefined>(undefined);
  // 剩余时间与起算时刻：暂停时折算剩余，恢复时用剩余重新排程。
  const errorRemaining = useRef(ERROR_MS);
  const noticeRemaining = useRef(NOTICE_MS);
  const errorStart = useRef(0);
  const noticeStart = useRef(0);
  const messageRef = useRef({ error: "", notice: "" });
  messageRef.current = { error, notice };

  const scheduleError = useCallback((ms: number) => {
    if (errorTimer.current !== undefined) window.clearTimeout(errorTimer.current);
    errorRemaining.current = ms;
    errorStart.current = Date.now();
    errorTimer.current = window.setTimeout(() => {
      errorTimer.current = undefined;
      setErrorText("");
    }, ms);
  }, []);

  const scheduleNotice = useCallback((ms: number) => {
    if (noticeTimer.current !== undefined) window.clearTimeout(noticeTimer.current);
    noticeRemaining.current = ms;
    noticeStart.current = Date.now();
    noticeTimer.current = window.setTimeout(() => {
      noticeTimer.current = undefined;
      setNoticeText("");
    }, ms);
  }, []);

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
      setErrorText(message);
      if (message) scheduleError(ERROR_MS);
      else if (errorTimer.current !== undefined) {
        window.clearTimeout(errorTimer.current);
        errorTimer.current = undefined;
      }
    },
    [clearNotice, scheduleError],
  );

  const setNotice = useCallback(
    (message: string) => {
      clearError();
      setNoticeText(message);
      if (message) scheduleNotice(NOTICE_MS);
      else if (noticeTimer.current !== undefined) {
        window.clearTimeout(noticeTimer.current);
        noticeTimer.current = undefined;
      }
    },
    [clearError, scheduleNotice],
  );

  // 注册本实例的 pause / resume；通过 messageRef 读取最新消息，避免闭包过期。
  useEffect(() => {
    const controller: Controller = {
      pause: () => {
        if (errorTimer.current !== undefined) {
          window.clearTimeout(errorTimer.current);
          errorTimer.current = undefined;
          errorRemaining.current = Math.max(0, errorRemaining.current - (Date.now() - errorStart.current));
        }
        if (noticeTimer.current !== undefined) {
          window.clearTimeout(noticeTimer.current);
          noticeTimer.current = undefined;
          noticeRemaining.current = Math.max(0, noticeRemaining.current - (Date.now() - noticeStart.current));
        }
      },
      resume: () => {
        if (errorTimer.current === undefined && errorRemaining.current > 0 && messageRef.current.error) {
          scheduleError(errorRemaining.current);
        }
        if (noticeTimer.current === undefined && noticeRemaining.current > 0 && messageRef.current.notice) {
          scheduleNotice(noticeRemaining.current);
        }
      },
    };
    controllers.add(controller);
    if (!listening) {
      window.addEventListener("mouseover", onOver, true);
      window.addEventListener("mouseout", onOut, true);
      listening = true;
    }
    return () => {
      controllers.delete(controller);
      if (controllers.size === 0 && listening) {
        window.removeEventListener("mouseover", onOver, true);
        window.removeEventListener("mouseout", onOut, true);
        listening = false;
        hoveredBars.clear();
      }
    };
  }, [scheduleError, scheduleNotice]);

  useEffect(
    () => () => {
      if (errorTimer.current !== undefined) window.clearTimeout(errorTimer.current);
      if (noticeTimer.current !== undefined) window.clearTimeout(noticeTimer.current);
    },
    [],
  );

  return { error, notice, setError, setNotice, clearError, clearNotice };
}
