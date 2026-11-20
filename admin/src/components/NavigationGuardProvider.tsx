import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
  type ReactNode,
} from "react";

type Guard = () => boolean | Promise<boolean>;

type GuardValue = {
  /** 注册一个离开守卫；传 null 表示注销。视图卸载时应注销。 */
  setGuard: (guard: Guard | null) => void;
  /** 询问当前守卫是否允许离开。没有守卫时直接放行。 */
  canLeave: () => Promise<boolean>;
  /** 当前是否存在未保存改动（供界面提示用）。 */
  dirty: boolean;
  setDirty: (dirty: boolean) => void;
};

const GuardContext = createContext<GuardValue | null>(null);

export function useNavigationGuard(): GuardValue {
  const value = useContext(GuardContext);
  if (!value) throw new Error("useNavigationGuard 必须在 NavigationGuardProvider 内使用");
  return value;
}

/**
 * 导航守卫。目前主要内容编辑器会注册守卫；
 * Shell 在切换视图前调用 canLeave()，未保存时弹出确认框。
 */
export function NavigationGuardProvider({ children }: { children: ReactNode }) {
  const guardRef = useRef<Guard | null>(null);
  const [dirty, setDirty] = useState(false);

  const setGuard = useCallback((guard: Guard | null) => {
    guardRef.current = guard;
    if (!guard) setDirty(false);
  }, []);

  const canLeave = useCallback(async () => {
    const guard = guardRef.current;
    if (!guard) return true;
    try {
      return await guard();
    } catch {
      return false;
    }
  }, []);

  return (
    <GuardContext.Provider value={{ setGuard, canLeave, dirty, setDirty }}>
      {children}
    </GuardContext.Provider>
  );
}
