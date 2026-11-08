// The current plan and what happens to it: it is rebuilt by the backend
// (no AI) whenever the config, the manual decisions or the files change, and
// this hook asks for that — on start, on window focus, after every decision —
// debounced. A real AI scan is the only explicit scan. Lives in App so a scan
// keeps reporting while another screen is open.
import { useCallback, useEffect, useRef, useState } from "react";
import {
  errorText,
  planApi,
  type CurrentView,
  type OverrideAction,
  type OverrideResult,
  type OverrideView,
  type ScanDone,
  type ScanError,
  type ScanProgress,
  type ScanRequest,
  type ScanStage,
} from "../api";
import type { StageProgress } from "./model";

const LAST_SCAN_KEY = "gallery.lastScan";
/** Waits for the editing to settle before the plan is rebuilt. */
export const REFRESH_DEBOUNCE_MS = 400;
const AI_REQUEST: ScanRequest = {};

export interface ScanRun {
  request: ScanRequest;
  stage: ScanStage | null;
  progress: StageProgress;
  current: string | null;
  cancelling: boolean;
}

export interface PlanState {
  view: CurrentView | null;
  loading: boolean;
  loadError: string | null;
  /** The backend is bringing the plan up to date. */
  updating: boolean;
  /** Why the last update failed (a config that does not read, …). */
  updateError: string | null;
  /** The running AI scan. */
  scan: ScanRun | null;
  /** Cost and notes of the last AI scan. */
  lastScan: ScanDone | null;
  scanError: string | null;
  /** "Scan cancelled" and the like: not an error. */
  scanNotice: string | null;
  /** Ask the backend to bring the plan up to date (debounced); it does nothing when nothing changed. */
  refresh: () => void;
  /** The AI scan: the only paid action. */
  startScan: (configPath: string | null) => Promise<void>;
  cancelScan: () => void;
  setOverride: (source: string, action: OverrideAction) => Promise<void>;
  /** One decision for several files in one write; results per file, in order. */
  setOverrides: (sources: string[], action: OverrideAction) => Promise<OverrideResult[]>;
  removeOverride: (source: string) => Promise<void>;
}

function readJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

/** Subscribe to a Tauri event; outside Tauri (tests) nothing happens. */
async function listenSafe<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<T>(name, (e) => cb(e.payload));
  } catch {
    return () => {};
  }
}

const newRun = (request: ScanRequest): ScanRun => ({
  request,
  stage: null,
  progress: {},
  current: null,
  cancelling: false,
});

