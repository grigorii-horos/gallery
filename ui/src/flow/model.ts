// The Sort flow: which steps exist and which are open, the status
// line over the current plan and the text of the AI button. Counts come from
// the plan the core built; nothing here touches files.
import type { AiEstimate, CurrentView } from "../api";
import { t } from "../i18n";
import { usd } from "../i18n/format";

export type StepId = "check" | "decide" | "run";
export type StepState = "locked" | "open" | "done";

export interface FlowInput {
  hasPlan: boolean;
  /** Files the plan will run. */
  files: number;
  /** Files that need a decision (low-confidence answers, names that did not fit). */
  review: number;
  /** The user went through Check. */
  checked: boolean;
  /** The user went through Decide. */
  decided: boolean;
  /** A run's result (or a pilot waiting for the decision) is on show: Run stays open for it. */
  showRun?: boolean;
}

/** Decide exists only while there is something to decide. */
export function visibleSteps(i: Pick<FlowInput, "review">): StepId[] {
  return i.review > 0 ? ["check", "decide", "run"] : ["check", "run"];
}

/// Steps unlock in order: Check with a plan, Decide after it, Run
/// after the last step before it, and only with something to run.
export function stepStates(i: FlowInput): Record<StepId, StepState> {
  const check: StepState = !i.hasPlan ? "locked" : i.checked ? "done" : "open";
  const decide: StepState = !i.checked ? "locked" : i.decided ? "done" : "open";
  const beforeRun = i.review > 0 ? i.decided : true;
  const run: StepState = i.showRun || (i.checked && beforeRun && i.files > 0) ? "open" : "locked";
  return { check, decide, run };
}

/** Why Run is closed, for the step header; null when it is open. */
export function runLockedText(i: FlowInput): string | null {
  if (!i.hasPlan) return t("run_locked_no_plan");
  if (i.files === 0) return t("run_locked_no_files");
  if (!i.checked) return t("run_locked_check");
  if (i.review > 0 && !i.decided) return t("run_locked_decide");
  return null;
}

export interface StatusPart {
  id: "run" | "waiting" | "unchanged" | "kept" | "unmatched";
  count: number;
  text: string;
}

/** The status line: "N to sort", "waiting for AI", "unchanged", "unmatched". */
export function statusParts(v: CurrentView): StatusPart[] {
  const p = v.plan;
  const parts: StatusPart[] = [
    { id: "run", count: p.items.length, text: t("status_to_sort", { n: p.items.length }) },
    { id: "waiting", count: v.waiting_ai, text: t("status_waiting_ai", { n: v.waiting_ai }) },
    { id: "unchanged", count: (p.unchanged ?? []).length, text: t("status_unchanged", { n: (p.unchanged ?? []).length }) },
    { id: "kept", count: (p.kept ?? []).length, text: t("status_kept", { n: (p.kept ?? []).length }) },
    { id: "unmatched", count: p.unmatched.length, text: t("status_unmatched", { n: p.unmatched.length }) },
  ];
  // Zero is worth saying only for the main figure.
  return parts.filter((x) => x.id === "run" || x.count > 0);
}

/** "Ask AI (12 files, ≈$0.34)"; the price is the core's estimate, the count its own or the plan's. */
export function aiAskText(waiting: number, est: AiEstimate | null): string {
  const n = est ? est.to_send : waiting;
  if (!est) return t("ask_ai", { count: n });
  return est.usd
    ? t("ask_ai_price", { count: n, price: usd(est.usd.mid) })
    : t("ask_ai_unknown", { count: n });
}
