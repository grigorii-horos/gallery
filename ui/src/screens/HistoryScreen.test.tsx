import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ReportDetail, ReportEntry } from "../api";
import fixture from "../plan/fixture-plan.json";

const summary = { total: 4, done: 1, renamed: 0, failed: 2, stale: 1, missing: 0, not_started: 0, cancelled: false };
const local: ReportEntry = {
  path: "/d/plans/20261105-101500.plan.report.json",
  file_name: "20261105-101500.plan.report.json",
  plan_path: "/d/plans/20261105-101500.plan.json",
  plan_exists: true,
  side: "local",
  run_id: "20261105-101512",
  started: "2026-11-05T10:15:12+02:00",
  finished: "2026-11-05T10:20:00+02:00",
  archived: false,
  summary,
  undone: false,
  partially_undone: false,
  undone_items: 0,
  error: null,
};
const server: ReportEntry = {
  ...local,
  path: "/sync/plans/20261104-090000.plan.report.json",
  file_name: "20261104-090000.plan.report.json",
  plan_path: "/sync/plans/20261104-090000.plan.json",
  side: "server",
  started: "2026-11-04T09:00:00+02:00",
  summary: { ...summary, failed: 0, stale: 0, done: 4 },
};

const item = (source: string, status: string, error: string | null = null) => ({
  source,
  status,
  trashed: status === "done" ? "/s/.gallery-trash/x" : null,
  renamed: false,
  undone: false,
  error,
  outputs: [],
});

