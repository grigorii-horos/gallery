import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import { errorText, pickFolder, runApi, syncApi, type ApplyPreview, type ExportDone, type PilotSplit, type SyncCheck } from "../api";
import { formatCost, SCAN_STAGES, stageCount, stageFraction } from "../plan/model";
import { SyncBanner } from "../components/SyncBanner";
import type { SyncPauseState } from "../sync/useSyncPause";
import type { PlanState } from "../plan/usePlan";
import type { AppData } from "../store";
import {
  applyBlock,
  confirmLines,
  fates,
  filesWord,
  PILOT_PER_TARGET,
  progressFraction,
  type ConfirmKind,
  resultHeadline,
  resultRows,
  syncOption,
  type Fate,
} from "../run/model";
import type { RunState } from "../run/useRun";
import { EstimateBlock } from "../run/EstimateBlock";
import { PilotPanel } from "./PilotPanel";

interface Props {
  plan: PlanState;
  run: RunState;
  sync: SyncPauseState;
  /** Another operation that holds the slot (AI scan, export, undo, retry), or null. */
  busy: string | null;
  onOpenHistory: () => void;
  /** Undo the run of the report at this path (the pilot). */
  onUndoPilot?: (reportPath: string) => void;
}

type Mode = "pilot" | "all";

/// "Run": how much to run (a pilot by default), the estimate, the
/// confirmation, the progress and the result with before → after.
export function RunStep({ plan, run, sync, busy, onOpenHistory, onUndoPilot }: Props) {
  const { t } = useTranslation();
  // The confirmation that is open: the whole plan, the pilot, or what the pilot left.
  // `planId`: the plan the dialog was opened for; once the plan is another one, the dialog is void.
  const [dialog, setDialog] = useState<{ kind: ConfirmKind; preview: ApplyPreview; planId: string; split?: PilotSplit } | null>(null);
  const planId = plan.view?.plan_id ?? null;
  const outdated = !!dialog && dialog.planId !== planId;
  const [mode, setMode] = useState<Mode>("pilot");
  const p = run.preview;
  // The counts are of the plan file read for the preview; until that is the plan the
  // screen shows, a real run would confirm numbers of another plan.
  const previewStale = !!p && !!planId && p.plan_id !== planId;
  const input = {
    preview: p,
    busy,
    updating: plan.updating,
    pilotOpen: !!run.pilot,
  };
  const block = run.run ? null : applyBlock(input);
  const dryBlock = run.run ? null : applyBlock(input, true);

  return (
    <div className="run-card">
      {run.previewError ? (
        <div className="notice bad">
          <p className="notice-title">{t("run_plan_unreadable")}</p>
          <pre className="notice-text">{run.previewError}</pre>
        </div>
      ) : p ? (
        <>
          <p className="run-plan">
            <span className="muted">{t("run_to_process", { count: p.files })}</span>
          </p>
          <FateLedger preview={p} />
          {!run.run && p.files > 0 && (
            <EstimateBlock planPath={p.plan_path} pilot={mode === "pilot" ? PILOT_PER_TARGET : undefined} />
          )}
        </>
      ) : (
        <p className="empty small">{plan.loading ? t("flow_opening_short") : t("run_no_plan")}</p>
      )}

      {run.run ? (
        <Progress run={run} />
      ) : (
        <>
          {(dryBlock ?? block) && p && (
            <p className="run-block">
              {(dryBlock ?? block)!.text}
              {(dryBlock ?? block)!.history && (
                <>
                  {" "}
                  <button type="button" className="link" onClick={onOpenHistory}>
                    {t("run_open_history")}
                  </button>
                </>
              )}
            </p>
          )}
          <fieldset className="run-mode">
            <legend className="muted small">{t("run_how_much")}</legend>
            <label className="check">
              <input type="radio" name="run-mode" checked={mode === "pilot"} onChange={() => setMode("pilot")} />
              {t("run_mode_pilot", { count: PILOT_PER_TARGET })}
            </label>
            <label className="check">
              <input type="radio" name="run-mode" checked={mode === "all"} onChange={() => setMode("all")} />
              {t("run_mode_all")}
            </label>
          </fieldset>
          <div className="run-actions">
            <button
              type="button"
              className="btn"
              disabled={!!dryBlock}
              title={t("run_check_title")}
              onClick={() => planId && void run.startCurrent({ dryRun: true, planId })}
            >
              {t("run_check")}
            </button>
            <button
              type="button"
              className="btn primary"
              disabled={!!block || run.splitting || (mode === "all" && previewStale)}
              title={
                mode === "pilot" ? t("run_pilot_title") : t("run_all_title")
              }
              onClick={async () => {
                if (mode === "all") {
                  if (p) setDialog({ kind: "all", preview: p, planId: p.plan_id });
                  return;
                }
                const split = await run.splitPilot();
                if (split) setDialog({ kind: "pilot", preview: split.preview, planId: split.plan_id, split });
              }}
            >
              {mode === "pilot" ? t("run_pilot_button", { count: PILOT_PER_TARGET }) : t("run_all_button")}
            </button>
          </div>
        </>
      )}
      {run.pilot && !run.run && (
        <PilotPanel
          pilot={run.pilot}
          planPath={p?.plan_path ?? null}
          busy={busy}
          onContinue={async () => {
            const preview = await run.previewRest();
            if (preview) setDialog({ kind: "rest", preview, planId: preview.plan_id });
          }}
          onRollback={() => run.pilot?.result && onUndoPilot?.(run.pilot.result.report_path)}
        />
      )}

      {run.error && (
        <div className="notice bad">
          <p className="notice-title">{t("run_not_run")}</p>
          <pre className="notice-text">{run.error}</pre>
        </div>
      )}
      {run.result && !run.run && <Result run={run} onOpenHistory={onOpenHistory} />}
      <SyncBanner sync={sync} />

      {outdated && (
        <div className="notice warn" role="alert">
          <p className="notice-title">{t("run_refreshed")}</p>
          <p className="notice-text">{t("run_refreshed_note")}</p>
          <button type="button" className="btn" onClick={() => setDialog(null)}>
            {t("got_it")}
          </button>
        </div>
      )}
      {dialog && !outdated && (
        <ConfirmDialog
          preview={dialog.preview}
          kind={dialog.kind}
          pilot={dialog.kind === "pilot" ? PILOT_PER_TARGET : undefined}
          onCancel={() => setDialog(null)}
          onConfirm={(allow, pause) => {
            const d = dialog;
            setDialog(null);
            if (d.kind === "pilot" && d.split) void run.runPilot(d.split, allow, pause);
            else if (d.kind === "rest") void run.runRest(d.planId, allow, pause);
            else void run.startCurrent({ dryRun: false, planId: d.planId, allowAnomalies: allow, pauseSync: pause });
          }}
        />
      )}
    </div>
  );
}

