import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOp } from "./model";

const invoke = vi.fn<(cmd: string, args: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: unknown) => invoke(cmd, args) }));
const { useEffectPreview, EFFECT_PAUSE_MS } = await import("./useEffectPreview");

const op = (name: string): EditOp => ({ op: "remove_target", stage: "ai", index: name.length });

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
});