const detailOf = (e: ReportEntry): ReportDetail =>
  ({
    entry: e,
    items:
      e.side === "server"
        ? [item("/srv/a.jpg", "done")]
        : [
            item("/s/a.jpg", "done"),
            item("/s/b.mov", "failed", "ffmpeg: exit 1"),
            item("/s/c.jpg", "failed", "no space left"),
            item("/s/d.jpg", "stale"),
          ],
    warnings: [],
    undo: { items: 3, restore: 1, rename_back: 0, remove_outputs: 2 },
    server_command: e.side === "server" ? "gallery undo /srv/sync/plans/20261104-090000.plan.report.json" : null,
  }) as ReportDetail;

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd, args) => {
  const a = args as Record<string, string>;
  switch (cmd) {
    case "list_reports":
      return [server, local];
    case "load_report":
      return detailOf(a.path === server.path ? server : local);
    case "current_plan":
      return fixture;
    case "refresh_current":
      return true;
    case "apply_preview":
      return { plan_path: a.planPath, created: "", side: "local", files: 2, trash: 2, renamed: 0, kept: 0, writes: 2, review: 0, unmatched: 0, unchanged: 0, applied: null };
    case "load_config":
      throw new Error("no config in this test");
    case "app_status":
      throw new Error("no status in this test");
    case "builtin_presets":
      return { name: [], content: [], masks: [] };
    case "apply_status":
      return { running: null, last_done: null };
    case "undo_status":
    case "scan_status":
    case "start_undo":
      return null;
    case "start_retry":
      return {
        report_path: local.path,
        plan_path: "/d/plans/20261105-101500-retry.plan.json",
        failed: 2,
        files: 2,
        left_out: 0,
        preview: null,
        reused: false,
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
const calls = (cmd: string) => invoke.mock.calls.filter(([c]) => c === cmd);

describe("history screen", () => {
  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem("gallery.exportDir", "/sync/plans");
    invoke.mockClear();
    history.replaceState(null, "", "#history");
  });

  it("lists runs with status chips and filters the chosen run's files", async () => {
    const user = userEvent.setup();
    render(<App />);
    const list = await screen.findByRole("region", { name: "Runs" });
    const rows = await within(list).findAllByRole("button");
    expect(rows[0]).toHaveTextContent("November 4, 2026, 09:00");
    expect(rows[0]).toHaveTextContent("server");
    expect(rows[1]).toHaveTextContent("2 errors");
    expect(calls("list_reports")[0][1]).toEqual({ exportDir: "/sync/plans" });

    await user.click(rows[1]);
    const detail = screen.getByRole("region", { name: "Details" });
    const files = await within(detail).findByRole("list", { name: "Run files" });
    expect(within(files).getAllByRole("listitem")).toHaveLength(4);
    await user.click(within(detail).getByRole("tab", { name: /Errors/ }));
    const failed = within(detail).getByRole("list", { name: "Run files" });
    expect(within(failed).getAllByRole("listitem")).toHaveLength(2);
    expect(within(failed).getAllByText("ffmpeg: exit 1").length).toBeGreaterThan(0);
    expect(within(detail).getByRole("tab", { name: /Missing/ })).toBeDisabled();
  });

  it("server reports show the command and offer no undo or retry", async () => {
    render(<App />);
    const list = await screen.findByRole("region", { name: "Runs" });
    await within(list).findAllByRole("button");
    const detail = screen.getByRole("region", { name: "Details" });
    expect(await within(detail).findByText("gallery undo /srv/sync/plans/20261104-090000.plan.report.json")).toBeInTheDocument();
    expect(within(detail).queryByRole("button", { name: "Undo…" })).not.toBeInTheDocument();
    expect(within(detail).queryByRole("button", { name: "Retry failed…" })).not.toBeInTheDocument();
  });

  it("confirms an undo with its counts, then shows progress and the result", async () => {
    const user = userEvent.setup();
    render(<App />);
    const list = await screen.findByRole("region", { name: "Runs" });
    await user.click((await within(list).findAllByRole("button"))[1]);
    const detail = screen.getByRole("region", { name: "Details" });
    await within(detail).findByRole("list", { name: "Run files" });
    await user.click(within(detail).getByRole("button", { name: "Undo…" }));
    const dialog = screen.getByRole("dialog", { name: "Undo the run from November 5, 2026, 10:15?" });
    expect(within(dialog).getByText("1 source is restored from the trash.")).toBeInTheDocument();
    expect(within(dialog).getByText("2 copies will be deleted from the target folders.")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(calls("start_undo")).toHaveLength(0);

    await user.click(within(detail).getByRole("button", { name: "Undo…" }));
    await user.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Undo" }));
    expect(calls("start_undo")).toEqual([["start_undo", { reportPath: local.path }]]);
    act(() => handlers.get("undo-progress")!({ payload: { done: 1, total: 3, current: "/s/a.jpg" } }));
    expect(within(detail).getByText("Undo: 1 of 3")).toBeInTheDocument();
    const lists = calls("list_reports").length;
    act(() =>
      handlers.get("undo-done")!({
        payload: { report_path: local.path, restored: 1, removed_outputs: 2, problems: [], cancelled: false, undone: true },
      }),
    );
    expect(await within(detail).findByText("The run was undone")).toBeInTheDocument();
    await vi.waitFor(() => expect(calls("list_reports").length).toBe(lists + 1));
  });

  it("prepares a retry after confirmation and offers it on the main screen", async () => {
    const user = userEvent.setup();
    render(<App />);
    const list = await screen.findByRole("region", { name: "Runs" });
    await user.click((await within(list).findAllByRole("button"))[1]);
    const detail = screen.getByRole("region", { name: "Details" });
    await user.click(await within(detail).findByRole("button", { name: "Retry failed…" }));
    const dialog = screen.getByRole("dialog", { name: "Retry the failed files?" });
    expect(within(dialog).getByText(/2 failed files/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Prepare retry" }));
    expect(calls("start_retry")).toEqual([["start_retry", { reportPath: local.path }]]);
    await user.click(await within(detail).findByRole("button", { name: "Open in Sort" }));
    expect(location.hash).toBe("#flow");
    // The retry plan is a file of its own: it is previewed by its path and
    // never replaces the current plan.
    const card = await screen.findByRole("region", { name: "Retry of failed files" });
    expect(await within(card).findByRole("button", { name: "Run retry…" })).toBeEnabled();
    expect(calls("apply_preview").at(-1)![1]).toEqual({ planPath: "/d/plans/20261105-101500-retry.plan.json" });
  });
});
