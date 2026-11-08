import { useEffect, useState } from "react";
import { planApi, type AiEstimate } from "../api";
import { SCAN_STAGES, formatCost, stageCount, stageFraction } from "../plan/model";
import type { PlanState } from "../plan/usePlan";
import { aiAskText } from "./model";

interface Props {
  plan: PlanState;
  configPath: string | null;
  /** The config reads and no other operation holds the slot. */
  canScan: boolean;
}

/// "Ask AI": the one place that pays. Shown while files wait for an AI
/// answer. The price comes from the core before anything is sent; above $1
/// the click asks once more. While the scan runs: its stages and "Cancel".
export function AiAsk({ plan, configPath, canScan }: Props) {
  const { scan, view, lastScan } = plan;
  const waiting = view?.waiting_ai ?? 0;
  const version = view?.built_at ?? null;
  const [est, setEst] = useState<AiEstimate | null>(null);
  const [confirm, setConfirm] = useState(false);

  // Read again whenever the plan was rebuilt (the cache may have grown).
  useEffect(() => {
    if (waiting === 0) return;
    let live = true;
    planApi.estimateAiScan(configPath).then(
      (e) => live && setEst(e),
      () => live && setEst(null),
    );
    return () => {
      live = false;
    };
  }, [configPath, waiting, version]);

  const ask = () => {
    if (est?.confirm && !confirm) {
      setConfirm(true);
      return;
    }
    setConfirm(false);
    void plan.startScan(configPath);
  };

  if (waiting === 0 && !scan && !lastScan?.summary.ai) return null;
  return (
    <div className="scanbar ai-ask">
      {waiting > 0 && !scan && (
        <div className="scanbar-row">
          <button type="button" className="btn primary" disabled={!canScan || confirm} onClick={ask}>
            {aiAskText(waiting, est)}
          </button>
          <span className="muted small">
            Without AI these files do not make it into the plan. Only files without a cached answer cost money.
          </span>
        </div>
      )}
      {confirm && est && (
        <div className="notice warn" role="alertdialog" aria-label="Confirm AI cost">
          <p className="notice-title">This AI request will cost more than $1</p>
          <p className="notice-text">
            {est.label} · {est.source_label}. This is an estimate: no API calls, based on the number of files without a cached answer.
          </p>
          <div className="run-actions">
            <button type="button" className="btn primary" onClick={ask}>
              Ask AI
            </button>
            <button type="button" className="btn" onClick={() => setConfirm(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {lastScan?.summary.ai && !scan && <p className="muted small cost">Previous AI request: {formatCost(lastScan.summary.ai)}</p>}
      {scan && (
        <div className="scan-progress" role="status" aria-label="AI request progress">
          <ol className="stages">
            {SCAN_STAGES.map((st) => {
              const s = scan.progress[st.id];
              const active = scan.stage === st.id;
              const frac = stageFraction(s);
              const state = active ? "active" : s ? "done" : "wait";
              return (
                <li key={st.id} className={`stage-step ${state}`}>
                  <span className="stage-dot" aria-hidden />
                  <span className="stage-label">{st.label}</span>
                  <span className="stage-count">{stageCount(s)}</span>
                  <span className="stage-bar" aria-hidden>
                    <span
                      className={frac === null && active ? "indeterminate" : undefined}
                      style={{ width: `${(frac ?? (s && !active ? 1 : 0)) * 100}%` }}
                    />
                  </span>
                </li>
              );
            })}
          </ol>
          <div className="scan-current">
            <span className="path" title={scan.current ?? ""}>
              <bdi>{scan.current ?? "Reading config…"}</bdi>
            </span>
            <button type="button" className="btn" disabled={scan.cancelling} onClick={plan.cancelScan}>
              {scan.cancelling ? "Stopping…" : "Cancel"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
