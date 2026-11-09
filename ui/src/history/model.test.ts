import { describe, expect, it } from "vitest";
import type { ItemView, ReportEntry } from "../api";
import {
  entryChips,
  entryTally,
  filterCounts,
  filterItems,
  retryBlock,
  retryConfirmLines,
  runTime,
  undoBlock,
  partialUndoConfirmLines,
  undoConfirmLines,
  undoTargets,
} from "./model";

export const entry = (over: Partial<ReportEntry> = {}): ReportEntry => ({
  path: "/d/plans/20261105-101500.plan.report.json",
  file_name: "20261105-101500.plan.report.json",
  plan_path: "/d/plans/20261105-101500.plan.json",
  plan_exists: true,
  side: "local",
  run_id: "20261105-101512",
  started: "2026-11-05T10:15:12+02:00",
  finished: "2026-11-05T10:20:00+02:00",
  archived: false,
  summary: { total: 10, done: 7, renamed: 2, failed: 2, stale: 1, missing: 0, not_started: 0, cancelled: false },
  undone: false,
  partially_undone: false,
  undone_items: 0,
  error: null,
  ...over,
});

const item = (source: string, status: ItemView["status"]): ItemView => ({
  source,
  status,
  trashed: null,
  renamed: false,
  undone: false,
  target: null,
  undo: { items: 0, restore: 0, rename_back: 0, remove_outputs: 0 },
  error: null,
  outputs: [],
});

describe("history model", () => {
  it("reads the run time from the report as written", () => {
    expect(runTime("2026-11-05T10:15:12+02:00")).toBe("November 5, 2026, 10:15");
    expect(runTime("")).toBe(null);
    expect(runTime("garbage")).toBe(null);
  });

  it("chips say what state the run is in", () => {
    const labels = (e: ReportEntry) => entryChips(e).map((c) => `${c.tone}:${c.label}`);
    expect(labels(entry())).toEqual(["bad:2 errors", "warn:1 skipped"]);
    expect(labels(entry({ undone: true }))).toEqual(["muted:undone"]);
    expect(labels(entry({ partially_undone: true, undone_items: 2 }))).toContain("muted:partly undone");
    expect(labels(entry({ side: "server", summary: { ...entry().summary, failed: 0, stale: 0 } }))).toEqual([
      "accent:server",
      "ok:no errors",
    ]);
    expect(labels(entry({ finished: "" }))).toContain("warn:unfinished");
    expect(labels(entry({ summary: { ...entry().summary, cancelled: true, not_started: 3 } }))).toContain(
      "warn:stopped",
    );
    expect(labels(entry({ archived: true }))).toContain("muted:earlier run");
    expect(labels(entry({ error: "x" }))).toEqual(["bad:unreadable"]);
  });

  it("the tally splits the plan into done, failed, skipped and not started", () => {
    const t = entryTally(entry({ summary: { ...entry().summary, total: 12, not_started: 2 } }));
    expect(t.map((s) => [s.id, s.count])).toEqual([
      ["done", 7],
      ["failed", 2],
      ["skipped", 1],
      ["not_started", 2],
    ]);
  });

  it("filters the files and counts each filter", () => {
    const items = [item("/a", "done"), item("/b", "failed"), item("/c", "stale"), item("/d", "missing"), item("/e", "failed")];
    expect(filterCounts(items)).toEqual({ all: 5, failed: 2, stale: 1, missing: 1 });
    expect(filterItems(items, "failed").map((i) => i.source)).toEqual(["/b", "/e"]);
    expect(filterItems(items, "all")).toHaveLength(5);
    expect(filterItems(items, "missing").map((i) => i.source)).toEqual(["/d"]);
  });

  it("undo is offered only for a local, finished-or-not, not undone report", () => {
    expect(undoBlock(entry(), null)).toBe(null);
    expect(undoBlock(entry({ side: "server" }), null)?.text).toMatch(/server/);
    expect(undoBlock(entry({ undone: true }), null)?.text).toMatch(/already undone/);
    expect(undoBlock(entry({ plan_exists: false }), null)?.text).toMatch(/plan/);
    expect(undoBlock(entry({ error: "x" }), null)?.text).toMatch(/could not be read/);
    expect(undoBlock(entry(), "a run is in progress")?.text).toBe("a run is in progress");
  });

  it("retry is offered only for a finished local run with failures", () => {
    expect(retryBlock(entry(), null)).toBe(null);
    expect(retryBlock(entry({ summary: { ...entry().summary, failed: 0 } }), null)?.text).toMatch(/no failed/);
    expect(retryBlock(entry({ finished: "" }), null)?.text).toMatch(/not finished/);
    expect(retryBlock(entry({ side: "server" }), null)?.text).toMatch(/server/);
    expect(retryBlock(entry({ undone: true }), null)?.text).toMatch(/undone/);
  });

  it("the undo confirmation counts what moves", () => {
    const { title, lines } = undoConfirmLines(entry(), { items: 9, restore: 6, rename_back: 2, remove_outputs: 11 });
    expect(title).toBe("Undo the run from November 5, 2026, 10:15?");
    expect(lines).toEqual([
      "6 sources are restored from the trash.",
      "2 renamed files will get the old names back.",
      "11 copies will be deleted from the target folders.",
      "Nothing is overwritten: an occupied spot, a modified copy or a file missing from the trash stays as it is and goes to the problem list.",
    ]);
    const none = undoConfirmLines(entry(), { items: 0, restore: 0, rename_back: 0, remove_outputs: 0 });
    expect(none.lines[0]).toMatch(/Nothing to restore/);
  });

  it("groups the files still to undo by target and sums what undoing each does", () => {
    const it2 = (source: string, target: string | null, restore: number, out: number): ItemView => ({
      ...item(source, "done"),
      target,
      undo: { items: restore + out > 0 ? 1 : 0, restore, rename_back: 0, remove_outputs: out },
    });
    const g = undoTargets([it2("/a", "pics", 1, 2), it2("/b", "texts", 1, 1), it2("/c", "pics", 1, 1), it2("/d", "pics", 0, 0), it2("/e", null, 1, 0)]);
    expect(g.map((x) => [x.target, x.sources])).toEqual([
      ["pics", ["/a", "/c"]],
      ["texts", ["/b"]],
    ]);
    expect(g[0].preview).toEqual({ items: 2, restore: 2, rename_back: 0, remove_outputs: 3 });
    expect(partialUndoConfirmLines("pics", g[0].preview).title).toBe("Undo target “pics”?");
    expect(partialUndoConfirmLines(null, g[0].preview).title).toBe("Undo this file?");
  });

  it("the retry confirmation says what happens before anything runs", () => {
    const lines = retryConfirmLines(entry());
    expect(lines.join(" ")).toMatch(/2 failed files/);
    expect(lines.join(" ")).toMatch(/Sort/);
  });
});
