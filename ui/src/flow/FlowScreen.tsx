import { useState, type ReactNode } from "react";
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

const TITLES: Record<StepId, string> = {
  check: "Check",
  decide: "Decide",
  run: "Run",
};

/// Sort: the whole sorting on one screen over the one current plan.
/// The status line says where things stand; three steps open in order —
/// check the plan, decide the doubtful files (only when there are any), run it.
export function FlowScreen({ data, plan, run, sync, otherBusy, retryPath, onRetryDone, onOpenHistory, onUndoPilot }: Props) {
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
        <p className="empty">Opening the current plan…</p>
      ) : plan.loadError ? (
        <div className="notice bad">
          <p className="notice-title">The plan did not open</p>
          <pre className="notice-text">{plan.loadError}</pre>
        </div>
      ) : !view ? (
        <div className="plan-empty">
          <p>
            {plan.updateError
              ? "The plan could not be built (see the reason above)."
              : "The plan builds itself: it reads the sources from the config and assigns files to targets. The files themselves are not changed."}
          </p>
          <p className="muted">AI is not called until you press “Ask AI” yourself.</p>
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
                    ? `${view.plan.review.length.toLocaleString("en-US")} to decide`
                    : null
              }
            >
              {id === "check" && (
                <>
                  <CheckStep data={data} plan={plan} onConfigSaved={data.reload} />
                  <div className="step-foot">
                    <button type="button" className="btn primary" disabled={states.check === "locked"} onClick={checkDone}>
                      {input.checked ? "Checked" : "Looks right — next"}
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
                      {input.decided ? "Decided" : "Decisions made — next"}
                    </button>
                    <span className="muted small">Undecided files will not be part of this run and stay where they are.</span>
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
  const [open, setOpen] = useState(false);
  return (
    <section className="flow-step flow-server">
      <h2>
        <button type="button" className="flow-step-head" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          <span className="step-title">For server</span>
          <span className="muted small">export plan</span>
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
  const view = plan.view;
  return (
    <div className="flow-status" role="status" aria-label="Plan status">
      {view ? (
        <ul className="flow-counts">
          {statusParts(view).map((p) => (
            <li key={p.id} className={p.count === 0 ? "muted" : undefined}>
              {p.text}
            </li>
          ))}
        </ul>
      ) : (
        <span className="muted">{plan.loading ? "Opening the plan…" : "No plan yet"}</span>
      )}
      {plan.updating && <span className="muted small">Refreshing the plan…</span>}
    </div>
  );
}

function Notices({ data, plan }: { data: AppData; plan: PlanState }) {
  const notes = plan.view?.notes ?? [];
  return (
    <>
      {!data.summary && data.loadError && (
        <div className="notice bad">
          <p className="notice-title">Config could not be read, the plan is not refreshed</p>
          <pre className="notice-text">{data.loadError}</pre>
        </div>
      )}
      {plan.updateError && (
        <div className="notice bad">
          <p className="notice-title">The plan was not refreshed</p>
          <pre className="notice-text">{plan.updateError}</pre>
        </div>
      )}
      {plan.scanError && (
        <div className="notice bad">
          <p className="notice-title">The AI request failed</p>
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
  const locked = state === "locked";
  return (
    <section className={`flow-step ${state}`} aria-label={TITLES[id]} data-step={id}>
      <h2>
        <button type="button" className="flow-step-head" aria-expanded={open} disabled={locked} onClick={onToggle}>
          <span className="step-n" aria-hidden>
            {state === "done" ? "✓" : n}
          </span>
          <span className="step-title">{TITLES[id]}</span>
          {note && <span className="muted small">{note}</span>}
        </button>
      </h2>
      {open && <div className="flow-step-body">{children}</div>}
    </section>
  );
}
