import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PendingState } from "../tray/usePending";

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));

const { SettingsScreen } = await import("./SettingsScreen");

const view = { settings: { check_interval_min: 30, notify_threshold: 1, hide_to_tray: true }, autostart: false };

const pending = (over: Partial<PendingState> = {}): PendingState => ({
  snapshot: null,
  checking: false,
  error: null,
  checkNow: vi.fn(async () => {}),
  ...over,
});

describe("settings screen", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "tray_settings") return view;
      if (cmd === "set_tray_settings") return args as never;
      if (cmd === "list_ai_examples") return [];
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("shows the defaults and keeps Save off until something changes", async () => {
    render(<SettingsScreen pending={pending()} />);
    expect(await screen.findByLabelText("Check")).toHaveValue(30);
    expect(screen.getByLabelText("Notify")).toHaveValue(1);
    expect(screen.getByLabelText(/Closing the window hides/)).toBeChecked();
    expect(screen.getByLabelText(/Start at login/)).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("saves the changed values, autostart only as the user chose it", async () => {
    const user = userEvent.setup();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "tray_settings") return view;
      if (cmd === "set_tray_settings") return args as never;
      if (cmd === "list_ai_examples") return [];
      throw new Error(`unexpected ${cmd}`);
    });
    render(<SettingsScreen pending={pending()} />);
    const interval = await screen.findByLabelText("Check");
    await user.clear(interval);
    await user.type(interval, "60");
    await user.click(screen.getByLabelText(/Closing the window hides/));
    await user.click(screen.getByLabelText(/Start at login/));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(invoke).toHaveBeenCalledWith("set_tray_settings", {
      settings: { check_interval_min: 60, notify_threshold: 1, hide_to_tray: false },
      autostart: true,
    });
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("does not save on its own: loading the screen never writes", async () => {
    render(<SettingsScreen pending={pending()} />);
    await screen.findByLabelText("Check");
    // Only reads: the settings and the (read-only) list of AI examples.
    expect(invoke.mock.calls.map((c) => c[0]).sort()).toEqual(["list_ai_examples", "tray_settings"]);
  });

  it("checks now and shows the last result", async () => {
    const user = userEvent.setup();
    const checkNow = vi.fn(async () => {});
    render(
      <SettingsScreen
        pending={pending({
          checkNow,
          snapshot: { to_process: 3, need_ai: 2, total: 5, by_target: {}, checked_at: "", error: null },
        })}
      />,
    );
    expect(await screen.findByText(/5 to sort \(ready to run 3, waiting for AI 2\)/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Check now" }));
    expect(checkNow).toHaveBeenCalled();
  });

  it("shows why saving failed", async () => {
    const user = userEvent.setup();
    invoke.mockImplementation(async (cmd) => {
      if (cmd === "tray_settings") return view;
      if (cmd === "list_ai_examples") return [];
      throw "no write permission";
    });
    render(<SettingsScreen pending={pending()} />);
    await user.click(await screen.findByLabelText(/Start at login/));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("no write permission")).toBeInTheDocument();
  });
});
