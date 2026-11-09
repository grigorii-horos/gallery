import { describe, expect, it } from "vitest";
import type { ApplyDone, ApplyPreview, PilotFacts, PilotResult } from "../api";
import {
  syncOption,
  applyBlock,
  confirmLines,
  factRows,
  fates,
  filesWord,
  pilotHeadline,
  progressFraction,
  resultHeadline,
  resultRows,
} from "./model";

const preview = (p: Partial<ApplyPreview> = {}): ApplyPreview => ({
  plan_path: "/d/plans/20261105-101500.plan.json",
  created: "2026-11-05T10:15:00+02:00",
  side: "local",
  files: 120,
  trash: 84,
  renamed: 30,
  kept: 6,
  to_trash: 0,
  kept_targets: 0,
  writes: 96,
  review: 7,
  unmatched: 4,
  unchanged: 1,
  applied: null,
  plan_id: "p1",
  ...p,
});

const done = (s: Partial<ApplyDone["summary"]> = {}, dry = false): ApplyDone => ({
  plan_path: "/d/plans/a.plan.json",
  dry_run: dry,
  report_path: dry ? null : "/d/plans/a.plan.report.json",
  summary: {
    total: 10,
    done: 10,
    renamed: 0,
    failed: 0,
    stale: 0,
    missing: 0,
    not_started: 0,
    cancelled: false,
    ...s,
  },
  failures: [],
  warnings: [],
});

describe("filesWord", () => {
  it("pluralizes in English", () => {
    expect([1, 2, 5, 11, 21, 22, 25, 111].map((n) => filesWord(n))).toEqual([
      "1 file",
      "2 files",
      "5 files",
      "11 files",
      "21 files",
      "22 files",
      "25 files",
      "111 files",
    ]);
    expect(filesWord(12000)).toBe("12,000 files");
  });
});

describe("fates", () => {
  it("splits files into trash, renamed and kept, in that order", () => {
    expect(fates(preview()).map((f) => [f.id, f.count])).toEqual([
      ["trash", 84],
      ["renamed", 30],
      ["kept", 6],
    ]);
  });
});

describe("confirmLines", () => {
  it("says how many files, what goes to the trash and what is renamed in place", () => {
    const c = confirmLines(preview());
    expect(c.title).toBe("Run the plan: 120 files?");
    expect(c.lines.join("\n")).toContain("84 sources go to the trash");
    expect(c.lines.join("\n")).toContain("30 will be renamed in place");
    expect(c.lines.join("\n")).toContain("6 will stay where they are");
    expect(c.lines.join("\n")).toContain("96 new files");
    expect(c.lines.join("\n")).toContain("12 files outside the plan");
  });

  it("drops lines with nothing in them", () => {
    const c = confirmLines(preview({ files: 1, trash: 1, renamed: 0, kept: 0, writes: 1, review: 0, unmatched: 0, unchanged: 0 }));
    expect(c.title).toBe("Run the plan: 1 file?");
    expect(c.lines.join("\n")).toContain("1 source goes to the trash");
    expect(c.lines.join("\n")).not.toMatch(/renamed in place|stay where|outside the plan|Trash targets|Keep targets/);
  });

  it("names the files of trash and keep targets", () => {
    const p = preview({ files: 3, trash: 0, renamed: 0, kept: 0, to_trash: 3, kept_targets: 2, writes: 0 });
    const c = confirmLines(p);
    expect(c.lines.join("\n")).toContain("Trash targets: 3 files go to the trash");
    expect(c.lines.join("\n")).toContain("Keep targets: 2 files are kept");
    expect(fates(p).find((f) => f.id === "to_trash")?.count).toBe(3);
  });
});

describe("applyBlock", () => {
  const ok = { preview: preview(), busy: null };

  it("allows the current plan", () => {
    expect(applyBlock(ok)).toBeNull();
  });

  it("refuses a server plan, an empty plan and while busy", () => {
    expect(applyBlock({ ...ok, preview: preview({ side: "server" }) })?.text).toMatch(/server/);
    expect(applyBlock({ ...ok, preview: preview({ files: 0 }) })?.text).toMatch(/no files/);
    expect(applyBlock({ ...ok, preview: null })?.text).toMatch(/No plan yet/);
    expect(applyBlock({ ...ok, busy: "a scan is running" })).toEqual({ text: "a scan is running", history: false });
  });

  it("waits for the plan to be brought up to date, but lets a dry run be checked", () => {
    expect(applyBlock({ ...ok, updating: true })?.text).toMatch(/refreshing/);
    expect(applyBlock({ ...ok, updating: true }, true)?.text).toMatch(/refreshing/);
    expect(applyBlock({ ...ok, updating: false })).toBeNull();
    expect(applyBlock({ ...ok, preview: preview({ side: "server" }) }, true)?.text).toMatch(/server/);
  });
});

describe("progressFraction", () => {
  it("is 0…1 and 0 without a total", () => {
    expect(progressFraction({ done: 0, total: 0, current: null, failed: 0, skipped: 0 })).toBe(0);
    expect(progressFraction({ done: 5, total: 20, current: null, failed: 0, skipped: 0 })).toBe(0.25);
    expect(progressFraction({ done: 25, total: 20, current: null, failed: 0, skipped: 0 })).toBe(1);
  });
});

