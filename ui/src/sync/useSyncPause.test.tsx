import { act, render, renderHook, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
let paused: unknown = null;
let pausedError: string | null = null;

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd) => {
  switch (cmd) {
    case "sync_paused":
      if (pausedError) throw pausedError;
      return paused;
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

import { SyncBanner } from "../components/SyncBanner";
import { useSyncPause } from "./useSyncPause";

const state = { folders: [{ id: "a", label: "Photos" }], plan: "/p", report: "/r", paused_at: "2026-11-08T10:00:00+02:00" };

beforeEach(() => {
  handlers.clear();
  paused = null;
  pausedError = null;
  invoke.mockClear();
});

describe("useSyncPause", () => {
  it("reads what is paused and again when the tray says it changed", async () => {
    const { result } = renderHook(() => useSyncPause());
    await waitFor(() => expect(handlers.has("sync-changed")).toBe(true));
    expect(result.current.paused).toBeNull();
    paused = state;
    await act(async () => handlers.get("sync-changed")!({ payload: null }));
    await waitFor(() => expect(result.current.paused).toEqual(state));
    expect(result.current.error).toBeNull();
  });

  it("surfaces an unreadable state file instead of swallowing it, and clears it once readable", async () => {
    pausedError = "sync-paused.json: expected value at line 1";
    const { result } = renderHook(() => useSyncPause());
    await waitFor(() => expect(result.current.error).toContain("sync-paused.json: expected value"));
    expect(result.current.paused).toBeNull();
    pausedError = null;
    await act(async () => handlers.get("apply-done")!({ payload: null }));
    await waitFor(() => expect(result.current.error).toBeNull());
  });

  it("shows the error in the banner even when nothing is known to be paused", async () => {
    pausedError = "no data directory";
    function Banner() {
      return <SyncBanner sync={useSyncPause()} />;
    }
    render(<Banner />);
    expect(await screen.findByRole("alert")).toHaveTextContent(/Could not tell whether Syncthing is paused: no data directory/);
  });
});
