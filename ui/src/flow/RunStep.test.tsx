import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApplyPreview, ConfigSummary } from "../api";
import fixture from "../plan/fixture-plan.json";

const summary: ConfigSummary = {
  path: "/home/u/.config/gallery/gallery.toml",
  valid: true,
  error: null,
  warnings: [],
  sources: [],
  targets: [],
  presets: { name: [], content: [], masks: [] },
  ai: { model: "m", effort: "low", jobs: 8 },
};

let currentId = "p1";
// The plan the preview counts are of; null: the current one.
let previewId: string | null = null;
let preview: ApplyPreview;
let split: unknown;
let pilotResult: unknown;
let status: unknown = { running: null, last_done: null };
let syncCheck: unknown = { reachable: false, reason: "Syncthing config.xml not found", folders: [] };
let syncPaused: unknown = null;
const basePreview = (): ApplyPreview => ({
  plan_path: fixture.path,
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
});

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
const subs = new Map<string, Set<Handler>>();

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd) => {
  switch (cmd) {
    case "current_plan":
      // A rebuild changes the plan id and the build time together.
      return { ...fixture, plan_id: currentId, built_at: `${fixture.built_at}#${currentId}` };
    case "refresh_current":
      return true;
    case "load_config":
      return summary;
    case "app_status":
      throw new Error("no status in this test");
    case "builtin_presets":
      return { name: [], content: [], masks: [] };
    case "apply_preview":
      return { ...preview, plan_id: previewId ?? currentId };
    case "estimate_plan":
      return {
        targets: [],
        total: {
          name: "",
          files: 120,
          input_bytes: 0,
          output_bytes: { low: 0, mid: 0, high: 0 },
          saving_bytes: { low: 1024 ** 3, mid: 2 * 1024 ** 3, high: 3 * 1024 ** 3 },
          seconds: { low: 480, mid: 720, high: 1140 },
          trash: 84,
          renamed: 30,
          kept: 6,
        },
        jobs: 4,
        source: { kind: "history", samples: 31, guessed_files: 0 },
        source_label: "from past runs (31 files)",
      };
    case "apply_status":
      return status;
    case "sync_check":
      return syncCheck;
    case "sync_paused":
      return syncPaused;
    case "sync_resume": {
      const labels = (syncPaused as { folders: { label: string }[] }).folders.map((f) => f.label);
      syncPaused = null;
      return { resumed: labels, failed: [] };
    }
    case "scan_status":
    case "start_apply":
    case "start_apply_current":
      return null;
    case "cancel_apply":
      return true;
    case "pilot_preview":
      if (split instanceof Error) throw split.message;
      return split;
    case "pilot_result":
      return pilotResult;
    case "thumb":
      return "asset://localhost/t.jpg";
    case "start_undo":
      return null;
    case "export_server_plan":
      return {
        plan_path: "/sync/plans/20261105-111500.plan.json",
        file_name: "20261105-111500.plan.json",
        command: "gallery apply <path to the plan on the server>/20261105-111500.plan.json",
        summary: { files: 3, targets: {}, review: 0, unmatched: 0, unchanged: 0, warnings: 0, notes: [], overrides_applied: 0, ai: null },
      };
    default:
      throw new Error(`unexpected ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, h: Handler) => {
    // Several hooks may listen to one event: the handler fans out to all.
    const all = subs.get(name) ?? new Set<Handler>();
    all.add(h);
    subs.set(name, all);
    handlers.set(name, (e) => all.forEach((f) => f(e)));
    return () => all.delete(h);
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: async () => "/sync/plans" }));

const { default: App } = await import("../App");

const starts = () => invoke.mock.calls.filter(([c]) => c === "start_apply_current");

/// Go through “Check” and “Decide” (the fixture has a file to decide).
async function reach() {
  const user = userEvent.setup();
  render(<App />);
  await user.click(await screen.findByRole("button", { name: "Looks right — next" }));
  await user.click(await screen.findByRole("button", { name: "Decisions made — next" }));
  const here = await screen.findByRole("region", { name: "Run" });
  await within(here).findByText("84");
  return { user, here };
}

const wholePlan = async (user: ReturnType<typeof userEvent.setup>, here: HTMLElement) => {
  await user.click(within(here).getByRole("radio", { name: "Everything at once" }));
  return within(here).getByRole("button", { name: "Run everything…" });
};

describe("“Run”", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
    currentId = "p1";
    previewId = null;
    preview = basePreview();
    status = { running: null, last_done: null };
    syncCheck = { reachable: false, reason: "Syncthing config.xml not found", folders: [] };
    syncPaused = null;
    history.replaceState(null, "", "#flow");
  });

  const doneOf = (over = {}, plan = fixture.path, dry = false) => ({
    payload: {
      plan_path: plan,
      dry_run: dry,
      report_path: dry ? null : "/d/plans/a.plan.report.json",
      summary: { total: 120, done: 120, renamed: 30, failed: 0, stale: 0, missing: 0, not_started: 0, cancelled: false },
      failures: [],
      warnings: [],
      ...over,
    },
  });

  it("asks for confirmation with the plan's counts before a real run, on the current plan", async () => {
    const { user, here } = await reach();
    await user.click(await wholePlan(user, here));
    const dialog = screen.getByRole("dialog", { name: "Run the plan: 120 files?" });
    expect(within(dialog).getByText(/84 sources go to the trash/)).toBeInTheDocument();
    expect(within(dialog).getByText(/30 will be renamed in place/)).toBeInTheDocument();
    // The estimate: saving and time as ranges, with where they come from.
    expect(await within(dialog).findByText("≈ 2.0 GB (1.0 GB – 3.0 GB)")).toBeInTheDocument();
    expect(within(dialog).getByText("≈ 12 min (8 min – 19 min)")).toBeInTheDocument();
    expect(within(dialog).getByText(/from past runs \(31 files\)/)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("estimate_plan", { planPath: fixture.path, pilot: null });
    // “Cancel” is focused and closes without running anything.
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(starts()).toHaveLength(0);

    await user.click(within(here).getByRole("button", { name: "Run everything…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Run" }));
    // The current plan runs, not a file named by the screen.
    expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p1" } }]]);

    // Progress with failures so far, then cancel and the result.
    act(() =>
      handlers.get("apply-progress")!({
        payload: { done: 30, total: 120, current: "/media/cam/x.jpg", failed: 2, skipped: 1 },
      }),
    );
    const progress = within(here).getByRole("status", { name: "Run: progress" });
    expect(within(progress).getByText("Run: 30 of 120")).toBeInTheDocument();
    expect(within(progress).getByText("errors: 2")).toBeInTheDocument();
    await user.click(within(progress).getByRole("button", { name: "Cancel" }));
    expect(invoke).toHaveBeenCalledWith("cancel_apply", undefined);
    act(() =>
      handlers.get("apply-done")!(
        doneOf(
          {
            summary: { total: 120, done: 27, renamed: 0, failed: 2, stale: 1, missing: 0, not_started: 90, cancelled: true },
            failures: [{ source: "/media/cam/bad.jpg", reason: "ffmpeg crashed" }],
          },
          "/d/plans/20261107-100000.plan.json",
        ),
      ),
    );
    expect(within(here).getByText("Stopped: 27 of 120 processed, 90 not started")).toBeInTheDocument();
    await user.click(within(here).getByRole("button", { name: "Open in History" }));
    expect(location.hash).toBe("#history");
  });

  it("never runs before the confirmation and runs a dry run without it", async () => {
    const { user, here } = await reach();
    await user.click(within(here).getByRole("button", { name: "Check (dry-run)" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: true, plan_id: "p1" } }]]);
    act(() => handlers.get("apply-done")!(doneOf({}, fixture.path, true)));
    expect(within(here).getByText("Check passed: 120 files can be processed")).toBeInTheDocument();
    // The button alone starts nothing real: only the dialog's “Run” does.
    await user.click(await wholePlan(user, here));
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(starts().filter(([, a]) => !(a as { run: { dry_run: boolean } }).run.dry_run)).toHaveLength(0);
  });

  it("reads the preview again when a real run ends; the plan is open for the next run, nothing is “already run”", async () => {
    const { here } = await reach();
    const previews = () => invoke.mock.calls.filter(([c]) => c === "apply_preview").length;
    const before = previews();
    act(() => handlers.get("apply-done")!(doneOf({}, "/d/plans/20261107-100000.plan.json")));
    await vi.waitFor(() => expect(previews()).toBe(before + 1));
    expect(within(here).queryByText(/already run/)).toBeNull();
    expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeEnabled();
  });

  it("voids the confirmation when the plan changes under it and starts nothing", async () => {
    const { user, here } = await reach();
    await user.click(await wholePlan(user, here));
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    // A rebuild produced another plan while the dialog was open.
    currentId = "p2";
    act(() => handlers.get("current-updated")!({ payload: null }));
    expect(await screen.findByText("The plan was refreshed — check it again")).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(starts()).toHaveLength(0);
    // Looking again and confirming names the new plan.
    await user.click(screen.getByRole("button", { name: "Got it" }));
    // The preview is read again for the new plan before a run can be confirmed.
    await vi.waitFor(() => expect(within(here).getByRole("button", { name: "Run everything…" })).toBeEnabled());
    await user.click(within(here).getByRole("button", { name: "Run everything…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Run" }));
    expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p2" } }]]);
  });

  it("confirms the plan its counts are of, and voids a dialog whose counts are of another plan", async () => {
    // The preview was read for p0 while the screen already shows p1.
    previewId = "p0";
    const { user, here } = await reach();
    await user.click(within(here).getByLabelText("Everything at once"));
    const go = within(here).getByRole("button", { name: "Run everything…" });
    expect(go).toBeDisabled();
    await user.click(go);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(starts()).toHaveLength(0);
  });

  it("names the previewed plan's id when the counts and the screen agree", async () => {
    const { user, here } = await reach();
    await user.click(await wholePlan(user, here));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Run" }));
    expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p1" } }]]);
  });

  it("shows the backend's “plan refreshed” refusal and does not retry by itself", async () => {
    const { user, here } = await reach();
    await user.click(await wholePlan(user, here));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Run" }));
    const stale = "Files, config or decisions changed since the check. The plan was refreshed: review it again and re-run.";
    act(() => handlers.get("apply-error")!({ payload: { message: stale } }));
    expect(await within(here).findByText(stale)).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 600));
    expect(starts()).toHaveLength(1);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("waits while the plan is being updated and refuses to run on it", async () => {
    const { here } = await reach();
    act(() => handlers.get("current-rebuilding")!({ payload: null }));
    expect(await within(here).findByText(/The plan is refreshing/)).toBeInTheDocument();
    expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeDisabled();
    act(() => handlers.get("current-updated")!({ payload: null }));
    await vi.waitFor(() => expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeEnabled());
  });

  it("shows the result of a run that ended while the view was reloaded", async () => {
    status = {
      running: null,
      last_done: {
        plan_path: "/d/plans/20261107-100000.plan.json",
        dry_run: false,
        report_path: "/r.json",
        summary: { total: 10, done: 10, renamed: 0, failed: 0, stale: 0, missing: 0, not_started: 0, cancelled: false },
        failures: [],
        warnings: [],
      },
    };
    const { here } = await reach();
    expect(await within(here).findByText("Done: 10 files processed")).toBeInTheDocument();
  });

  it("refuses to run a server plan here", async () => {
    preview = { ...basePreview(), side: "server" };
    const { here } = await reach().catch(async () => {
      const here = await screen.findByRole("region", { name: "Run" });
      return { here };
    });
    expect(await within(here).findByText(/server plan/)).toBeInTheDocument();
    expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeDisabled();
    expect(within(here).getByRole("button", { name: "Check (dry-run)" })).toBeDisabled();
  });

  it("exports a server plan to the chosen folder and shows the command", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<App />);
    await user.click(await screen.findByRole("button", { name: /For server/ }));
    const server = await screen.findByRole("region", { name: "On the server" });
    expect(within(server).getByRole("button", { name: "Export plan" })).toBeDisabled();
    await user.click(within(server).getByRole("button", { name: "Choose folder…" }));
    expect(localStorage.getItem("gallery.exportDir")).toBe("/sync/plans");
    await user.click(within(server).getByRole("button", { name: "Export plan" }));
    expect(invoke).toHaveBeenCalledWith("export_server_plan", { path: null, request: { dest_dir: "/sync/plans" } });
    const cmd = "gallery apply <path to the plan on the server>/20261105-111500.plan.json";
    expect(await within(server).findByText(cmd)).toBeInTheDocument();
    await user.click(within(server).getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith(cmd);
    expect(within(server).getByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  describe("pilot", () => {
    const pilotReport = "/d/plans/20261107-100000.plan.report.json";
    const pilotPlan = "/d/plans/20261107-100000.plan.json";
    const facts = (over = {}) => ({ size: 1000, mtime: "2026-04-12 10:33:14", capture: "2026-04-12 10:33:14", gps: true, ...over });
    const done = (over = {}) =>
      doneOf({
        report_path: pilotReport,
        summary: { total: 5, done: 5, renamed: 0, failed: 0, stale: 0, missing: 0, not_started: 0, cancelled: false },
        ...over,
      }, pilotPlan);
    const doneOf = (over: object, plan: string) => ({
      payload: {
        plan_path: plan,
        dry_run: false,
        report_path: pilotReport,
        summary: { total: 5, done: 5, renamed: 0, failed: 0, stale: 0, missing: 0, not_started: 0, cancelled: false },
        failures: [],
        warnings: [],
        ...over,
      },
    });

    beforeEach(() => {
      split = {
        plan_id: "p1",
        pilot_id: "s1",
        per_target: 5,
        pilot_files: 5,
        rest_files: 115,
        preview: { ...basePreview(), files: 5, trash: 5, renamed: 0, kept: 0, writes: 5 },
      };
      pilotResult = {
        report_path: pilotReport,
        anomalies: 0,
        stopped_by_anomalies: false,
        files: [
          {
            source: "/media/cam/one.jpg",
            target: "Timeline",
            status: "done",
            error: null,
            before: facts({ size: 3_000_000 }),
            outputs: [
              {
                planned: "/out/one.avif",
                actual: "/out/one.avif",
                error: null,
                anomalies: [],
                after: facts({ size: 400_000 }),
              },
            ],
          },
        ],
      };
    });

    it("the pilot is the default: chosen, primary, and the whole plan is the other option", async () => {
      const { here } = await reach();
      expect(within(here).getByRole("radio", { name: /Pilot: 5 files per target/ })).toBeChecked();
      expect(within(here).getByRole("radio", { name: "Everything at once" })).not.toBeChecked();
      expect(within(here).getByRole("button", { name: /Run pilot/ })).toHaveClass("primary");
      // The estimate is for the pilot slice.
      await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("estimate_plan", { planPath: fixture.path, pilot: 5 }));
    });

    it("confirms the pilot, shows each file before and after, then continues with the rest", async () => {
      const { user, here } = await reach();
      await user.click(within(here).getByRole("button", { name: /Run pilot/ }));
      expect(invoke).toHaveBeenCalledWith("pilot_preview", { planPath: fixture.path, perTarget: 5 });
      const dialog = await screen.findByRole("dialog", { name: "Pilot: 5 files?" });
      expect(starts()).toHaveLength(0);
      // Syncthing is checked for the pilot slice only.
      await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("sync_check", { planPath: fixture.path, pilot: 5 }));
      await user.click(within(dialog).getByRole("checkbox", { name: /Allow results with mismatches/ }));
      await user.click(within(dialog).getByRole("button", { name: "Run" }));
      expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p1", pilot_id: "s1", allow_anomalies: true, pilot: 5 } }]]);

      act(() => handlers.get("apply-done")!(done()));
      const panel = await within(here).findByRole("region", { name: "Pilot result" });
      expect(await within(panel).findByText("Pilot: 1 file, no mismatches with the sources")).toBeInTheDocument();
      expect(invoke).toHaveBeenCalledWith("pilot_result", { reportPath: pilotReport });
      expect(within(panel).getByText("/media/cam/one.jpg")).toBeInTheDocument();
      expect(within(panel).getByText("2.9 MB → 390.6 KB")).toBeInTheDocument();
      expect(within(panel).getAllByText("2026-04-12 10:33:14 → 2026-04-12 10:33:14", { selector: "dd" })).toHaveLength(2);
      expect(within(panel).getByText("yes → yes")).toBeInTheDocument();
      // Nothing else starts while the pilot waits for the decision.
      expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeDisabled();

      // The rest is the rebuilt current plan.
      preview = { ...basePreview(), files: 115 };
      await user.click(within(panel).getByRole("button", { name: "Continue with the rest" }));
      const confirm = await screen.findByRole("dialog", { name: "Run the rest: 115 files?" });
      await user.click(within(confirm).getByRole("button", { name: "Run" }));
      expect(starts()[1]).toEqual(["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p1" } }]);
      await vi.waitFor(() => expect(within(here).queryByRole("region", { name: "Pilot result" })).not.toBeInTheDocument());
    });

    it("rolls the pilot back with the existing undo", async () => {
      const { user, here } = await reach();
      await user.click(within(here).getByRole("button", { name: /Run pilot/ }));
      await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Run" }));
      act(() => handlers.get("apply-done")!(done()));
      const panel = await within(here).findByRole("region", { name: "Pilot result" });
      await user.click(await within(panel).findByRole("button", { name: "Undo pilot" }));
      expect(invoke).toHaveBeenCalledWith("start_undo", { reportPath: pilotReport });
      act(() =>
        handlers.get("undo-done")!({
          payload: { report_path: pilotReport, restored: 5, removed_outputs: 5, problems: [], cancelled: false, undone: true },
        }),
      );
      await vi.waitFor(() => expect(within(here).queryByRole("region", { name: "Pilot result" })).not.toBeInTheDocument());
      expect(within(here).getByRole("button", { name: /Run pilot/ })).toBeEnabled();
    });

    it("flags anomalies and a run stopped by them", async () => {
      pilotResult = {
        report_path: pilotReport,
        anomalies: 3,
        stopped_by_anomalies: true,
        files: [
          {
            source: "/media/cam/one.jpg",
            target: "Timeline",
            status: "failed",
            error: null,
            before: facts(),
            outputs: [
              {
                planned: "/out/one.avif",
                actual: null,
                error: "the result differs from the source",
                anomalies: ["capture date: 2026-04-12 10:33:14 in the source, none in the result"],
                after: { size: null, mtime: null, capture: null, gps: null },
              },
            ],
          },
        ],
      };
      const { user, here } = await reach();
      await user.click(within(here).getByRole("button", { name: /Run pilot/ }));
      await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Run" }));
      act(() =>
        handlers.get("apply-done")!(
          done({ summary: { total: 5, done: 2, renamed: 0, failed: 3, stale: 0, missing: 0, not_started: 0, cancelled: true, stopped_by_anomalies: true, anomalies: 3 } }),
        ),
      );
      const panel = await within(here).findByRole("region", { name: "Pilot result" });
      expect(await within(panel).findByText(/Pilot stopped: results differ from the sources \(3\)/)).toBeInTheDocument();
      expect(within(panel).getByText(/Mismatch: capture date/)).toBeInTheDocument();
      // No way on until the user accepts the anomalies.
      expect(within(panel).queryByRole("button", { name: "Continue with the rest" })).not.toBeInTheDocument();
      await user.click(within(panel).getByRole("checkbox", { name: /despite mismatches/ }));
      expect(within(panel).getByRole("button", { name: "Continue with the rest" })).toBeEnabled();
    });

    it("shows why the pilot cannot start", async () => {
      split = new Error("The plan has so few files that a pilot would cover everything.");
      const { user, here } = await reach();
      await user.click(within(here).getByRole("button", { name: /Run pilot/ }));
      expect(await within(here).findByText(/pilot would cover everything/)).toBeInTheDocument();
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
  });

  describe("Syncthing pause", () => {
    const pausedState = {
      folders: [
        { id: "cam", label: "Camera" },
        { id: "shots", label: "Screenshots" },
      ],
      plan: "/d/plans/a.plan.json",
      report: "/d/plans/a.plan.report.json",
      paused_at: "2026-11-07T10:00:00+02:00",
    };

    it("offers the pause on by default, with the folders, and sends pause_sync", async () => {
      syncCheck = { reachable: true, reason: null, folders: ["Camera", "Screenshots"] };
      const { user, here } = await reach();
      await user.click(await wholePlan(user, here));
      const dialog = screen.getByRole("dialog");
      const box = await within(dialog).findByRole("checkbox", { name: "Pause Syncthing during the run" });
      await waitFor(() => expect(box).toBeEnabled());
      expect(box).toBeChecked();
      expect(within(dialog).getByText("Folders: Camera, Screenshots")).toBeInTheDocument();
      await user.click(within(dialog).getByRole("button", { name: "Run" }));
      expect(starts()).toEqual([["start_apply_current", { path: null, run: { dry_run: false, plan_id: "p1", pause_sync: true } }]]);
    });

    it("lets the user turn the pause off", async () => {
      syncCheck = { reachable: true, reason: null, folders: ["Camera"] };
      const { user, here } = await reach();
      await user.click(await wholePlan(user, here));
      const dialog = screen.getByRole("dialog");
      const box = await within(dialog).findByRole("checkbox", { name: /Pause Syncthing/ });
      await waitFor(() => expect(box).toBeEnabled());
      await user.click(box);
      await user.click(within(dialog).getByRole("button", { name: "Run" }));
      expect(starts()[0][1]).toEqual({ path: null, run: { dry_run: false, plan_id: "p1" } });
    });

    it("disables the option with a reason when Syncthing is unreachable and still runs", async () => {
      const { user, here } = await reach();
      await user.click(await wholePlan(user, here));
      const dialog = screen.getByRole("dialog");
      expect(await within(dialog).findByText(/Syncthing unavailable: Syncthing config.xml not found/)).toBeInTheDocument();
      const box = within(dialog).getByRole("checkbox", { name: /Pause Syncthing/ });
      expect(box).toBeDisabled();
      expect(box).not.toBeChecked();
      await user.click(within(dialog).getByRole("button", { name: "Run" }));
      expect(starts()).toHaveLength(1);
    });

    it("shows the banner at start when the state file exists and resumes from it", async () => {
      syncPaused = pausedState;
      const user = userEvent.setup();
      render(<App />);
      expect(await screen.findByText("Syncthing paused: Camera, Screenshots")).toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "Resume sync" }));
      await waitFor(() => expect(screen.queryByText(/Syncthing paused/)).not.toBeInTheDocument());
      expect(invoke).toHaveBeenCalledWith("sync_resume", undefined);
    });

    it("offers “Resume sync” in the result after a run", async () => {
      const { user, here } = await reach();
      expect(screen.queryByText(/Syncthing paused/)).not.toBeInTheDocument();
      await user.click(await wholePlan(user, here));
      await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Run" }));
      syncPaused = pausedState;
      act(() => handlers.get("apply-done")!(doneOf()));
      const banner = await within(here).findByText("Syncthing paused: Camera, Screenshots");
      expect(banner).toBeInTheDocument();
      expect(within(here).getByRole("button", { name: "Resume sync" })).toBeEnabled();
    });
  });
});
