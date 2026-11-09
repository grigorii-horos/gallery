import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOp } from "./model";

const invoke = vi.fn<(cmd: string, args: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: unknown) => invoke(cmd, args) }));
type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, h: Handler) => {
    handlers.set(name, h);
    return () => handlers.delete(name);
  },
}));
const { useEffectPreview, EFFECT_PAUSE_MS } = await import("./useEffectPreview");

const op = (name: string): EditOp => ({ op: "remove_target", index: name.length });

describe("useEffectPreview", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockResolvedValue({ files: 1, changed: 1 });
  });
  afterEach(() => vi.useRealTimers());

  it("waits for a pause, asks once for the last edits, and stays idle without edits", async () => {
    const { result, rerender } = renderHook(({ ops, active }) => useEffectPreview("/c.toml", ops, active), {
      initialProps: { ops: [op("a")], active: true },
    });
    expect(result.current.loading).toBe(true);
    await act(async () => void vi.advanceTimersByTime(EFFECT_PAUSE_MS - 10));
    rerender({ ops: [op("ab")], active: true });
    await act(async () => void vi.advanceTimersByTime(EFFECT_PAUSE_MS - 10));
    expect(invoke).not.toHaveBeenCalled();
    await act(async () => void vi.advanceTimersByTime(20));
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith("effect_preview", { configPath: "/c.toml", ops: [op("ab")], refresh: false });
    expect(result.current.loading).toBe(false);
    expect(result.current.diff).toMatchObject({ changed: 1 });

    invoke.mockClear();
    rerender({ ops: [], active: false });
    await act(async () => void vi.advanceTimersByTime(1000));
    expect(invoke).not.toHaveBeenCalled();
  });

  it("reports the slow plan-stage counting while the answer is pending, and drops it with the answer", async () => {
    let answer!: (v: unknown) => void;
    invoke.mockReturnValue(new Promise((r) => (answer = r)));
    const ops = [op("a")];
    const { result } = renderHook(() => useEffectPreview("/c.toml", ops, true));
    await act(async () => void vi.advanceTimersByTime(EFFECT_PAUSE_MS + 10));
    const emit = (payload: unknown) => act(() => handlers.get("effect-progress")!({ payload }));
    expect(result.current.counting).toBeNull();
    // Scan and EXIF stages are not the slow pass.
    await emit({ stage: "exif", done: 3, total: 10, current: null });
    expect(result.current.counting).toBeNull();
    await emit({ stage: "plan", done: 40, total: 120, current: "/m/a.jpg" });
    expect(result.current).toMatchObject({ loading: true, counting: { done: 40, total: 120 } });
    await act(async () => {
      answer({ files: 1, changed: 0 });
    });
    expect(result.current).toMatchObject({ loading: false, counting: null });
  });
});
