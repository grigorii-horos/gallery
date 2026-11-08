import { describe, expect, it } from "vitest";
import type { AiEstimate, CurrentView } from "../api";
import { aiAskText, runLockedText, statusParts, stepStates, visibleSteps, type FlowInput } from "./model";

const input = (o: Partial<FlowInput> = {}): FlowInput => ({
  hasPlan: true,
  files: 12,
  review: 0,
  checked: false,
  decided: false,
  ...o,
});

describe("flow steps", () => {
  it("unlocks in order: check first, run only after it", () => {
    expect(stepStates(input())).toEqual({ check: "open", decide: "locked", run: "locked" });
    expect(stepStates(input({ checked: true }))).toEqual({ check: "done", decide: "open", run: "open" });
    // Nothing is open without a plan.
    expect(stepStates(input({ hasPlan: false, files: 0 })).check).toBe("locked");
  });

  it("shows “Decide” only when there is something to decide", () => {
    expect(visibleSteps({ review: 0 })).toEqual(["check", "run"]);
    expect(visibleSteps({ review: 3 })).toEqual(["check", "decide", "run"]);
  });

  it("holds “Run” until the doubtful files are decided", () => {
    const i = input({ review: 3, checked: true });
    expect(stepStates(i)).toEqual({ check: "done", decide: "open", run: "locked" });
    expect(runLockedText(i)).toMatch(/Decide/);
    expect(stepStates({ ...i, decided: true })).toEqual({ check: "done", decide: "done", run: "open" });
    expect(runLockedText({ ...i, decided: true })).toBeNull();
    // “Decide” stays locked until the plan is checked.
    expect(stepStates(input({ review: 3 })).decide).toBe("locked");
  });

  it("keeps “Run” open for a result or a pilot on show", () => {
    // After a pilot the plan is rebuilt and may be empty: the result is still there to read.
    expect(stepStates(input({ files: 0, showRun: true })).run).toBe("open");
    expect(stepStates(input({ showRun: true })).run).toBe("open");
    expect(stepStates(input({ showRun: false })).run).toBe("locked");
  });

  it("has nothing to run when the plan is empty", () => {
    const i = input({ files: 0, checked: true });
    expect(stepStates(i).run).toBe("locked");
    expect(runLockedText(i)).toMatch(/no files/);
    expect(runLockedText(input())).toMatch(/Check/);
  });
});

const view = (over: Partial<CurrentView["plan"]> = {}, waiting = 0): CurrentView =>
  ({
    path: "/d/current/x.plan.json",
    built_at: "2026-11-07T10:00:00+02:00",
    waiting_ai: waiting,
    notes: [],
    overrides: [],
    plan: { version: 1, created: "", side: "local", items: [], review: [], unmatched: [], warnings: [], unchanged: [], ...over },
  }) as unknown as CurrentView;

describe("status line", () => {
  it("counts what is to sort, waits for AI, stays and does not match", () => {
    const v = view({ items: [{}, {}] as never, unmatched: ["/a", "/b", "/c"] as never, unchanged: ["/k"] as never }, 4);
    expect(statusParts(v).map((p) => p.text)).toEqual(["2 to sort", "4 waiting for AI", "1 unchanged", "3 unmatched"]);
  });

  it("keeps only the main figure when the rest is zero", () => {
    expect(statusParts(view()).map((p) => p.text)).toEqual(["0 to sort"]);
  });
});

describe("AI button", () => {
  const est = (usd: number | null, to_send = 12): AiEstimate => ({
    candidates: 20,
    cached: 8,
    to_send,
    model: "m",
    usd: usd === null ? null : { low: usd, mid: usd, high: usd },
    source_label: "",
    label: "",
    confirm: false,
  });

  it("says how many files and about how much", () => {
    expect(aiAskText(5, est(0.344))).toBe("Ask AI (12 files, ≈$0.34)");
    expect(aiAskText(5, est(null, 1))).toBe("Ask AI (1 file, price unknown)");
  });

  it("shows the plan's own count until the estimate comes", () => {
    expect(aiAskText(5, null)).toBe("Ask AI (5 files)");
  });
});
