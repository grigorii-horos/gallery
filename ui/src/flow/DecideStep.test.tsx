import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConfigSummary, PlanView, ReviewItem, TargetSummary } from "../api";

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
  path: "/c/gallery.toml",
  valid: true,
  error: null,
  warnings: [],
  sources: [],
  targets: {
    before_ai: [target("Screenshots", "before_ai")],
    ai: [target("Documents", "ai"), target("Photos", "ai")],
    after_ai: [],
  },
  presets: { name: [], content: [], masks: [] },
  ai: { model: "m", effort: "low", jobs: 8 },
};

const item = (name: string, captured: string | null, reason = "Looks like a document"): ReviewItem => ({
  source: `/media/cam/${name}`,
  size: 1,
  mtime_ns: 0,
  suggested: "Documents",
  reason,
  captured,
  source_root: "/media/cam",
});

const nameError = (it: ReviewItem): ReviewItem => ({ ...it, name_error: true });

const base: PlanView = {
  path: "/d/plans/20261105-101500.plan.json",
  overrides: [],
  plan: {
    version: 1,
    created: "2026-11-05T10:15:00+01:00",
    side: "local",
    items: [],
    unmatched: [],
    warnings: [],
    review: [
      item("a.jpg", "2026-04-12T10:00:00"),
      item("b.jpg", "2026-04-12T10:00:40"),
      item("c.jpg", "2026-04-12T12:00:00", "Landscape or document"),
      nameError(item("noext", "2026-04-12T10:00:20", "template “Date”: no extension for {ext}")),
    ],
  },
};

let view: PlanView = base;

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd, args) => {
  switch (cmd) {
    case "current_plan":
      return { ...view, built_at: "t1", waiting_ai: 0, notes: [] };
    case "refresh_current":
      return true;
    case "load_config":
      return summary;
    case "app_status":
      throw new Error("no status in this test");
    case "builtin_presets":
      return { name: [], content: [], masks: [] };
    case "thumb":
      throw new Error("no thumbnail");
    case "start_scan":
    case "scan_status":
      return null;
    case "set_overrides":
      return (args as { items: { source_path: string }[] }).items.map((i) => ({
        source_path: i.source_path,
        key: i.source_path,
        size: 1,
        mtime_ns: 0,
        error: null,
      }));
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

async function open() {
  render(<App />);
  // “Decide” opens after “Check”; the config arrives after the plan.
  await userEvent.click(await screen.findByRole("button", { name: "Looks right — next" }));
  await screen.findAllByRole("option", { name: "Photos" });
  return screen.getAllByRole("region", { name: /^Series/ });
}

describe("“Decide”", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
    view = base;
    history.replaceState(null, "", "#flow");
  });

  it("groups review files into series and applies a target to the checked ones", async () => {
    const user = userEvent.setup();
    const series = await open();
    expect(series).toHaveLength(3);
    const first = series[0];
    const cells = within(first).getAllByRole("checkbox", { name: /jpg/ });
    expect(cells.map((c) => c.textContent)).toEqual(["jpga.jpg", "jpgb.jpg"]);
    // Everything starts checked; the group box clears and refills the series.
    const all = within(first).getByRole("checkbox", { name: "2 files" });
    expect(all).toBeChecked();
    await user.click(all);
    expect(cells.every((c) => c.getAttribute("aria-checked") === "false")).toBe(true);
    await user.click(all);
    await user.click(cells[1]);
    expect(cells[1]).toHaveAttribute("aria-checked", "false");

    // The AI suggestion is the default choice.
    const picker = within(first).getByRole("combobox", { name: "Target for the checked files" });
    expect(picker).toHaveValue("Documents");
    await user.selectOptions(picker, "Photos");
    await user.click(within(first).getByRole("button", { name: "Apply to checked (1)" }));
    expect(invoke).toHaveBeenCalledWith("set_overrides", {
      path: null,
      items: [{ source_path: "/media/cam/a.jpg", action: { kind: "target", name: "Photos" } }],
    });
    expect(await within(first).findByText("→ Photos")).toBeInTheDocument();
    expect(cells[0]).toHaveAttribute("aria-checked", "false");
    expect(screen.getByText(/1 decided/)).toBeInTheDocument();
    // The plan follows the decision by itself.
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith("refresh_current", { path: null }));
    expect(screen.queryByRole("button", { name: /Rebuild/ })).toBeNull();
  });

  it("does not count a decision made for another version of the file", async () => {
    const keep = { kind: "keep" } as const;
    view = {
      ...base,
      overrides: [
        { path: "/media/cam/a.jpg", action: keep, size: 1, mtime_ns: 0 },
        { path: "/media/cam/b.jpg", action: keep, size: 1, mtime_ns: 5 },
      ],
    };
    const series = await open();
    const first = series[0];
    expect(within(first).getAllByRole("checkbox", { name: /jpg/ }).map((c) => c.getAttribute("aria-checked"))).toEqual([
      "false",
      "true",
    ]);
    expect(within(first).getByText("→ in source")).toBeInTheDocument();
    expect(within(first).getByText("file changed, plan will refresh")).toBeInTheDocument();
    expect(screen.getByText(/1 decided/)).toBeInTheDocument();
    // b.jpg is still open, so the series is not done.
    expect(within(first).queryByText("decided")).toBeNull();
  });

  it("offers only “keep in source” for a name error", async () => {
    const user = userEvent.setup();
    const series = await open();
    const nameErr = series[2];
    expect(within(nameErr).getByText(/no extension for \{ext\}/)).toBeInTheDocument();
    expect(within(nameErr).queryByRole("combobox")).toBeNull();
    await user.click(within(nameErr).getByRole("button", { name: "Keep in source (1)" }));
    expect(invoke).toHaveBeenCalledWith("set_overrides", {
      path: null,
      items: [{ source_path: "/media/cam/noext", action: { kind: "keep" } }],
    });
  });

  it("works from the keyboard: arrows, space, number keys, Enter", async () => {
    const user = userEvent.setup();
    const series = await open();
    const [a, b] = within(series[0]).getAllByRole("checkbox", { name: /jpg/ });
    a.focus();
    await user.keyboard("{ArrowRight}");
    expect(b).toHaveFocus();
    await user.keyboard(" ");
    expect(b).toHaveAttribute("aria-checked", "false");
    await user.keyboard("{ArrowDown}");
    const c = within(series[1]).getByRole("checkbox", { name: /c\.jpg/ });
    expect(c).toHaveFocus();
    // 1 = the first target in config order (nothing used yet).
    await user.keyboard("1");
    expect(within(series[1]).getByRole("combobox")).toHaveValue("Screenshots");
    await user.keyboard("{Enter}");
    expect(invoke).toHaveBeenCalledWith("set_overrides", {
      path: null,
      items: [{ source_path: "/media/cam/c.jpg", action: { kind: "target", name: "Screenshots" } }],
    });
    // Enter applied and did not toggle the focused file.
    expect(invoke.mock.calls.filter(([cmd]) => cmd === "set_overrides")).toHaveLength(1);
  });
});
