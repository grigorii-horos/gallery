import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConfigSummary } from "../api";
import fixture from "../plan/fixture-plan.json";

const summary: ConfigSummary = {
  path: "/home/u/.config/gallery/gallery.toml",
  valid: true,
  error: null,
  warnings: [],
  sources: [],
  targets: { before_ai: [], ai: [], after_ai: [] },
  presets: { name: [], content: [], masks: [] },
  ai: { model: "m", effort: "low", jobs: 8 },
};

let view: unknown = null;
const planWith = (review: boolean) => ({
  ...fixture,
  plan: { ...fixture.plan, review: review ? fixture.plan.review : [] },
});

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd) => {
  switch (cmd) {
    case "current_plan":
      return view;
    case "refresh_current":
      return true;
    case "load_config":
      return { ...summary };
    case "builtin_presets":
      return { name: [], content: [], masks: [] };
    case "apply_preview":
      return {
        plan_path: fixture.path,
        created: "",
        side: "local",
        files: 4,
        trash: 4,
        renamed: 0,
        kept: 0,
        writes: 4,
        review: 0,
        unmatched: 0,
        unchanged: 0,
        applied: null,
      };
    case "estimate_plan":
      throw new Error("no estimate");
    case "apply_status":
      return { running: null, last_done: null };
    case "sync_paused":
      return null;
    case "sync_check":
      return { reachable: false, reason: "none", folders: [] };
    case "thumb":
      throw new Error("no thumbnail");
    case "triage_view":
      return null;
    default:
      throw new Error(`unexpected ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

const { default: App } = await import("../App");

/// The header button of a step (the only buttons that say whether they are expanded).
const head = (name: string) =>
  screen.queryAllByRole("button", { name: new RegExp(`^${name}`) }).find((b) => b.hasAttribute("aria-expanded")) ?? null;

describe("Sort", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
    history.replaceState(null, "", "#flow");
  });

  it("is the first screen and keeps “Run” closed until the plan is checked", async () => {
    view = planWith(false);
    render(<App />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Sort");
    await screen.findByRole("navigation", { name: "Plan summary" });
    expect(head("Check")).toBeEnabled();
    expect(head("Run")).toBeDisabled();
    expect(await screen.findByText("Check the plan first.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Run pilot/ })).toBeNull();
  });

  it("goes straight from “Check” to “Run” when nothing needs a decision", async () => {
    view = planWith(false);
    const user = userEvent.setup();
    render(<App />);
    await user.click(await screen.findByRole("button", { name: "Looks right — next" }));
    expect(head("Decide")).toBeNull();
    expect(head("Run")).toBeEnabled();
    expect(await screen.findByRole("button", { name: /Run pilot/ })).toBeInTheDocument();
    // The checked step collapsed.
    expect(screen.queryByRole("navigation", { name: "Plan summary" })).toBeNull();
    expect(head("Check")).toHaveAttribute("aria-expanded", "false");
  });

  it("adds “Decide” when files need a decision and holds “Run” behind it", async () => {
    view = planWith(true);
    const user = userEvent.setup();
    render(<App />);
    await screen.findByRole("navigation", { name: "Plan summary" });
    expect(head("Decide")).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Looks right — next" }));
    expect(head("Decide")).toBeEnabled();
    expect(head("Run")).toBeDisabled();
    expect(screen.getByText("Decide the uncertain files first.")).toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Decisions made — next" }));
    expect(head("Run")).toBeEnabled();
    expect(await screen.findByRole("button", { name: /Run pilot/ })).toBeInTheDocument();
  });

  it("never asks the AI by itself", async () => {
    view = { ...planWith(false), waiting_ai: 5 };
    render(<App />);
    await screen.findByRole("navigation", { name: "Plan summary" });
    expect(await screen.findByRole("button", { name: /Ask AI/ })).toBeInTheDocument();
    expect(invoke.mock.calls.some(([c]) => c === "start_scan")).toBe(false);
    const status = screen.getByRole("status", { name: "Plan status" });
    expect(within(status).getByText("5 waiting for AI")).toBeInTheDocument();
  });

  it("says the plan is being built while there is none", async () => {
    view = null;
    render(<App />);
    expect(await screen.findByText(/The plan builds itself/)).toBeInTheDocument();
    expect(screen.getByText("No plan yet")).toBeInTheDocument();
  });
});
