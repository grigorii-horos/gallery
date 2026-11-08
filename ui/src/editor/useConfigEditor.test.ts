import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOp, EditOutcome, TargetDraft } from "./model";

const target = (name: string): TargetDraft => ({
  name,
  filters: [],
  sources: [],
  description: null,
  prefilter: null,
  outputs: [],
});

const outcome = (over: Partial<EditOutcome> = {}): EditOutcome => ({
  version: "v1",
  changed: false,
  saved: false,
  error: null,
  warnings: [],
  renames: [],
  draft: { sources: [], targets: { before_ai: [target("A")], ai: [], after_ai: [] }, masks: [] },
  ...over,
});

type Args = { path: string | null; ops: EditOp[]; version?: string };
const invoke = vi.fn<(cmd: string, args: Args) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: Args) => invoke(cmd, args) }));

const { useConfigEditor } = await import("./useConfigEditor");

/// A draft that echoes the operations: the first one's target name.
function echo(_cmd: string, args: Args): Promise<EditOutcome> {
  const op = args.ops.find((o) => o.op === "update_target");
  const name = op && op.op === "update_target" ? op.target.name : "A";
  return Promise.resolve(
    outcome({
      changed: name !== "A",
      draft: { sources: [], targets: { before_ai: [target(name)], ai: [], after_ai: [] }, masks: [] },
    }),
  );
}

const rename = (name: string): EditOp => ({ op: "update_target", stage: "before_ai", index: 0, target: target(name) });

describe("useConfigEditor", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd, args) => {
      if (cmd === "config_draft") return echo(cmd, args);
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("loads, collects field edits into one operation and is dirty after the check", async () => {
    const { result } = renderHook(() => useConfigEditor("/c/gallery.toml", () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    expect(result.current.dirty).toBe(false);

    act(() => {
      result.current.edit(rename("AB"));
      result.current.edit(rename("ABC"));
    });
    expect(result.current.ops).toHaveLength(1);
    // Not checked yet: dirty by the operations alone.
    expect(result.current.dirty).toBe(true);
    await waitFor(() => expect(result.current.view?.draft.targets.before_ai[0].name).toBe("ABC"));
    expect(result.current.dirty).toBe(true);

    // Typed back to the saved value: nothing to save.
    act(() => result.current.edit(rename("A")));
    await waitFor(() => expect(result.current.view?.draft.targets.before_ai[0].name).toBe("A"));
    expect(result.current.dirty).toBe(false);
  });

  it("discards edits back to the saved draft", async () => {
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    const gen = result.current.formGen;
    await act(() => result.current.editNow(rename("B")));
    expect(result.current.view?.draft.targets.before_ai[0].name).toBe("B");
    act(() => result.current.discard());
    expect(result.current.ops).toEqual([]);
    expect(result.current.dirty).toBe(false);
    expect(result.current.view?.draft.targets.before_ai[0].name).toBe("A");
    expect(result.current.formGen).toBeGreaterThan(gen);
  });

  it("saves with the loaded version and starts clean", async () => {
    const onSaved = vi.fn();
    invoke.mockImplementation((cmd, args) => {
      if (cmd === "config_draft") return echo(cmd, args);
      if (cmd === "config_edit") return Promise.resolve(outcome({ version: "v2", changed: true, saved: true }));
      throw new Error(cmd);
    });
    const { result } = renderHook(() => useConfigEditor(null, onSaved));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.editNow(rename("B")));
    await act(() => result.current.save());
    const call = invoke.mock.calls.find(([c]) => c === "config_edit")!;
    expect(call[1].version).toBe("v1");
    expect(call[1].ops).toEqual([rename("B")]);
    expect(result.current.ops).toEqual([]);
    expect(result.current.dirty).toBe(false);
    expect(result.current.notice?.tone).toBe("ok");
    expect(onSaved).toHaveBeenCalledOnce();
  });

  it("keeps the edits when the result is invalid", async () => {
    invoke.mockImplementation((cmd, args) => {
      if (cmd === "config_draft") return echo(cmd, args);
      if (cmd === "config_edit")
        return Promise.resolve(outcome({ changed: true, saved: false, error: { message: "bad", path: "targets.before_ai[0]" } }));
      throw new Error(cmd);
    });
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.editNow(rename("B")));
    await act(() => result.current.save());
    expect(result.current.ops).toHaveLength(1);
    expect(result.current.view?.error?.message).toBe("bad");
  });

  it("reports a file changed outside and drops edits on reload", async () => {
    invoke.mockImplementation((cmd, args) => {
      if (cmd === "config_draft") return echo(cmd, args);
      if (cmd === "config_edit") return Promise.reject({ kind: "changed", message: "changed", op: null });
      throw new Error(cmd);
    });
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.editNow(rename("B")));
    await act(() => result.current.save());
    expect(result.current.failure?.kind).toBe("changed");
    act(() => result.current.reload());
    await waitFor(() => expect(result.current.failure).toBeNull());
    await waitFor(() => expect(result.current.ops).toEqual([]));
  });

  it("drops an operation the file rejects and keeps the rest", async () => {
    invoke.mockImplementation((cmd, args) => {
      if (cmd !== "config_draft") throw new Error(cmd);
      const bad = args.ops.findIndex((o) => o.op === "remove_target");
      if (bad >= 0) return Promise.reject({ kind: "op", message: "no such target", op: bad });
      return echo(cmd, args);
    });
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.editNow(rename("B")));
    await act(() => result.current.editNow({ op: "remove_target", stage: "ai", index: 3 }));
    expect(result.current.ops).toEqual([rename("B")]);
    expect(result.current.notice).toEqual({ tone: "bad", text: "Change not applied: no such target" });
    expect(result.current.view?.draft.targets.before_ai[0].name).toBe("B");
  });

  it("sends the saved version with the draft and reports a file changed meanwhile", async () => {
    invoke.mockImplementation((cmd, args) => {
      if (cmd !== "config_draft") throw new Error(cmd);
      if (args.version === "v1" && args.ops.length > 0)
        return Promise.reject({ kind: "changed", message: "changed", op: null });
      return echo(cmd, args);
    });
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    expect(invoke.mock.calls[0][1].version).toBeNull();
    await act(() => result.current.editNow(rename("B")));
    expect(invoke.mock.calls[1][1].version).toBe("v1");
    expect(result.current.failure?.kind).toBe("changed");
  });

  it("offers to move overrides to a renamed target after the save", async () => {
    const renames = [{ stage: "before_ai" as const, index: 0, from: "A", to: "B", overrides: 2 }];
    invoke.mockImplementation((cmd, args) => {
      if (cmd === "config_draft") return echo(cmd, args);
      if (cmd === "config_edit") return Promise.resolve(outcome({ version: "v2", changed: true, saved: true, renames }));
      if (cmd === "config_retarget_overrides") return Promise.resolve(2);
      throw new Error(cmd);
    });
    const { result } = renderHook(() => useConfigEditor(null, () => {}));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    await act(() => result.current.editNow(rename("B")));
    await act(() => result.current.save());
    expect(result.current.retargetable).toEqual(renames);
    await act(() => result.current.retarget());
    expect(invoke).toHaveBeenCalledWith("config_retarget_overrides", {
      path: null,
      version: "v2",
      renames: [["A", "B"]],
    });
    expect(result.current.retargetable).toEqual([]);
    expect(result.current.notice?.tone).toBe("ok");
  });
});
