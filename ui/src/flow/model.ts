// The Sort flow: which steps exist and which are open, the status
// line over the current plan and the text of the AI button. Counts come from
// the plan the core built; nothing here touches files.
import type { AiEstimate, CurrentView } from "../api";
import { plural } from "../run/model";

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
  if (!i.hasPlan) return "No plan yet.";
  if (i.files === 0) return "The plan has no files to run.";
  if (!i.checked) return "Check the plan first.";
  if (i.review > 0 && !i.decided) return "Decide the uncertain files first.";
  return null;
}

export interface StatusPart {
  id: "run" | "waiting" | "unchanged" | "unmatched";
  count: number;
  text: string;
}

const files = (n: number) => plural(n, "file", "files");

/** The status line: "N to sort", "waiting for AI", "unchanged", "unmatched". */
export function statusParts(v: CurrentView): StatusPart[] {
  const p = v.plan;
  const parts: StatusPart[] = [
    { id: "run", count: p.items.length, text: `${p.items.length.toLocaleString("en-US")} to sort` },
    { id: "waiting", count: v.waiting_ai, text: `${v.waiting_ai.toLocaleString("en-US")} waiting for AI` },
    { id: "unchanged", count: (p.unchanged ?? []).length, text: `${(p.unchanged ?? []).length.toLocaleString("en-US")} unchanged` },
    { id: "unmatched", count: p.unmatched.length, text: `${p.unmatched.length.toLocaleString("en-US")} unmatched` },
  ];
  // Zero is worth saying only for the main figure.
  return parts.filter((x) => x.id === "run" || x.count > 0);
}

/** "Ask AI (12 files, ≈$0.34)"; the price is the core's estimate, the count its own or the plan's. */
export function aiAskText(waiting: number, est: AiEstimate | null): string {
  const n = est ? est.to_send : waiting;
  const price = !est ? "" : est.usd ? `, ≈$${est.usd.mid.toFixed(2)}` : ", price unknown";
  return `Ask AI (${n.toLocaleString("en-US")} ${files(n)}${price})`;
}
