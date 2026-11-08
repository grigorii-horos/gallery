// "Run" state: the current plan's preview, a running apply (kept in App
// so it keeps reporting on other screens) and the server export.
import { t } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { PILOT_PER_TARGET } from "./model";
import {
  errorText,
  runApi,
  type ApplyDone,
  type ApplyError,
  type ApplyPreview,
  type ApplyProgress,
  type ApplyRequest,
  type ExportDone,
  type PilotResult,
  type PilotSplit,
  type ScanProgress,
} from "../api";

const EXPORT_DIR_KEY = "gallery.exportDir";

/** What a run of the current plan names: the plan (and pilot slice) the user confirmed. */
export interface CurrentStart {
  dryRun: boolean;
  planId: string;
  allowAnomalies?: boolean;
  pauseSync?: boolean;
  pilot?: number | null;
  pilotId?: string | null;
}

export interface ApplyRun {
  request: ApplyRequest;
  progress: ApplyProgress;
  cancelling: boolean;
}

export interface ExportRun {
  progress: ScanProgress | null;
  cancelling: boolean;
}

/** A pilot that ran and waits for the user's decision. */
export interface PilotRun {
  split: PilotSplit;
  /** Each pilot file before and after; null while it loads. */
  result: PilotResult | null;
  resultError: string | null;
}

export interface RunState {
  preview: ApplyPreview | null;
  previewError: string | null;
  run: ApplyRun | null;
  result: ApplyDone | null;
  error: string | null;
  /** How many real runs ended in this session (not cancelled): steps start over. */
  finished: number;
  /** Run (or dry-run) the current plan; the backend checks it is fresh first. */
  startCurrent: (o: CurrentStart) => Promise<void>;
  /** Run a plan file (a retry plan from History). */
  start: (planPath: string, dryRun: boolean, allowAnomalies?: boolean, pauseSync?: boolean) => Promise<void>;
  /** The plan is being split for a pilot. */
  splitting: boolean;
  pilot: PilotRun | null;
  /** How the plan splits for a pilot (nothing runs); the split is then confirmed and run with `runPilot`. */
  splitPilot: () => Promise<PilotSplit | null>;
  runPilot: (split: PilotSplit, allowAnomalies: boolean, pauseSync?: boolean) => Promise<void>;
  /** What running the plan now would do (after a pilot it is the rest, rebuilt). */
  previewRest: () => Promise<ApplyPreview | null>;
  runRest: (planId: string, allowAnomalies: boolean, pauseSync?: boolean) => Promise<void>;
  cancel: () => void;
  exportDir: string | null;
  setExportDir: (dir: string) => void;
  exporting: ExportRun | null;
  exported: ExportDone | null;
  exportError: string | null;
  startExport: (configPath: string | null) => Promise<void>;
  cancelExport: () => void;
}

async function listenSafe<T>(name: string, cb: (payload: T) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<T>(name, (e) => cb(e.payload));
  } catch {
    return () => {};
  }
}

const emptyProgress = (): ApplyProgress => ({ done: 0, total: 0, current: null, failed: 0, skipped: 0 });