/** `configPath`: the UI's config, which the current plan belongs to and manual decisions are checked against. */
export function usePlan(configPath: string | null = null): PlanState {
  const [view, setView] = useState<CurrentView | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [updating, setUpdating] = useState(false);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [scan, setScan] = useState<ScanRun | null>(null);
  const [lastScan, setLastScan] = useState<ScanDone | null>(() => readJson(LAST_SCAN_KEY, null));
  const [scanError, setScanError] = useState<string | null>(null);
  const [scanNotice, setScanNotice] = useState<string | null>(null);
  // Set synchronously with `setScan`, so a second click in the same tick
  // sees the first scan.
  const scanRef = useRef<ScanRun | null>(scan);
  const showScan = useCallback((run: ScanRun | null) => {
    scanRef.current = run;
    setScan(run);
  }, []);
  const config = useRef(configPath);
  useEffect(() => {
    config.current = configPath;
  }, [configPath]);

  // Only sets state when the answer comes.
  const fetchPlan = useCallback((path: string | null) => {
    planApi.currentPlan(path).then(
      (v) => {
        if (config.current !== path) return;
        setView(v);
        setLoadError(null);
        setLoading(false);
      },
      (e) => {
        setLoadError(errorText(e));
        setLoading(false);
      },
    );
  }, []);

  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const refresh = useCallback(() => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      timer.current = null;
      planApi.refreshCurrent(config.current).catch(() => {});
    }, REFRESH_DEBOUNCE_MS);
  }, []);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  // The stored plan on start and for another config, then bring it up to date.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    setView(null);
    fetchPlan(configPath);
    refresh();
    // A scan still running from before a reload.
    planApi
      .scanStatus()
      .then((req) => {
        if (req && !scanRef.current) showScan(newRun(req));
      })
      .catch(() => {});
  }, [configPath, fetchPlan, refresh, showScan]);

  // The window comes back to the front: files may have changed meanwhile.
  useEffect(() => {
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh]);

  useEffect(() => {
    const offs = [
      listenSafe<null>("current-rebuilding", () => setUpdating(true)),
      listenSafe<null>("current-updated", () => {
        setUpdating(false);
        setUpdateError(null);
        fetchPlan(config.current);
      }),
      listenSafe<string>("current-error", (m) => {
        setUpdating(false);
        setUpdateError(m);
      }),
      listenSafe<ScanProgress>("scan-progress", (p) =>
        setScan((s) =>
          s && {
            ...s,
            stage: p.stage,
            current: p.current,
            progress: { ...s.progress, [p.stage]: { done: p.done, total: p.total } },
          },
        ),
      ),
      listenSafe<ScanDone>("scan-done", (d) => {
        showScan(null);
        setLastScan(d);
        localStorage.setItem(LAST_SCAN_KEY, JSON.stringify(d));
        fetchPlan(config.current);
      }),
      listenSafe<ScanError>("scan-error", (e) => {
        showScan(null);
        if (e.cancelled) setScanNotice(e.message);
        else setScanError(e.message);
      }),
    ];
    return () => {
      offs.forEach((p) => p.then((off) => off()));
    };
  }, [fetchPlan, showScan]);

  const startScan = useCallback(
    async (path: string | null) => {
      if (scanRef.current) return;
      const run = newRun(AI_REQUEST);
      setScanError(null);
      setScanNotice(null);
      showScan(run);
      try {
        await planApi.startScan(path);
      } catch (e) {
        if (scanRef.current !== run) return;
        // "A scan is already running" (another window, a lost reload): show that scan.
        showScan(null);
        const req = await planApi.scanStatus().catch(() => null);
        if (req) showScan(newRun(req));
        else setScanError(errorText(e));
      }
    },
    [showScan],
  );

  const cancelScan = useCallback(() => {
    setScan((s) => s && { ...s, cancelling: true });
    planApi.cancelScan().catch(() => {});
  }, []);

  const setOverride = useCallback(
    async (source: string, action: OverrideAction) => {
      const res = await planApi.setOverride(configPath, source, action);
      const key = res.key ?? source;
      const stamp = { size: res.size ?? 0, mtime_ns: res.mtime_ns ?? 0 };
      // Keep the local list in step so the trace shows the stored decision.
      setView((v) =>
        v && {
          ...v,
          overrides: [...v.overrides.filter((o) => o.path !== key), { path: key, action, ...stamp }],
        },
      );
      refresh();
    },
    [configPath, refresh],
  );

  const setOverrides = useCallback(
    async (sources: string[], action: OverrideAction) => {
      const results = await planApi.setOverrides(
        configPath,
        sources.map((source_path) => ({ source_path, action })),
      );
      const accepted = new Map<string, OverrideView>();
      for (const r of results) {
        if (r.key) accepted.set(r.key, { path: r.key, action, size: r.size ?? 0, mtime_ns: r.mtime_ns ?? 0 });
      }
      if (accepted.size > 0) {
        setView(
          (v) =>
            v && {
              ...v,
              overrides: [
                ...v.overrides.filter((o) => !accepted.has(o.path)),
                ...accepted.values(),
              ],
            },
        );
        refresh();
      }
      return results;
    },
    [configPath, refresh],
  );

  const removeOverride = useCallback(
    async (source: string) => {
      if (await planApi.removeOverride(source)) {
        setView((v) => v && { ...v, overrides: v.overrides.filter((o) => o.path !== source) });
        refresh();
      }
    },
    [refresh],
  );

  return {
    view,
    loading,
    loadError,
    updating,
    updateError,
    scan,
    lastScan,
    scanError,
    scanNotice,
    refresh,
    startScan,
    cancelScan,
    setOverride,
    setOverrides,
    removeOverride,
  };
}
