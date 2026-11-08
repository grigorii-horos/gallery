import { describe, expect, it } from "vitest";
import type { PlanView } from "../api";
import fixture from "./fixture-plan.json";
import {
  decisionText,
  formatCost,
  gridWindow,
  groupFiles,
  indexPlan,
  planTime,
  splitTrace,
  stageCount,
  stageFraction,
  summarize,
  traceOf,
} from "./model";

const view = fixture as unknown as PlanView;
const ix = indexPlan(view);

describe("summary", () => {
  it("counts targets in config order and the other groups", () => {
    const s = summarize(view.plan, ["Screenshots", "Videos", "Documents", "Empty"]);
    expect(s.total).toBe(8);
    expect(s.targets.map((t) => [t.label, t.count, !!t.unknown])).toEqual([
      ["Screenshots", 1, false],
      ["Videos", 1, false],
      ["Documents", 2, false],
    ]);
    expect(s.others.map((g) => [g.label, g.count])).toEqual([
      ["To decide", 1],
      ["Unmatched", 1],
      ["Unchanged", 2],
    ]);
    expect(s.warnings).toEqual([
      "A series was split across targets: IMG_1.jpg and IMG_2.jpg",
      "Double re-encoding: a.mp4 → /lib/video/a.mp4",
    ]);
  });

  it("puts targets the config no longer has last, marked", () => {
    const s = summarize(view.plan, ["Documents"]);
    expect(s.targets.map((t) => [t.label, !!t.unknown])).toEqual([
      ["Documents", false],
      ["Screenshots", true],
      ["Videos", true],
    ]);
    // Without a config nothing is marked unknown.
    expect(summarize(view.plan).targets.some((t) => t.unknown)).toBe(false);
  });

  it("lists the files of each group", () => {
    expect(groupFiles(view.plan, { kind: "target", name: "Documents" })).toEqual([
      "/media/cam/IMG_20260412_103314.jpg",
      "/media/cam/IMG_20260413_090000.jpg",
    ]);
    expect(groupFiles(view.plan, { kind: "review" })).toEqual(["/media/cam/IMG_20260415_120000.jpg"]);
    expect(groupFiles(view.plan, { kind: "unchanged" })).toHaveLength(2);
  });
});

describe("trace", () => {
  it("explains a mask decision with a rename", () => {
    const t = traceOf(ix, "/media/cam/Screenshot_20260412.png")!;
    expect(decisionText(t.decision)).toBe("Target mask “Screenshots”");
    expect(t.outputs).toHaveLength(1);
    expect(t.outputs[0].op).toBe("rename");
    expect(t.outputs[0].opLabel).toBe("Move the source");
    expect(t.outputs[0].content).toEqual([]);
    expect(t.outputs[0].rename.map((s) => s.preset)).toEqual(["capture-date"]);
  });

  it("splits content and name steps and keeps unmatched ones", () => {
    const t = traceOf(ix, "/media/cam/IMG_20260412_103314.jpg")!;
    expect(t.decision).toEqual({
      kind: "ai",
      target: "Documents",
      reason: "Passport photo on a table",
      low: false,
    });
    const o = t.outputs[0];
    expect(o.content.map((s) => s.preset)).toEqual(["metadata", "avif-high"]);
    expect(o.rename.map((s) => [s.preset, s.matched])).toEqual([
      ["capture-date", true],
      ["Prefix", false],
    ]);
    expect(o.dest).toBe("/lib/docs/2026/04/20260412_103314.avif");
  });

  it("reads old plans: no op means write, pipes split at the index restart", () => {
    const t = traceOf(ix, "/media/cam/IMG_20260413_090000.jpg")!;
    expect(decisionText(t.decision)).toBe("AI: “Documents”, low confidence (accepted)");
    expect(t.outputs[0].op).toBe("write");
    expect(t.outputs[0].content.map((s) => s.preset)).toEqual(["avif-high"]);
    expect(t.outputs[0].rename.map((s) => s.preset)).toEqual(["capture-date"]);
    const single = { dest: "/x", steps: [], trace: [{ index: 0, preset: "n", matched: true, after: "a" }] };
    expect(splitTrace(single).rename).toHaveLength(1);
    expect(splitTrace({ ...single, steps: ["c"] }).content).toHaveLength(1);
  });

  it("marks manual decisions and the stored override", () => {
    const t = traceOf(ix, "/media/cam/VID_20260414.mp4")!;
    expect(decisionText(t.decision)).toBe("Manual decision: “Videos”");
    expect(t.override).toEqual({ kind: "target", name: "Videos" });
    const kept = traceOf(ix, "/media/cam/kept.jpg")!;
    expect(decisionText(kept.decision)).toBe("Manual decision: keep in source");
    const same = traceOf(ix, "/media/cam/2026-01-01 10.00.00.jpg")!;
    expect(same.decision).toEqual({ kind: "unchanged", manualKeep: false });
    expect(same.override).toBeNull();
  });

  it("explains review and unmatched files; unknown paths give null", () => {
    const r = traceOf(ix, "/media/cam/IMG_20260415_120000.jpg")!;
    expect(r.group).toEqual({ kind: "review" });
    expect(decisionText(r.decision)).toBe("AI is unsure, suggests “Documents”");
    expect(decisionText({ kind: "review", suggested: "—", reason: "" })).toBe(
      "AI is unsure and suggested no target",
    );
    expect(decisionText(traceOf(ix, "/media/cam/notes.bin")!.decision)).toBe("No target fit");
    expect(traceOf(ix, "/nope")).toBeNull();
  });
});

