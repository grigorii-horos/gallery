// What Gallery holds paused in Syncthing: read at start (the state file survives
// a restart or a crash) and again whenever a run ends.
import { useCallback, useEffect, useState } from "react";
import { errorText, syncApi, type PauseState } from "../api";

export interface SyncPauseState {
  paused: PauseState | null;
  resuming: boolean;
  error: string | null;
  resume: () => Promise<void>;
}

async function listenSafe(name: string, cb: () => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen(name, cb);
  } catch {
    return () => {};
  }
}

export function useSyncPause(): SyncPauseState {
  const [paused, setPaused] = useState<PauseState | null>(null);
  const [resuming, setResuming] = useState(false);
  const [resumeError, setResumeError] = useState<string | null>(null);
  // The state file could not be read: never shown as "nothing paused".
  const [loadError, setLoadError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    syncApi.paused().then(
      (p) => {
        setPaused(p);
        setLoadError(null);
      },
      (e) => setLoadError(`Could not tell whether Syncthing is paused: ${errorText(e)}`),
    );
  }, []);

  useEffect(() => {
    refresh();
    const offs = [listenSafe("apply-done", refresh), listenSafe("apply-error", refresh), listenSafe("sync-changed", refresh)];
    return () => {
      offs.forEach((p) => p.then((off) => off()));
    };
  }, [refresh]);

  const resume = useCallback(async () => {
    setResuming(true);
    setResumeError(null);
    try {
      const out = await syncApi.resume();
      if (out.failed.length > 0) setResumeError(`Could not resume: ${out.failed.join(", ")}`);
    } catch (e) {
      setResumeError(errorText(e));
    } finally {
      setResuming(false);
      refresh();
    }
  }, [refresh]);

  return { paused, resuming, error: resumeError ?? loadError, resume };
}
