import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConfigSummary, PlanView, ReviewItem, TargetSummary } from "../api";

const target = (name: string, ai = false): TargetSummary => ({
  name,
  kind: "folders",
  ai,
  filters: [],
  outputs: [],
});

const summary: ConfigSummary = {
  path: "/c/gallery.toml",
  valid: true,
  error: null,
  warnings: [],
  sources: [],
  targets: [target("Screenshots"), target("Documents", true), target("Photos", true)],
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
    case "confirm_series":
    case "decide_event":
      return (args as { members: { source_path: string }[] }).members.map((m) => ({
        source_path: m.source_path,
        key: m.source_path,
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

  describe("series and events", () => {
    const members = ["/media/cam/s1.jpg", "/media/cam/s2.jpg", "/media/cam/s3.jpg"];
    const inSeries = (name: string) => ({
      ...item(name, "2026-04-12T10:00:00"),
      suggested: "Photos",
      series: { members, best: members[0] },
    });
    const seriesView = (): PlanView => ({
      ...base,
      plan: {
        ...base.plan,
        review: [inSeries("s1.jpg"), inSeries("s2.jpg"), inSeries("s3.jpg"), item("late.jpg", "2026-04-12T20:00:00")],
      },
    });

    it("shows a held series side by side with the suggested best marked", async () => {
      view = seriesView();
      await open();
      const card = screen.getByRole("region", { name: "Best shot of the series s1.jpg" });
      const shots = within(card).getAllByRole("radio");
      expect(shots).toHaveLength(3);
      expect(shots.map((x) => x.getAttribute("aria-checked"))).toEqual(["true", "false", "false"]);
      expect(within(shots[0]).getByText("suggested")).toBeInTheDocument();
    });

    it("confirms another best: series_best for it, the target for the rest", async () => {
      const user = userEvent.setup();
      view = seriesView();
      await open();
      const card = screen.getByRole("region", { name: "Best shot of the series s1.jpg" });
      await user.click(within(card).getAllByRole("radio")[1]);
      await user.click(within(card).getByRole("button", { name: "Confirm best, send the others to “Photos”" }));
      expect(invoke).toHaveBeenCalledWith("confirm_series", {
        path: null,
        members: members.map((m) => ({ source_path: m, size: 1, mtime_ns: 0 })),
        best: members[1],
        target: "Photos",
      });
      expect(await within(card).findByText("→ best")).toBeInTheDocument();
      expect(within(card).getAllByText("→ Photos")).toHaveLength(2);
    });

    it("groups by event and decides a whole event at once", async () => {
      const user = userEvent.setup();
      view = seriesView();
      view.plan.review.push(nameError(item("bad", "2026-04-12T10:30:00", "no ext")), item("plain.jpg", "2026-04-12T10:10:00"));
      await open();
      expect(screen.queryByRole("region", { name: /^Event/ })).toBeNull();
      await user.click(screen.getByRole("checkbox", { name: "Group by event" }));
      const events = screen.getAllByRole("region", { name: /^Event/ });
      // 10:00-10:30 is one event; 20:00 is 9.5 h later.
      expect(events).toHaveLength(2);
      const first = events[0];
      await user.selectOptions(within(first).getByRole("combobox", { name: "Target for the whole event" }), "Photos");
      // The name-error file and the held series' members are left out of a target.
      expect(within(first).getByText(/1 files with a name that cannot be built are left out/)).toBeInTheDocument();
      expect(within(first).getByText(/3 files held for a series are left out/)).toBeInTheDocument();
      await user.click(within(first).getByRole("button", { name: "Apply to the whole event (1)" }));
      expect(invoke).toHaveBeenCalledWith("decide_event", {
        path: null,
        members: [{ source_path: "/media/cam/plain.jpg", size: 1, mtime_ns: 0 }],
        action: { kind: "target", name: "Photos" },
      });
      expect(invoke.mock.calls.filter((c) => c[0] === "decide_event")).toHaveLength(1);
    });

    it("counts a card as a series in the head and shows it in every event of its members", async () => {
      const user = userEvent.setup();
      // One series whose members fall in two events.
      const split = [
        { ...inSeries("s1.jpg"), captured: "2026-04-12T10:00:00" },
        { ...inSeries("s2.jpg"), captured: "2026-04-12T20:00:00" },
        { ...inSeries("s3.jpg"), captured: "2026-04-12T20:00:05" },
      ];
      view = { ...base, plan: { ...base.plan, review: [...split, item("late.jpg", "2026-04-12T20:30:00")] } };
      await open();
      // 4 files in the card and one ordinary series.
      expect(screen.getByText("4 files in 2 series")).toBeInTheDocument();
      await user.click(screen.getByRole("checkbox", { name: "Group by event" }));
      const events = screen.getAllByRole("region", { name: /^Event/ });
      expect(events).toHaveLength(2);
      for (const ev of events) expect(within(ev).getByRole("region", { name: "Best shot of the series s1.jpg" })).toBeInTheDocument();
      // The count is what is on screen: the card's 3 files, plus late.jpg in the second event.
      const count = (ev: HTMLElement) => ev.querySelector(".event-head .muted")!.textContent;
      expect(count(events[0])).toBe("3 files");
      expect(count(events[1])).toBe("4 files");
    });

    it("moves the best-shot pick with the arrow keys like a radio group", async () => {
      const user = userEvent.setup();
      view = seriesView();
      await open();
      const card = screen.getByRole("region", { name: "Best shot of the series s1.jpg" });
      const shots = within(card).getAllByRole("radio");
      expect(shots.map((x) => x.tabIndex)).toEqual([0, -1, -1]);
      shots[0].focus();
      await user.keyboard("{ArrowRight}");
      expect(shots[1]).toHaveFocus();
      expect(shots.map((x) => x.getAttribute("aria-checked"))).toEqual(["false", "true", "false"]);
      await user.keyboard("{ArrowLeft}{ArrowLeft}");
      // Wraps round to the last shot.
      expect(shots[2]).toHaveFocus();
      expect(shots[2]).toHaveAttribute("aria-checked", "true");
    });
  });
});
