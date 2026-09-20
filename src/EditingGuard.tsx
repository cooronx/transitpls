import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { confirm } from "@tauri-apps/plugin-dialog";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

type DirtyEditor = { id: string; discard: () => void };
const Context = createContext({
  dirtyId: null as string | null,
  pendingId: null as string | null,
  setPending: (_id: string | null) => {},
  setDirty: (_editor: DirtyEditor | null) => {},
});
export const useEditingGuard = () => useContext(Context);

export function EditingGuard({ children }: { children: ReactNode }) {
  const [dirtyId, setDirtyId] = useState<string | null>(null);
  const [pendingId, setPendingId] = useState<string | null>(null);
  const pending = useRef<string | null>(null);
  const setPending = useCallback((id: string | null) => {
    pending.current = id;
    setPendingId(id);
  }, []);
  const dirty = useRef<DirtyEditor | null>(null);
  const asking = useRef(false);
  const setDirty = useCallback((editor: DirtyEditor | null) => {
    dirty.current = editor;
    setDirtyId(editor?.id ?? null);
  }, []);
  const discard = useCallback(async () => {
    if (pending.current) return false;
    if (!dirty.current) return true;
    if (asking.current) return false;
    asking.current = true;
    try {
      const accepted = await confirm("此段有未保存的修改。放弃修改并继续吗？", {
        title: "未保存的译文",
        kind: "warning",
        okLabel: "放弃修改",
        cancelLabel: "继续编辑",
      });
      if (accepted) {
        dirty.current?.discard();
        setDirty(null);
      }
      return accepted;
    } finally {
      asking.current = false;
    }
  }, [setDirty]);
  useEffect(() => {
    const beforeUnload = (event: BeforeUnloadEvent) => {
      if (dirty.current || pending.current) {
        event.preventDefault();
        event.returnValue = "";
      }
    };
    window.addEventListener("beforeunload", beforeUnload);
    let dispose: (() => void) | undefined;
    let stopped = false;
    if (isTauri())
      void getCurrentWindow()
        .onCloseRequested(async (event) => {
          if (pending.current) {
            event.preventDefault();
            return;
          }
          if (!dirty.current) return;
          if (!(await discard())) event.preventDefault();
        })
        .then((unlisten) => {
          if (stopped) unlisten();
          else dispose = unlisten;
        });
    return () => {
      stopped = true;
      dispose?.();
      window.removeEventListener("beforeunload", beforeUnload);
    };
  }, [discard]);
  return (
    <Context.Provider value={{ dirtyId, setDirty, pendingId, setPending }}>
      <div
        className="editing-guard"
        onClickCapture={(event) => {
          if (
            (!dirty.current && !pending.current) ||
            !(event.target instanceof Element)
          )
            return;
          const action = event.target.closest<
            HTMLButtonElement | HTMLAnchorElement
          >("button,a");
          if (
            !action ||
            action.closest(".window-controls") ||
            action.closest<HTMLElement>("[data-editing-segment]")?.dataset
              .editingSegment === (pending.current ?? dirty.current?.id)
          )
            return;
          event.preventDefault();
          event.stopPropagation();
          if (pending.current) return;
          void discard().then((accepted) => {
            if (accepted)
              window.setTimeout(() => {
                if (action.isConnected) action.click();
              }, 0);
          });
        }}
      >
        {children}
      </div>
    </Context.Provider>
  );
}