describe("result", () => {
  it("is calm when everything went through", () => {
    expect(resultHeadline(done())).toEqual({ tone: "ok", title: "Done: 10 files processed" });
    expect(resultHeadline(done({}, true))).toEqual({ tone: "ok", title: "Check passed: 10 files can be processed" });
  });

  it("warns on skipped and failed files, and on cancel", () => {
    expect(resultHeadline(done({ done: 8, stale: 2 })).tone).toBe("warn");
    expect(resultHeadline(done({ done: 7, failed: 3 }))).toEqual({ tone: "bad", title: "Finished with errors: 3 of 10" });
    expect(resultHeadline(done({ done: 4, not_started: 6, cancelled: true }))).toEqual({
      tone: "warn",
      title: "Stopped: 4 of 10 processed, 6 not started",
    });
  });

  it("lists only non-zero rows, done first", () => {
    expect(resultRows(done({ done: 7, renamed: 2, failed: 1, stale: 1, missing: 1 }).summary, false)).toEqual([
      { label: "processed", count: 7, tone: "ok" },
      { label: "of them renamed in place", count: 2, tone: undefined },
      { label: "errors", count: 1, tone: "bad" },
      { label: "changed after the plan — left untouched", count: 1, tone: "warn" },
      { label: "file missing", count: 1, tone: "warn" },
    ]);
    expect(resultRows(done({ done: 0, not_started: 10, cancelled: true }).summary, true)).toEqual([
      { label: "can be processed", count: 0, tone: "ok" },
      { label: "not started (cancelled) — left untouched", count: 10, tone: undefined },
    ]);
  });
});

describe("pilot", () => {
  const f = (over: Partial<PilotFacts> = {}): PilotFacts => ({
    size: 2048,
    mtime: "2026-04-12 10:33:14",
    capture: "2026-04-12 10:33:14",
    gps: true,
    ...over,
  });

  it("lists before and after of size, date, mtime and GPS", () => {
    const rows = factRows(f({ size: 3 * 1024 * 1024 }), f({ mtime: "2026-11-07 10:00:00", gps: false, capture: null }));
    expect(rows.map((r) => [r.label, r.before, r.after, r.changed])).toEqual([
      ["Size", "3.0 MB", "2.0 KB", true],
      ["Capture date", "2026-04-12 10:33:14", "—", true],
      ["mtime", "2026-04-12 10:33:14", "2026-11-07 10:00:00", true],
      ["GPS", "yes", "no", true],
    ]);
    expect(factRows(f(), f()).every((r) => !r.changed)).toBe(true);
    expect(factRows(f(), { size: null, mtime: null, capture: null, gps: null })[3].after).toBe("—");
  });

  it("words a run stopped by anomalies", () => {
    const h = resultHeadline(done({ cancelled: true, stopped_by_anomalies: true, anomalies: 3, not_started: 9 }));
    expect(h.tone).toBe("bad");
    expect(h.title).toBe("Stopped: results differ from the sources (3), 9 not started");
  });

  it("names the pilot's outcome", () => {
    const file = (status: "done" | "failed") => ({
      source: "/a",
      target: "t",
      status,
      error: null,
      before: f(),
      outputs: [],
    });
    const r = (over: Partial<PilotResult>): PilotResult => ({
      report_path: "/r",
      files: [file("done"), file("done")],
      anomalies: 0,
      stopped_by_anomalies: false,
      ...over,
    });
    expect(pilotHeadline(r({}))).toEqual({ tone: "ok", title: "Pilot: 2 files, no mismatches with the sources" });
    expect(pilotHeadline(r({ anomalies: 1 })).tone).toBe("bad");
    expect(pilotHeadline(r({ files: [file("done"), file("failed")] })).tone).toBe("warn");
    expect(pilotHeadline(r({ anomalies: 3, stopped_by_anomalies: true })).title).toMatch(/stopped/);
  });

  it("words the confirmation per kind and blocks while a pilot waits", () => {
    const p = preview({ files: 5 });
    expect(confirmLines(p, "pilot").title).toBe("Pilot: 5 files?");
    expect(confirmLines(p, "rest").title).toBe("Run the rest: 5 files?");
    expect(confirmLines(p).title).toBe("Run the plan: 5 files?");
    expect(applyBlock({ preview: p, busy: null, pilotOpen: true })?.text).toMatch(
      /The pilot is done/,
    );
    // A dry run is not held back by it.
    expect(applyBlock({ preview: p, busy: null, pilotOpen: true }, true)).toBeNull();
  });
});

describe("syncOption", () => {
  it("is on when reachable with folders, off with a reason otherwise", () => {
    expect(syncOption({ reachable: true, reason: null, folders: ["Camera", "Shots"] })).toEqual({
      disabled: false,
      checked: true,
      note: "Folders: Camera, Shots",
    });
    expect(syncOption({ reachable: false, reason: "no answer", folders: [] })).toMatchObject({ disabled: true, checked: false });
    expect(syncOption({ reachable: false, reason: "no answer", folders: [] }).note).toContain("no answer");
    expect(syncOption({ reachable: true, reason: null, folders: [] })).toMatchObject({ disabled: true, checked: false });
    expect(syncOption(null).disabled).toBe(true);
  });
});