/// "Retry of failed files": the retry plan History made (a plan file of its
/// own, not the current plan), run after the same confirmation as any run.
export function RetryCard({
  path,
  run,
  busy,
  onDone,
}: {
  path: string;
  run: RunState;
  busy: string | null;
  onDone?: () => void;
}) {
  const { t } = useTranslation();
  const [preview, setPreview] = useState<ApplyPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState(false);
  useEffect(() => {
    let live = true;
    runApi.applyPreview(path).then(
      (v) => live && setPreview(v),
      (e) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [path]);
  return (
    <section className="retry-card" aria-label={t("retry_aria")}>
      <h3 className="estimate-title">{t("retry_aria")}</h3>
      {error && <pre className="notice-text">{error}</pre>}
      {preview && (
        <>
          <p className="muted small">{t("retry_from_previous", { files: filesWord(preview.files) })}</p>
          <div className="run-actions">
            <button type="button" className="btn primary" disabled={!!busy} onClick={() => setDialog(true)}>
              {t("retry_run")}
            </button>
            <button type="button" className="btn" onClick={onDone}>
              {t("retry_not_needed")}
            </button>
          </div>
        </>
      )}
      {dialog && preview && (
        <ConfirmDialog
          preview={preview}
          kind="all"
          onCancel={() => setDialog(false)}
          onConfirm={(allow, pause) => {
            setDialog(false);
            void run.start(path, false, allow, pause).then(onDone);
          }}
        />
      )}
    </section>
  );
}

/// Where the sources end up: one bar split by fate, then the counts.
function FateLedger({ preview, note = true }: { preview: ApplyPreview; note?: boolean }) {
  const { t } = useTranslation();
  const rows = fates(preview);
  const total = Math.max(preview.files, 1);
  const outside = preview.review + preview.unmatched + preview.unchanged;
  return (
    <div className="fate">
      <div className="fate-bar" aria-hidden>
        {rows.map((f) =>
          f.count > 0 ? <span key={f.id} className={`fate-${f.id}`} style={{ flexGrow: f.count / total }} /> : null,
        )}
      </div>
      <ul className="fate-rows" aria-label={t("fate_aria")}>
        {rows.map((f: Fate) => (
          <li key={f.id} className={f.count === 0 ? "zero" : undefined}>
            <span className={`fate-swatch fate-${f.id}`} aria-hidden />
            <span className="fate-count">{num(f.count)}</span>
            <span>{f.label}</span>
          </li>
        ))}
      </ul>
      {note && (
        <p className="muted small">
          {outside > 0
            ? t("fate_note_untouched", { writes: preview.writes, outside })
            : t("fate_note", { writes: preview.writes, outside })}
        </p>
      )}
    </div>
  );
}

function Progress({ run }: { run: RunState }) {
  const { t } = useTranslation();
  const r = run.run!;
  const p = r.progress;
  const frac = progressFraction(p);
  const what = r.request.dry_run ? t("run_what_check") : t("run_what_run");
  return (
    <div className="run-progress" role="status" aria-label={t("run_progress_aria", { what })}>
      <div className="run-progress-head">
        <span>
          {t("run_progress", { what, done: p.done, total: p.total })}
        </span>
        <span className={p.failed > 0 ? "bad-text" : "muted"}>{t("run_errors", { n: p.failed })}</span>
        {p.skipped > 0 && <span className="warn-text">{t("run_skipped", { n: p.skipped })}</span>}
      </div>
      <div className="run-bar" aria-hidden>
        <span style={{ width: `${frac * 100}%` }} />
      </div>
      <div className="scan-current">
        <span className="path" title={p.current ?? ""}>
          <bdi>{p.current ?? t("getting_ready")}</bdi>
        </span>
        <button type="button" className="btn" disabled={r.cancelling} onClick={run.cancel}>
          {r.cancelling ? t("stopping") : t("cancel")}
        </button>
      </div>
      {r.cancelling && (
        <p className="muted small">{t("run_cancelling")}</p>
      )}
    </div>
  );
}

function Result({ run, onOpenHistory }: { run: RunState; onOpenHistory: () => void }) {
  const { t } = useTranslation();
  const d = run.result!;
  const head = resultHeadline(d);
  const rows = resultRows(d.summary, d.dry_run);
  return (
    <div className={`run-result ${head.tone}`} role="status">
      <p className="run-result-title">{head.title}</p>
      <ul className="run-result-rows">
        {rows.map((r) => (
          <li key={r.label} className={r.tone}>
            <span className="fate-count">{num(r.count)}</span> {r.label}
          </li>
        ))}
      </ul>
      {d.failures.length > 0 && (
        <details className="run-failures">
          <summary>{t("run_problems", { n: d.failures.length })}</summary>
          <ul>
            {d.failures.map((f) => (
              <li key={f.source}>
                <span className="code">{f.source}</span>
                <span className="muted">{f.reason}</span>
              </li>
            ))}
          </ul>
        </details>
      )}
      {d.warnings.map((w) => (
        <p key={w} className="muted small">
          {w}
        </p>
      ))}
      {d.report_path ? (
        <p className="run-report">
          <span className="muted">{t("history_report")}:</span> <span className="code">{d.report_path}</span>
          <button type="button" className="link" onClick={onOpenHistory}>
            {t("run_open_in_history")}
          </button>
        </p>
      ) : (
        <p className="muted small">{t("run_check_no_report")}</p>
      )}
    </div>
  );
}

/// The last word before files move: what goes where, with "Cancel" focused.
function ConfirmDialog({
  preview,
  kind,
  pilot,
  onCancel,
  onConfirm,
}: {
  preview: ApplyPreview;
  kind: ConfirmKind;
  /** Files per target when the pilot is what runs (the estimate is for that slice). */
  pilot?: number;
  onCancel: () => void;
  /** `allow`: place results that differ from their source; `pause`: pause Syncthing meanwhile. */
  onConfirm: (allow: boolean, pause: boolean) => void;
}) {
  const { t } = useTranslation();
  const { title, lines } = confirmLines(preview, kind);
  const [allow, setAllow] = useState(false);
  const [check, setCheck] = useState<SyncCheck | null>(null);
  // null: the user has not touched the checkbox, the default of `syncOption` stands.
  const [pauseChoice, setPauseChoice] = useState<boolean | null>(null);
  const sync = syncOption(check);
  const pause = !sync.disabled && (pauseChoice ?? sync.checked);
  useEffect(() => {
    let live = true;
    syncApi.check(preview.plan_path, pilot).then(
      (c) => live && setCheck(c),
      (e) => live && setCheck({ reachable: false, reason: errorText(e), folders: [] }),
    );
    return () => {
      live = false;
    };
  }, [preview.plan_path, pilot]);
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="confirm-title">
        <h2 id="confirm-title">{title}</h2>
        <FateLedger preview={preview} note={false} />
        <EstimateBlock planPath={preview.plan_path} pilot={pilot} />
        <ul className="modal-lines">
          {lines.map((l) => (
            <li key={l}>{l}</li>
          ))}
        </ul>
        <label className="modal-check">
          <input type="checkbox" checked={allow} onChange={(e) => setAllow(e.target.checked)} />
          {t("confirm_allow")}
        </label>
        <label className="modal-check">
          <input
            type="checkbox"
            checked={pause}
            disabled={sync.disabled}
            onChange={(e) => setPauseChoice(e.target.checked)}
          />
          {t("confirm_pause")}
        </label>
        <p className="muted small modal-note">{sync.note}</p>
        <div className="modal-actions">
          <button ref={cancelRef} type="button" className="btn" onClick={onCancel}>
            {t("cancel")}
          </button>
          <button type="button" className="btn primary" onClick={() => onConfirm(allow, pause)}>
            {t("confirm_run")}
          </button>
        </div>
      </div>
    </div>
  );
}

// ---------- server ----------

/// "For server": the current plan with the server's paths, saved to the export folder.
export function ServerCard({ data, plan, run, busy }: { data: AppData; plan: PlanState; run: RunState; busy: string | null }) {
  const { t } = useTranslation();
  const x = run.exporting;
  const canExport = !!data.summary?.valid && !!run.exportDir && !x && !run.run && !plan.scan && !busy;
  const choose = async () => {
    const dir = await pickFolder(run.exportDir);
    if (dir) run.setExportDir(dir);
  };
  return (
    <section className="run-card" aria-labelledby="run-server">
      <header className="run-card-head">
        <h2 id="run-server">{t("server_title")}</h2>
        <span className="muted">{t("flow_export_plan")}</span>
      </header>
      <p className="run-text">
        {t("server_text")}
      </p>
      <div className="run-folder">
        {run.exportDir ? (
          <span className="path" title={run.exportDir}>
            <bdi>{run.exportDir}</bdi>
          </span>
        ) : (
          <span className="run-folder-empty muted">{t("server_no_folder")}</span>
        )}
        <button type="button" className="btn" disabled={!!x} onClick={choose}>
          {t("server_choose")}
        </button>
      </div>
      <div className="run-actions">
        <button
          type="button"
          className="btn primary"
          disabled={!canExport}
          onClick={() => run.startExport(data.configPath)}
        >
          {t("server_export")}
        </button>
      </div>
      {x && (
        <div className="run-progress" role="status" aria-label={t("server_progress_aria")}>
          <div className="run-progress-head">
            <span>
              {t(SCAN_STAGES.find((s) => s.id === x.progress?.stage)?.label ?? "server_reading_config")}
              {x.progress ? `: ${stageCount(x.progress)}` : "…"}
            </span>
          </div>
          <div className="run-bar" aria-hidden>
            <span
              className={x.progress && stageFraction(x.progress) !== null ? undefined : "indeterminate"}
              style={{ width: `${(x.progress ? (stageFraction(x.progress) ?? 0.3) : 0.3) * 100}%` }}
            />
          </div>
          <div className="scan-current">
            <span className="path">
              <bdi>{x.progress?.current ?? ""}</bdi>
            </span>
            <button type="button" className="btn" disabled={x.cancelling} onClick={run.cancelExport}>
              {x.cancelling ? t("stopping") : t("cancel")}
            </button>
          </div>
        </div>
      )}
      {run.exportError && (
        <div className="notice bad">
          <p className="notice-title">{t("server_not_saved")}</p>
          <pre className="notice-text">{run.exportError}</pre>
        </div>
      )}
      {run.exported && <Exported done={run.exported} />}
      <p className="run-foot muted small">
        {t("server_foot")}
      </p>
    </section>
  );
}

function Exported({ done }: { done: ExportDone }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(done.command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };
  return (
    <div className="run-exported">
      <p>
        {t("server_saved", { files: filesWord(done.summary.files) })}
        {done.summary.ai && <span className="cost"> {formatCost(done.summary.ai)}</span>}
      </p>
      <p className="code">{done.plan_path}</p>
      <p className="muted small">{t("server_run_this")}</p>
      <div className="command">
        <code>{done.command}</code>
        <button type="button" className="btn" onClick={copy}>
          {copied ? t("copied") : t("copy")}
        </button>
      </div>
    </div>
  );
}
