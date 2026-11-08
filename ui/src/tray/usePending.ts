// How much work waits: the background check's last result, kept fresh by its
// event, and the tray's "Open" (with work waiting) asking for the Sort screen.
import { useCallback, useEffect, useState } from "react";
import { errorText, trayApi, type PendingSnapshot } from "../api";

export interface PendingState {
  snapshot: PendingSnapshot | null;
  checking: boolean;
  error: string | null;
  checkNow: () => Promise<void>;
}

async function listenSafe<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<T>(name, (e) => cb(e.payload));
  } catch {
    return () => {};
  }
}

export function usePending(configPath: string | null, onOpenPlan: () => void): PendingState {
  const [snapshot, setSnapshot] = useState<PendingSnapshot | null>(null);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    trayApi.pendingStatus().then(setSnapshot, () => {});
    const offs = [listenSafe<PendingSnapshot>("pending-updated", setSnapshot), listenSafe<null>("open-plan", onOpenPlan)];
    return () => {
      offs.forEach((p) => p.then((off) => off()));
    };
  }, [onOpenPlan]);

  const checkNow = useCallback(async () => {
    setChecking(true);
    setError(null);
    try {
      setSnapshot(await trayApi.checkPending(configPath));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setChecking(false);
    }
  }, [configPath]);

  // No check of its own on start: keeping the current plan up to date (which
  // also follows the config the UI works with) publishes the count.

  return { snapshot, checking, error, checkNow };
}
