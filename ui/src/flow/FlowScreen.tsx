import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { MessageKey } from "../i18n";
import type { AppData } from "../store";
import type { PlanState } from "../plan/usePlan";
import type { RunState } from "../run/useRun";
import type { SyncPauseState } from "../sync/useSyncPause";
import { SyncBanner } from "../components/SyncBanner";
import { AiAsk } from "./AiAsk";
import { CheckStep } from "./CheckStep";
import { DecideStep } from "./DecideStep";
import { RetryCard, RunStep, ServerCard } from "./RunStep";
import { runLockedText, statusParts, stepStates, visibleSteps, type StepId, type StepState } from "./model";

interface Props {
  data: AppData;
  plan: PlanState;
  run: RunState;
  sync: SyncPauseState;
  /** Another operation holds the slot (export, undo, retry), or null. */
  otherBusy: string | null;
  /** A retry plan made in History, waiting to be run. */
  retryPath: string | null;
  onRetryDone: () => void;
  onOpenHistory: () => void;
  onUndoPilot: (reportPath: string) => void;
}

const TITLES: Record<StepId, MessageKey> = {
  check: "step_check",
  decide: "step_decide",
  run: "step_run",
};

/// Sort: the whole sorting on one screen over the one current plan.
/// The status line says where things stand; three steps open in order —
/// check the plan, decide the doubtful files (only when there are any), run it.
export function FlowScreen({ data, plan, run, sync, otherBusy, retryPath, onRetryDone, onOpenHistory, onUndoPilot }: Props) {
  const { t } = useTranslation();
  const view = plan.view;
  const cfg = data.summary;
  // The steps start over after each real run: the plan has changed under them.
  const [checkedAt, setCheckedAt] = useState<number | null>(null);
  const [decidedAt, setDecidedAt] = useState<number | null>(null);
  const [forced, setForced] = useState<Partial<Record<StepId, boolean>>>({});
  const input = {
    hasPlan: !!view,
    files: view?.plan.items.length ?? 0,
    review: view?.plan.review.length ?? 0,
    checked: checkedAt === run.finished,
    decided: decidedAt === run.finished,
    showRun: !!run.result || !!run.pilot || !!run.run,
  };
  const states = stepStates(input);
  const steps = visibleSteps(input);
  const isOpen = (id: StepId) => states[id] !== "locked" && (forced[id] ?? states[id] === "open");
  const toggle = (id: StepId) => setForced((f) => ({ ...f, [id]: !isOpen(id) }));

  const canScan = !!cfg?.valid && !otherBusy && !plan.updating && !run.run;
  const afterCheck: StepId = steps.includes("decide") ? "decide" : "run";
  const checkDone = () => {
    setCheckedAt(run.finished);
    setForced((f) => ({ ...f, check: false, [afterCheck]: true }));
  };
  const decideDone = () => {
    setDecidedAt(run.finished);
    setForced((f) => ({ ...f, decide: false, run: true }));
  };

  return (
    <div className="flow">
      {!isOpen("run") && <SyncBanner sync={sync} />}
      <StatusLine plan={plan} />
      <AiAsk plan={plan} configPath={data.configPath} canScan={canScan} />
      <Notices data={data} plan={plan} />
      {retryPath && !run.run && <RetryCard path={retryPath} run={run} busy={otherBusy} onDone={onRetryDone} />}
      {plan.loading && !view ? (
        <p className="empty">{t("flow_opening")}</p>
      ) : plan.loadError ? (
        <div className="notice bad">
          <p className="notice-title">{t("flow_open_failed")}</p>
          <pre className="notice-text">{plan.loadError}</pre>
        </div>
      ) : !view ? (
        <div className="plan-empty">
          <p>
            {plan.updateError
              ? t("flow_build_failed")
              : t("flow_builds_itself")}
          </p>
          <p className="muted">{t("flow_ai_manual")}</p>
        </div>
      ) : (
        <>
          {steps.map((id, i) => (
            <Step
              key={id}
              n={i + 1}
              id={id}
              state={states[id]}
              open={isOpen(id)}
              onToggle={() => toggle(id)}
              note={
                id === "run" && states.run === "locked"
                  ? runLockedText(input)
                  : id === "decide"
                    ? t("flow_to_decide", { n: view.plan.review.length })
                    : null
              }
            >
              {id === "check" && (
                <>
                  <CheckStep data={data} plan={plan} onConfigSaved={data.reload} />
                  <div className="step-foot">
                    <button type="button" className="btn primary" disabled={states.check === "locked"} onClick={checkDone}>
                      {input.checked ? t("flow_checked") : t("flow_check_next")}
                    </button>
                  </div>
                </>
              )}
              {id === "decide" && (
                <>
                  <div className="review">
                    <DecideStep data={data} plan={plan} />
                  </div>
                  <div className="step-foot">
                    <button type="button" className="btn primary" onClick={decideDone}>
                      {input.decided ? t("flow_decided") : t("flow_decide_next")}
                    </button>
                    <span className="muted small">{t("flow_undecided")}</span>
                  </div>
                </>
              )}
              {id === "run" && (
                <RunStep
                  plan={plan}
                  run={run}
                  sync={sync}
                  busy={otherBusy}
                  onOpenHistory={onOpenHistory}
                  onUndoPilot={onUndoPilot}
                />
              )}
            </Step>
          ))}
        </>
      )}
      <ServerSection data={data} plan={plan} run={run} busy={run.exporting ? null : otherBusy} />
    </div>
  );
}

