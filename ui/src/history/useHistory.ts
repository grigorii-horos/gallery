// History state that outlives the screen: a running undo (it keeps going on
// other screens), its result, and the preparation of a retry plan.
import { useCallback, useEffect, useRef, useState } from "react";
import { errorText, historyApi, type RetryDone, type UndoDone, type UndoProgress } from "../api";

export interface UndoRun {
  reportPath: string;
  progress: UndoProgress;
  cancelling: boolean;
}

export interface HistoryState {
  undo: UndoRun | null;
  undoResult: UndoDone | null;
  undoError: { reportPath: string; message: string } | null;
  startUndo: (reportPath: string) => Promise<void>;
  /** Undo only these source files of the run. */
  startUndoFiles: (reportPath: string, sources: string[]) => Promise<void>;
  /** Undo only the files of one target of the run. */
  startUndoTarget: (reportPath: string, target: string) => Promise<void>;
  cancelUndo: () => void;
  /** Report whose retry plan is being prepared. */
  retrying: string | null;
  retryResult: RetryDone | null;
  retryError: { reportPath: string; message: string } | null;
  startRetry: (reportPath: string) => Promise<void>;
  /** Bumped when reports on disk changed (an undo or retry ended). */
  tick: number;
  refresh: () => void;
}

async function listenSafe<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<T>(name, (e) => cb(e.payload));
  } catch {
    return () => {};
  }
}

export function useHistory(): HistoryState {
  const [undo, setUndo] = useState<UndoRun | null>(null);
  const [undoResult, setUndoResult] = useState<UndoDone | null>(null);
  const [undoError, setUndoError] = useState<HistoryState["undoError"]>(null);
  const [retrying, setRetrying] = useState<string | null>(null);
  const [retryResult, setRetryResult] = useState<RetryDone | null>(null);
  const [retryError, setRetryError] = useState<HistoryState["retryError"]>(null);
  const [tick, setTick] = useState(0);
  const undoRef = useRef<UndoRun | null>(null);
  const showUndo = useCallback((u: UndoRun | null) => {
    undoRef.current = u;
    setUndo(u);
  }, []);
  const refresh = useCallback(() => setTick((t) => t + 1), []);

  // An undo still running from before a reload.
  useEffect(() => {
    historyApi
      .undoStatus()
      .then((st) => {
        if (st && !undoRef.current) showUndo({ reportPath: st.report_path, progress: st.progress, cancelling: false });
      })
      .catch(() => {});
  }, [showUndo]);

  useEffect(() => {
    const offs = [
      listenSafe<UndoProgress>("undo-progress", (p) => setUndo((u) => u && { ...u, progress: p })),
      listenSafe<UndoDone>("undo-done", (d) => {
        showUndo(null);
        setUndoResult(d);
        refresh();
      }),
      listenSafe<{ message: string }>("undo-error", (e) => {
        const path = undoRef.current?.reportPath ?? "";
        showUndo(null);
        setUndoError({ reportPath: path, message: e.message });
        refresh();
      }),
    ];
    return () => {
      offs.forEach((p) => p.then((off) => off()));
    };
  }, [showUndo, refresh]);

  const runUndo = useCallback(
    async (reportPath: string, start: () => Promise<void>) => {
      if (undoRef.current) return;
      const u: UndoRun = { reportPath, progress: { done: 0, total: 0, current: null }, cancelling: false };
      setUndoError(null);
      setUndoResult(null);
      showUndo(u);
      try {
        await start();
      } catch (e) {
        if (undoRef.current === u) showUndo(null);
        setUndoError({ reportPath, message: errorText(e) });
      }
    },
    [showUndo],
  );
  const startUndo = useCallback((r: string) => runUndo(r, () => historyApi.startUndo(r)), [runUndo]);
  const startUndoFiles = useCallback(
    (r: string, sources: string[]) => runUndo(r, () => historyApi.undoFiles(r, sources)),
    [runUndo],
  );
  const startUndoTarget = useCallback(
    (r: string, target: string) => runUndo(r, () => historyApi.undoTarget(r, target)),
    [runUndo],
  );

  const cancelUndo = useCallback(() => {
    setUndo((u) => u && { ...u, cancelling: true });
    historyApi.cancelUndo().catch(() => {});
  }, []);

  const retryingRef = useRef(false);
  const startRetry = useCallback(
    async (reportPath: string) => {
      if (retryingRef.current) return;
      retryingRef.current = true;
      setRetrying(reportPath);
      setRetryError(null);
      setRetryResult(null);
      try {
        setRetryResult(await historyApi.startRetry(reportPath));
      } catch (e) {
        setRetryError({ reportPath, message: errorText(e) });
      } finally {
        retryingRef.current = false;
        setRetrying(null);
        refresh();
      }
    },
    [refresh],
  );

  return {
    undo,
    undoResult,
    undoError,
    startUndo,
    startUndoFiles,
    startUndoTarget,
    cancelUndo,
    retrying,
    retryResult,
    retryError,
    startRetry,
    tick,
    refresh,
  };
}