describe("progress and labels", () => {
  it("formats stages, cost and plan time", () => {
    expect(stageFraction(undefined)).toBeNull();
    expect(stageFraction({ done: 3, total: 0 })).toBeNull();
    expect(stageFraction({ done: 5, total: 10 })).toBe(0.5);
    expect(stageCount({ done: 42, total: 0 })).toBe("42");
    expect(stageCount({ done: 5, total: 10 })).toBe("5 of 10");
    expect(formatCost({ input: 1200, output: 30, usd: 0.0123 })).toMatch(/^AI: \$0\.0123 \(1,200 in, 30 out\)$/);
    expect(formatCost({ input: 1, output: 2, usd: null })).toBe("AI: 1 in, 2 out tokens");
    expect(planTime("/p/20261105-101500.plan.json")).toBe("November 5, 2026, 10:15");
    expect(planTime("/p/20261105-101500-2.plan.json")).toBe("November 5, 2026, 10:15");
    expect(planTime("/p/20261105-101500-retry.plan.json")).toBe("November 5, 2026, 10:15 (retry of failed)");
    expect(planTime("/p/20261105-101500-2-retry-2.plan.json")).toBe("November 5, 2026, 10:15 (retry of failed)");
    expect(planTime("/p/x.json")).toBeNull();
  });
});

describe("virtual grid", () => {
  it("renders only the visible rows plus overscan", () => {
    // 1000 cells, 4 columns of 100 + 10 gap in 430 px, rows 120 + 10.
    const w = gridWindow(1000, 430, 300, 0, 100, 120, 10, 1);
    expect(w.columns).toBe(4);
    expect(w.start).toBe(0);
    expect(w.end).toBe(4 * 4); // rows 0..2 visible + 1 overscan
    expect(w.totalHeight).toBe(250 * 130 - 10);
    const far = gridWindow(1000, 430, 300, 13000, 100, 120, 10, 1);
    expect(far.start).toBe(99 * 4);
    expect(far.offsetY).toBe(99 * 130);
    expect(far.end - far.start).toBeLessThanOrEqual(6 * 4);
    // The end clamps to the count; an empty grid has no height.
    expect(gridWindow(5, 430, 3000, 0, 100, 120, 10).end).toBe(5);
    expect(gridWindow(0, 430, 300, 0, 100, 120, 10).totalHeight).toBe(0);
    expect(gridWindow(3, 50, 300, 0, 100, 120, 10).columns).toBe(1);
  });
});
