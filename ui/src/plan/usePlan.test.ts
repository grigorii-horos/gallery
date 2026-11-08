import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
let running: unknown = null;
let startError: string | null = null;
let planView: unknown = null;

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd, args) => {
  switch (cmd) {
    case "current_plan":
      return planView;
    case "refresh_current":
      return true;
    case "set_overrides":
      return (args as { items: { source_path: string }[] }).items.map((i) =>
        i.source_path.includes("bad")
          ? { source_path: i.source_path, key: null, size: null, mtime_ns: null, error: "no such file" }
          : { source_path: i.source_path, key: i.source_path, size: 7, mtime_ns: 9, error: null },
      );
    case "scan_status":
      return running;
    case "start_scan":
      if (startError) throw startError;
      return null;
    case "set_override":
      return { source_path: "x", key: (args as { sourcePath: string }).sourcePath, size: 7, mtime_ns: 9, error: null };
    case "remove_override":
      return true;
    case "cancel_scan":
      return true;
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

const { usePlan, REFRESH_DEBOUNCE_MS } = await import("./usePlan");

const emit = (name: string, payload: unknown) => act(() => handlers.get(name)!({ payload }));

async function setup(configPath: string | null = "/c/gallery.toml") {
  const hook = renderHook(() => usePlan(configPath));
  await waitFor(() => expect(handlers.has("scan-done")).toBe(true));
  await waitFor(() => expect(hook.result.current.loading).toBe(false));
  return hook;
}

describe("usePlan", () => {
  beforeEach(() => {
    localStorage.clear();
    handlers.clear();
    invoke.mockClear();
    running = null;
    startError = null;
    planView = null;
  });

  it("starts one scan for two quick clicks", async () => {
    const { result } = await setup();
    await act(async () => {
      const a = result.current.startScan("/c/gallery.toml");
      const b = result.current.startScan("/c/gallery.toml");
      await Promise.all([a, b]);
    });
    expect(invoke.mock.calls.filter(([c]) => c === "start_scan")).toHaveLength(1);
    expect(result.current.scan).not.toBeNull();
  });

  it("keeps showing a running scan when the backend says one is busy", async () => {
    const { result } = await setup();
    startError = "a scan is already running";
    running = {};
    await act(() => result.current.startScan("/c/gallery.toml"));
    expect(result.current.scan).not.toBeNull();
    expect(result.current.scan!.request).toEqual({});
    expect(result.current.scanError).toBeNull();
    // Progress of that scan is shown.
    emit("scan-progress", { stage: "scan", done: 3, total: 0, current: "/s/a.jpg" });
    expect(result.current.scan!.current).toBe("/s/a.jpg");
  });

  it("reports a failed start when no scan runs", async () => {
    const { result } = await setup();
    startError = "no config";
    await act(() => result.current.startScan("/c/gallery.toml"));
    expect(result.current.scan).toBeNull();
    expect(result.current.scanError).toBe("no config");
  });

  it("reattaches to a scan that runs when the webview loads", async () => {
    running = {};
    const { result } = await setup();
    await waitFor(() => expect(result.current.scan).not.toBeNull());
    expect(result.current.scan!.request).toEqual({});
    emit("scan-error", { message: "Scan cancelled", cancelled: true });
    expect(result.current.scan).toBeNull();
  });

  it("checks decisions against the UI's config", async () => {
    const { result } = await setup("/c/other.toml");
    await act(() => result.current.setOverride("/s/a.jpg", { kind: "keep" }));
    expect(invoke).toHaveBeenCalledWith("set_override", {
      path: "/c/other.toml",
      sourcePath: "/s/a.jpg",
      action: { kind: "keep" },
    });
  });

  it("keeps the stamp of a single decision in the view", async () => {
    planView = { path: "/p/a.plan.json", plan: {}, overrides: [] };
    const { result } = await setup();
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.setOverride("/s/a.jpg", { kind: "keep" }));
    expect(result.current.view!.overrides).toEqual([
      { path: "/s/a.jpg", action: { kind: "keep" }, size: 7, mtime_ns: 9 },
    ]);
  });

  it("records a batch in one call and keeps only accepted decisions", async () => {
    planView = { path: "/p/a.plan.json", plan: {}, overrides: [{ path: "/s/a.jpg", action: { kind: "keep" }, size: 1, mtime_ns: 2 }] };
    const { result } = await setup("/c/other.toml");
    const target = { kind: "target", name: "Photos" } as const;
    let res: unknown;
    await act(async () => {
      res = await result.current.setOverrides(["/s/a.jpg", "/s/bad.jpg", "/s/b.jpg"], target);
    });
    expect(invoke).toHaveBeenCalledWith("set_overrides", {
      path: "/c/other.toml",
      items: ["/s/a.jpg", "/s/bad.jpg", "/s/b.jpg"].map((source_path) => ({ source_path, action: target })),
    });
    expect((res as { error: string | null }[]).map((r) => r.error)).toEqual([null, "no such file", null]);
    expect(result.current.view!.overrides).toEqual([
      // The file state the core stamped, to compare with the plan's.
      { path: "/s/a.jpg", action: target, size: 7, mtime_ns: 9 },
      { path: "/s/b.jpg", action: target, size: 7, mtime_ns: 9 },
    ]);
  });
});

describe("usePlan: the current plan follows the files", () => {
  const refreshes = () => invoke.mock.calls.filter(([c]) => c === "refresh_current").length;

  beforeEach(() => {
    localStorage.clear();
    handlers.clear();
    invoke.mockClear();
    running = null;
    startError = null;
    planView = null;
  });

  it("asks for an update on start, once, and shows the stored plan meanwhile", async () => {
    planView = { path: "/p/cur.plan.json", built_at: "t1", waiting_ai: 0, notes: [], plan: {}, overrides: [] };
    const { result } = await setup();
    await waitFor(() => expect(result.current.view?.built_at).toBe("t1"));
    await waitFor(() => expect(refreshes()).toBe(1));
    expect(invoke).toHaveBeenCalledWith("refresh_current", { path: "/c/gallery.toml" });
    expect(invoke).toHaveBeenCalledWith("current_plan", { path: "/c/gallery.toml" });
  });

  it("debounces the updates after quick decisions into one", async () => {
    const { result } = await setup();
    await waitFor(() => expect(refreshes()).toBe(1));
    await act(async () => {
      await result.current.setOverride("/s/a.jpg", { kind: "keep" });
      await result.current.setOverride("/s/b.jpg", { kind: "keep" });
      await result.current.removeOverride("/s/a.jpg");
      await result.current.setOverrides(["/s/c.jpg"], { kind: "keep" });
    });
    expect(refreshes()).toBe(1);
    await waitFor(() => expect(refreshes()).toBe(2), { timeout: REFRESH_DEBOUNCE_MS * 5 });
    await new Promise((r) => setTimeout(r, REFRESH_DEBOUNCE_MS * 2));
    expect(refreshes()).toBe(2);
  });

  it("asks again when the window comes back to the front", async () => {
    await setup();
    await waitFor(() => expect(refreshes()).toBe(1));
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await waitFor(() => expect(refreshes()).toBe(2), { timeout: REFRESH_DEBOUNCE_MS * 5 });
  });

  it("reads the plan again when the backend says it was rebuilt and tracks the update", async () => {
    planView = { path: "/p/cur.plan.json", built_at: "t1", waiting_ai: 2, notes: [], plan: {}, overrides: [] };
    const { result } = await setup();
    await waitFor(() => expect(result.current.view?.built_at).toBe("t1"));
    emit("current-rebuilding", null);
    expect(result.current.updating).toBe(true);
    planView = { path: "/p/cur.plan.json", built_at: "t2", waiting_ai: 0, notes: [], plan: {}, overrides: [] };
    emit("current-updated", null);
    expect(result.current.updating).toBe(false);
    await waitFor(() => expect(result.current.view?.built_at).toBe("t2"));
    emit("current-error", "config unreadable");
    expect(result.current.updateError).toBe("config unreadable");
    emit("current-updated", null);
    expect(result.current.updateError).toBeNull();
  });

  it("a finished AI scan keeps its cost and reads the plan again", async () => {
    const { result } = await setup();
    planView = { path: "/p/cur.plan.json", built_at: "t3", waiting_ai: 0, notes: [], plan: {}, overrides: [] };
    await act(() => result.current.startScan("/c/gallery.toml"));
    expect(invoke).toHaveBeenCalledWith("start_scan", { path: "/c/gallery.toml" });
    emit("scan-done", { plan_path: "/p/cur.plan.json", summary: { notes: [], ai: { input: 1, output: 1, usd: 0.1 } } });
    expect(result.current.scan).toBeNull();
    expect(result.current.lastScan?.summary.ai?.usd).toBe(0.1);
    await waitFor(() => expect(result.current.view?.built_at).toBe("t3"));
  });
});