/** `planPath`: the current plan, previewed for the confirmation again whenever it is rebuilt (`version`). */
export function useRun(planPath: string | null, configPath: string | null = null, version: string | null = null): RunState {
  const [preview, setPreview] = useState<ApplyPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [run, setRun] = useState<ApplyRun | null>(null);
  const [result, setResult] = useState<ApplyDone | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [finished, setFinished] = useState(0);
  const [exportDir, setDir] = useState<string | null>(() => localStorage.getItem(EXPORT_DIR_KEY));
  const [exporting, setExporting] = useState<ExportRun | null>(null);
  const [exported, setExported] = useState<ExportDone | null>(null);
  const [exportError, setExportError] = useState<string | null>(null);
  const [splitting, setSplitting] = useState(false);
  const [pilot, setPilot] = useState<PilotRun | null>(null);
  const pilotRef = useRef<PilotRun | null>(null);
  // The split whose pilot plan is running now.
  const pendingSplit = useRef<PilotSplit | null>(null);
  const showPilot = useCallback((p: PilotRun | null) => {
    pilotRef.current = p;
    setPilot(p);
  }, []);
  const runRef = useRef<ApplyRun | null>(null);
  const showRun = useCallback((r: ApplyRun | null) => {
    runRef.current = r;
    setRun(r);
  }, []);

  // Bumped when a real run ends or is undone.
  const [previewTick, setPreviewTick] = useState(0);

  useEffect(() => {
    let live = true;
    if (!planPath) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPreview(null);
      return;
    }
    runApi.applyPreview(planPath).then(
      (p) => {
        if (!live) return;
        setPreview(p);
        setPreviewError(null);
      },
      (e) => {
        if (!live) return;
        setPreview(null);
        setPreviewError(errorText(e));
      },
    );
    return () => {
      live = false;
    };
  }, [planPath, version, previewTick]);

  // An apply still running from before a reload.
  useEffect(() => {
    runApi
      .applyStatus()
      .then((st) => {
        if (st.running && !runRef.current) {
          showRun({ request: st.running.request, progress: st.running.progress, cancelling: false });
        } else if (st.last_done && !runRef.current) {
          // The run ended while the webview was away: show its result.
          setResult(st.last_done);
        }
      })
      .catch(() => {});
  }, [showRun]);

  useEffect(() => {
    const offs = [
      listenSafe<ApplyProgress>("apply-progress", (p) => setRun((r) => r && { ...r, progress: p })),
      listenSafe<ApplyDone>("apply-done", (d) => {
        showRun(null);
        setResult(d);
        const split = pendingSplit.current;
        if (split && !d.dry_run) {
          // The pilot ran: show each file, then wait for the decision.
          pendingSplit.current = null;
          const p: PilotRun = { split, result: null, resultError: null };
          showPilot(p);
          if (d.report_path) {
            runApi.pilotResult(d.report_path).then(
              (r) => showPilot({ ...p, result: r }),
              (e) => showPilot({ ...p, resultError: errorText(e) }),
            );
          } else {
            showPilot({ ...p, resultError: t("pilot_no_report") });
          }
        }
        if (!d.dry_run && !d.summary.cancelled) setFinished((n) => n + 1);
        if (!d.dry_run) setPreviewTick((t) => t + 1);
      }),
      listenSafe<ApplyError>("apply-error", (e) => {
        showRun(null);
        pendingSplit.current = null;
        setError(e.message);
      }),
      // An undo changes what the plan's report says (`undone`); an undone
      // pilot is over.
      listenSafe<{ report_path: string; undone: boolean }>("undo-done", (u) => {
        setPreviewTick((t) => t + 1);
        const open = pilotRef.current;
        if (open?.result && u.undone && u.report_path === open.result.report_path) showPilot(null);
      }),
      listenSafe<ScanProgress>("export-progress", (p) => setExporting((x) => x && { ...x, progress: p })),
    ];
    return () => {
      offs.forEach((p) => p.then((off) => off()));
    };
  }, [showRun, showPilot]);

  const start = useCallback(
    async (path: string, dryRun: boolean, allowAnomalies = false, pauseSync = false) => {
      if (runRef.current) return;
      pendingSplit.current = null;
      const request: ApplyRequest = { plan_path: path, dry_run: dryRun };
      if (allowAnomalies) request.allow_anomalies = true;
      if (pauseSync) request.pause_sync = true;
      const r: ApplyRun = { request, progress: emptyProgress(), cancelling: false };
      setError(null);
      setResult(null);
      showRun(r);
      try {
        await runApi.startApply(request);
      } catch (e) {
        if (runRef.current === r) showRun(null);
        setError(errorText(e));
      }
    },
    [showRun],
  );

  const startCurrent = useCallback(
    async ({ dryRun, planId, allowAnomalies, pauseSync, pilot: pilotFiles, pilotId }: CurrentStart) => {
      if (runRef.current) return;
      const request: ApplyRequest = { plan_path: planPath ?? "", dry_run: dryRun };
      const r: ApplyRun = { request, progress: emptyProgress(), cancelling: false };
      setError(null);
      setResult(null);
      showRun(r);
      try {
        await runApi.startApplyCurrent(configPath, {
          dry_run: dryRun,
          plan_id: planId,
          ...(pilotId ? { pilot_id: pilotId } : {}),
          ...(allowAnomalies ? { allow_anomalies: true } : {}),
          ...(pauseSync ? { pause_sync: true } : {}),
          ...(pilotFiles ? { pilot: pilotFiles } : {}),
        });
      } catch (e) {
        if (runRef.current === r) showRun(null);
        pendingSplit.current = null;
        setError(errorText(e));
      }
    },
    [planPath, configPath, showRun],
  );

  const splitPilot = useCallback(async () => {
    if (runRef.current || splitting || !planPath) return null;
    setError(null);
    setResult(null);
    setSplitting(true);
    try {
      return await runApi.pilotPreview(planPath, PILOT_PER_TARGET);
    } catch (e) {
      setError(errorText(e));
      return null;
    } finally {
      setSplitting(false);
    }
  }, [splitting, planPath]);

  const runPilot = useCallback(
    async (split: PilotSplit, allowAnomalies: boolean, pauseSync = false) => {
      pendingSplit.current = split;
      await startCurrent({
        dryRun: false,
        planId: split.plan_id,
        pilotId: split.pilot_id,
        pilot: split.per_target,
        allowAnomalies,
        pauseSync,
      });
    },
    [startCurrent],
  );

  const previewRest = useCallback(async () => {
    if (!planPath) return null;
    try {
      return await runApi.applyPreview(planPath);
    } catch (e) {
      setError(errorText(e));
      return null;
    }
  }, [planPath]);

  const runRest = useCallback(
    async (planId: string, allowAnomalies: boolean, pauseSync = false) => {
      showPilot(null);
      await startCurrent({ dryRun: false, planId, allowAnomalies, pauseSync });
    },
    [startCurrent, showPilot],
  );

  const cancel = useCallback(() => {
    setRun((r) => r && { ...r, cancelling: true });
    runApi.cancelApply().catch(() => {});
  }, []);

  const setExportDir = useCallback((dir: string) => {
    localStorage.setItem(EXPORT_DIR_KEY, dir);
    setDir(dir);
  }, []);

  const exportingRef = useRef(false);
  const startExport = useCallback(
    async (configPath: string | null) => {
      if (!exportDir || exportingRef.current) return;
      exportingRef.current = true;
      setExportError(null);
      setExported(null);
      setExporting({ progress: null, cancelling: false });
      try {
        setExported(
          await runApi.exportServerPlan(configPath, { dest_dir: exportDir }),
        );
      } catch (e) {
        setExportError(errorText(e));
      } finally {
        exportingRef.current = false;
        setExporting(null);
      }
    },
    [exportDir],
  );

  const cancelExport = useCallback(() => {
    setExporting((x) => x && { ...x, cancelling: true });
    runApi.cancelExport().catch(() => {});
  }, []);

  return {
    preview: planPath ? preview : null,
    previewError,
    run,
    result,
    error,
    finished,
    startCurrent,
    start,
    splitting,
    pilot,
    splitPilot,
    runPilot,
    previewRest,
    runRest,
    cancel,
    exportDir,
    setExportDir,
    exporting,
    exported,
    exportError,
    startExport,
    cancelExport,
  };
}
