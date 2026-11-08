import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConfigSummary, TargetSummary } from "../api";
import fixture from "../plan/fixture-plan.json";

const target = (name: string, stage: TargetSummary["stage"]): TargetSummary => ({
  name,
  stage,
  filters: [],
  sources: [],
  description: null,
  prefilter: null,
  outputs: [],
});

const summary: ConfigSummary = {
  path: "/home/u/.config/gallery/gallery.toml",
  valid: true,
  error: null,
  warnings: [],
  sources: [],
  targets: {
    before_ai: [target("Screenshots", "before_ai")],
    ai: [target("Documents", "ai")],
    after_ai: [target("Videos", "after_ai")],
  },
  presets: { name: [], content: [], masks: [] },
  ai: { model: "m", effort: "low", jobs: 8 },
};

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();

let aiEstimate: unknown = null;
let waitingAi = 0;
const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd, args) => {
  switch (cmd) {
    case "estimate_ai_scan":
      if (aiEstimate === null) throw new Error("no estimate");
      return aiEstimate;
    case "current_plan":
      return { ...fixture, waiting_ai: waitingAi };
    case "refresh_current":
      return true;
    case "load_config":
      return { ...summary };
    case "app_status":
      throw new Error("no status in this test");
    case "builtin_presets":
      return { name: [], content: [], masks: [] };
    case "thumb":
      throw new Error("no thumbnail");
    case "start_scan":
    case "scan_status":
      return null;
    case "set_override":
      return (args as { sourcePath: string }).sourcePath;
    case "cancel_scan":
      return true;
    case "triage_view":
      return {
        version: "v1",
        total: 1,
        groups: [
          {
            source: "cam",
            dir: "Clips",
            ext: "mp4",
            count: 1,
            size: 2048,
            samples: ["/media/cam/Clips/x.mp4"],
            reason: "Extension .mp4 is not in the match [@photo-original]",
            actions: [
              {
                kind: "skip_source",
                label: "skip",
                target: null,
                dir: null,
                ops: [{ op: "update_source", id: "cam" }],
              },
            ],
          },
        ],
      };
    case "config_preview_change":
      return {
        version: "v1",
        changed: true,
        diff: [{ kind: "add", text: 'skip = [".stfolder", "*.mp4"]' }],
        error: null,
        warnings: [],
      };
    case "config_edit":
      return { saved: true, error: null, version: "v2" };
    case "explain_file":
      return {
        path: (args as { path: string }).path,
        scope: { kind: "scanned", source: "cam" },
        stages: [],
        other_targets: [],
        ai: { kind: "no_candidates" },
        decision: { kind: "unmatched", target: null, via: null, detail: null },
        outcome: "unmatched",
        outputs: [],
        verdict: "Extension .mp4 is not in the match [@photo-original]",
      };
    default:
      throw new Error(`unexpected ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, h: Handler) => {
    handlers.set(name, h);
    return () => handlers.delete(name);
  },
}));

const { default: App } = await import("../App");

const refreshes = () => invoke.mock.calls.filter(([c]) => c === "refresh_current").length;

describe("“Check”", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
    waitingAi = 0;
    aiEstimate = null;
    history.replaceState(null, "", "#flow");
  });

  it("summarizes the current plan by target and other groups, without a plan time", async () => {
    render(<App />);
    const tray = await screen.findByRole("navigation", { name: "Plan summary" });
    const rows = within(tray)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(rows).toEqual(["Screenshots1", "Documents2", "Videos1", "To decide1", "Unmatched1", "Unchanged2"]);
    expect(within(tray).getByText("Warnings")).toBeInTheDocument();
    // The plan is the current one: no file name, no "plan from …".
    expect(screen.queryByText(/Plan from/)).toBeNull();
    expect(screen.queryByText(/\.plan\.json/)).toBeNull();
    expect(invoke).toHaveBeenCalledWith("current_plan", { path: null });
    await vi.waitFor(() => expect(refreshes()).toBeGreaterThan(0));
  });

  it("states the status line", async () => {
    waitingAi = 3;
    render(<App />);
    const status = await screen.findByRole("status", { name: "Plan status" });
    expect(await within(status).findByText("3 waiting for AI")).toBeInTheDocument();
    expect(within(status).getByText("1 unmatched")).toBeInTheDocument();
    expect(within(status).getByText("2 unchanged")).toBeInTheDocument();
  });

  it("shows a group's files and the trace of a file, then records a new target and refreshes the plan", async () => {
    const user = userEvent.setup();
    render(<App />);
    const tray = await screen.findByRole("navigation", { name: "Plan summary" });
    // The first target is open at start.
    expect(within(tray).getByRole("button", { name: /Screenshots/ })).toHaveAttribute("aria-pressed", "true");
    await user.click(within(tray).getByRole("button", { name: /Documents/ }));
    const grid = screen.getByRole("listbox", { name: "Documents" });
    const cells = within(grid).getAllByRole("option");
    // Extension stands in for a missing thumbnail; "?" marks the low-confidence AI answer.
    expect(cells.map((c) => c.textContent)).toEqual(["jpgIMG_20260412_103314.jpg", "jpg?IMG_20260413_090000.jpg"]);

    await user.click(cells[0]);
    const trace = screen.getByRole("complementary", { name: "File trace" });
    expect(within(trace).getByText("AI: “Documents”")).toBeInTheDocument();
    expect(within(trace).getByText("Passport photo on a table")).toBeInTheDocument();
    expect(within(trace).getByText("Write a new file")).toBeInTheDocument();
    expect(within(trace).getByText("avif-high")).toBeInTheDocument();
    expect(within(trace).getByText("Prefix").closest("li")).toHaveClass("miss");
    expect(within(trace).getByText("/lib/docs/2026/04/20260412_103314.avif")).toBeInTheDocument();

    await vi.waitFor(() => expect(refreshes()).toBeGreaterThan(0));
    const before = refreshes();
    await user.click(within(trace).getByRole("button", { name: "Change target…" }));
    await user.click(within(trace).getByRole("button", { name: /Keep in source/ }));
    expect(invoke).toHaveBeenCalledWith("set_override", {
      path: null,
      sourcePath: "/media/cam/IMG_20260412_103314.jpg",
      action: { kind: "keep" },
    });
    expect(await within(trace).findByText(/Decision saved: keep in source/)).toBeInTheDocument();
    expect(within(trace).getByRole("button", { name: "Remove manual decision" })).toBeInTheDocument();

    // No "Rebuild plan": the plan follows the decision by itself.
    expect(screen.queryByRole("button", { name: /Rebuild/ })).toBeNull();
    await vi.waitFor(() => expect(refreshes()).toBe(before + 1));
  });

  it("“Unmatched” opens the triage; saving an action updates the plan by itself", async () => {
    const user = userEvent.setup();
    render(<App />);
    const tray = await screen.findByRole("navigation", { name: "Plan summary" });
    await user.click(within(tray).getByRole("button", { name: /Unmatched/ }));
    expect(await screen.findByText(/is not in the match/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "cam/Clips .mp4" }));
    await user.click(screen.getByRole("button", { name: "Always ignore in the whole source" }));
    expect(await screen.findByLabelText("TOML change")).toHaveTextContent('+ skip = [".stfolder", "*.mp4"]');
    await vi.waitFor(() => expect(refreshes()).toBeGreaterThan(0));
    const before = refreshes();
    await user.click(screen.getByRole("button", { name: "Save to config" }));
    await vi.waitFor(() => expect(refreshes()).toBe(before + 1));
    expect(screen.queryByText(/The config changed, the plan does not account for it yet/)).toBeNull();
    expect(screen.queryByRole("button", { name: /Rebuild/ })).toBeNull();
  });

  it("finds a file by name and explains it", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("navigation", { name: "Plan summary" });
    await user.type(screen.getByRole("searchbox", { name: "Find file" }), "IMG_20260412");
    const hits = screen.getByRole("listbox", { name: "Matching files" });
    await user.click(within(hits).getAllByRole("button")[0]);
    const panel = await screen.findByRole("complementary", { name: "Why this file" });
    expect(await within(panel).findByText("Extension .mp4 is not in the match [@photo-original]")).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("explain_file", {
      configPath: null,
      path: "/media/cam/IMG_20260412_103314.jpg",
    });
  });

  it("asks “Why?” from a file's trace", async () => {
    const user = userEvent.setup();
    render(<App />);
    const tray = await screen.findByRole("navigation", { name: "Plan summary" });
    await user.click(within(tray).getByRole("button", { name: /Unmatched/ }));
    await user.click(screen.getByRole("button", { name: "All files" }));
    await user.click(within(screen.getByRole("listbox", { name: "Unmatched" })).getAllByRole("option")[0]);
    await user.click(screen.getByRole("button", { name: "Why?" }));
    expect(await screen.findByRole("complementary", { name: "Why this file" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("explain_file", expect.objectContaining({ configPath: null }));
  });
});

describe("“Ask AI”", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
    waitingAi = 360;
    aiEstimate = null;
    history.replaceState(null, "", "#flow");
  });

  const estimateOf = (label: string, confirm: boolean, mid = 1.4) => ({
    candidates: 400,
    cached: 40,
    to_send: 360,
    model: "m",
    usd: { low: mid, mid, high: mid * 2 },
    source_label: "rough estimate",
    label,
    confirm,
  });
  const scanStarts = () => invoke.mock.calls.filter(([c]) => c === "start_scan");
  const askButton = (name: RegExp) => screen.findByRole("button", { name });

  it("is the only scan: no button while nothing waits for the AI", async () => {
    waitingAi = 0;
    render(<App />);
    await screen.findByRole("navigation", { name: "Plan summary" });
    expect(screen.queryByRole("button", { name: /Ask AI/ })).toBeNull();
    expect(scanStarts()).toHaveLength(0);
  });

  it("names the files and the price, and starts a cheap request without asking", async () => {
    aiEstimate = estimateOf("≈ $0.40 (360 files, 40 from cache)", false, 0.4);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await askButton(/Ask AI \(360 files, ≈\$0\.40\)/));
    expect(invoke).toHaveBeenCalledWith("estimate_ai_scan", { path: null });
    expect(scanStarts()).toEqual([["start_scan", { path: null }]]);
  });

  it("asks to confirm a request estimated above $1 and starts nothing until then", async () => {
    aiEstimate = estimateOf("≈ $1.40 (360 files, 40 from cache)", true);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await askButton(/≈\$1\.40/));
    const ask = await screen.findByRole("alertdialog", { name: "Confirm AI cost" });
    expect(within(ask).getByText(/≈ \$1.40 \(360 files, 40 from cache\)/)).toBeInTheDocument();
    expect(scanStarts()).toHaveLength(0);
    await user.click(within(ask).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(scanStarts()).toHaveLength(0);
    await user.click(await askButton(/≈\$1\.40/));
    await user.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Ask AI" }));
    expect(scanStarts()).toHaveLength(1);
  });

  it("asks anyway when the estimate fails", async () => {
    aiEstimate = null;
    const user = userEvent.setup();
    render(<App />);
    await user.click(await askButton(/Ask AI \(360 files\)/));
    await vi.waitFor(() => expect(scanStarts()).toHaveLength(1));
  });

  it("draws the progress per stage and cancels", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(await askButton(/Ask AI/));
    const progress = screen.getByRole("status", { name: "AI request progress" });
    const { act } = await import("@testing-library/react");
    act(() =>
      handlers.get("scan-progress")!({ payload: { stage: "exif", done: 50, total: 200, current: "/media/cam/x.jpg" } }),
    );
    expect(within(progress).getByText("50 of 200")).toBeInTheDocument();
    expect(within(progress).getByText("/media/cam/x.jpg")).toBeInTheDocument();
    await user.click(within(progress).getByRole("button", { name: "Cancel" }));
    expect(invoke).toHaveBeenCalledWith("cancel_scan", undefined);
    act(() => handlers.get("scan-error")!({ payload: { message: "Scan cancelled", cancelled: true } }));
    expect(screen.queryByRole("status", { name: "AI request progress" })).not.toBeInTheDocument();
    expect(screen.getByText("Scan cancelled.")).toBeInTheDocument();
  });
});