/// Collapsible section for the export, below the three steps.
function ServerSection({ data, plan, run, busy }: { data: AppData; plan: PlanState; run: RunState; busy: string | null }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <section className="flow-step flow-server">
      <h2>
        <button type="button" className="flow-step-head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          <span className="step-title">{t("flow_for_server")}</span>
          <span className="muted small">{t("flow_export_plan")}</span>
        </button>
      </h2>
      {open && (
        <div className="flow-step-body">
          <ServerCard data={data} plan={plan} run={run} busy={busy} />
        </div>
      )}
    </section>
  );
}

function StatusLine({ plan }: { plan: PlanState }) {
  const { t } = useTranslation();
  const view = plan.view;
  return (
    <div className="flow-status" role="status" aria-label={t("flow_status_aria")}>
      {view ? (
        <ul className="flow-counts">
          {statusParts(view).map((p) => (
            <li key={p.id} className={p.count === 0 ? "muted" : undefined}>
              {p.text}
            </li>
          ))}
        </ul>
      ) : (
        <span className="muted">{plan.loading ? t("flow_opening_short") : t("flow_no_plan")}</span>
      )}
      {plan.updating && <span className="muted small">{t("flow_refreshing")}</span>}
    </div>
  );
}

function Notices({ data, plan }: { data: AppData; plan: PlanState }) {
  const { t } = useTranslation();
  const notes = plan.view?.notes ?? [];
  return (
    <>
      {!data.summary && data.loadError && (
        <div className="notice bad">
          <p className="notice-title">{t("flow_config_unread")}</p>
          <pre className="notice-text">{data.loadError}</pre>
        </div>
      )}
      {plan.updateError && (
        <div className="notice bad">
          <p className="notice-title">{t("flow_not_refreshed")}</p>
          <pre className="notice-text">{plan.updateError}</pre>
        </div>
      )}
      {plan.scanError && (
        <div className="notice bad">
          <p className="notice-title">{t("flow_ai_failed")}</p>
          <pre className="notice-text">{plan.scanError}</pre>
        </div>
      )}
      {plan.scanNotice && !plan.scan && <p className="muted note-line">{plan.scanNotice}.</p>}
      {notes.length > 0 && (
        <div className="notice">
          {notes.map((n) => (
            <p key={n}>{n}</p>
          ))}
        </div>
      )}
    </>
  );
}

function Step({
  n,
  id,
  state,
  open,
  onToggle,
  note,
  children,
}: {
  n: number;
  id: StepId;
  state: StepState;
  open: boolean;
  onToggle: () => void;
  note: string | null;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const locked = state === "locked";
  return (
    <section className={`flow-step ${state}`} aria-label={t(TITLES[id])} data-step={id}>
      <h2>
        <button type="button" className="flow-step-head" aria-expanded={open} disabled={locked} onClick={onToggle}>
          <span className="step-n" aria-hidden>
            {state === "done" ? "✓" : n}
          </span>
          <span className="step-title">{t(TITLES[id])}</span>
          {note && <span className="muted small">{note}</span>}
        </button>
      </h2>
      {open && <div className="flow-step-body">{children}</div>}
    </section>
  );
}
