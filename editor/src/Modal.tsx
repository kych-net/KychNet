import { useEffect, useRef, type ReactNode } from "react";

/**
 * 轻量模态浮层：Esc 关闭、打开时聚焦首个可聚焦元素、Tab 在浮层内循环、
 * 关闭后把焦点还给触发它的元素。
 *
 * 三个浮层（页面设置 / 新建页面 / 移动端预览）都复用它，避免各写一遍
 * 焦点与键盘逻辑。onClose 保存在 ref 里，调用方不必用 useCallback 稳定它。
 */
export function Modal({
  title,
  onClose,
  children,
  className = "",
  labelledBy,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  className?: string;
  /** 需要自定义标题节点时用 id 指向它（aria-labelledby 优先于 aria-label）。 */
  labelledBy?: string;
}) {
  const dialogRef = useRef<HTMLDivElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const dialog = dialogRef.current;
    // 记录打开前的焦点，关闭时还原（若触发元素已被卸载则忽略）。
    const opener =
      document.activeElement instanceof HTMLElement ? document.activeElement : null;
    openerRef.current = opener;

    const focusables = (): HTMLElement[] => {
      if (!dialog) return [];
      const selector =
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
      return Array.from(dialog.querySelectorAll<HTMLElement>(selector));
    };

    // 焦点落在内容区第一个可聚焦元素（排除右上角关闭按钮）；
    // 内容区没有可聚焦元素时再退回关闭按钮。
    const items = focusables();
    const firstContent = items.find((el) => !el.classList.contains("modal-close")) ?? items[0];
    if (firstContent) firstContent.focus();
    else dialog?.focus();

    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        closeRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const items = focusables();
      if (items.length === 0) {
        event.preventDefault();
        dialog?.focus();
        return;
      }
      const head = items[0];
      const tail = items[items.length - 1];
      const active = document.activeElement;
      // 焦点跑出浮层（如点击遮罩后）时重新收拢。
      if (!dialog?.contains(active)) {
        event.preventDefault();
        head.focus();
        return;
      }
      if (event.shiftKey && active === head) {
        event.preventDefault();
        tail.focus();
      } else if (!event.shiftKey && active === tail) {
        event.preventDefault();
        head.focus();
      }
    };

    // 捕获阶段监听：浮层内的输入框按 Esc / Tab 也先经过这里。
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      const target = openerRef.current;
      if (target && target.isConnected) target.focus();
    };
  }, []);

  return (
    <div className="modal-backdrop" onClick={() => closeRef.current()}>
      <div
        ref={dialogRef}
        className={`modal ${className}`.trim()}
        role="dialog"
        aria-modal="true"
        aria-label={labelledBy ? undefined : title}
        aria-labelledby={labelledBy}
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2 id={labelledBy}>{title}</h2>
          <button
            type="button"
            className="icon-btn modal-close"
            aria-label={`关闭${title}`}
            onClick={() => closeRef.current()}
          >
            ✕
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
