import { describe, expect, it } from "vitest";
import type { EditOp, SourceDraft, TargetDraft } from "./model";
import {
  DEFAULT_SKIP,
  dropMove,
  emptyTarget,
  isDirty,
  issuesUnder,
  pathOf,
  pushOp,
  relativeTo,
  stepLabel,
} from "./model";

const src = (over: Partial<SourceDraft> = {}): SourceDraft => ({
  id: "camera",
  path: "~/Captures/Camera",
  server_path: null,
  recursive: false,
  skip: null,
  ai_prefix: null,
  ai_prefix_file: null,
  ai_suffix: null,
  ai_suffix_file: null,
  ...over,
});

const tgt = (name: string): TargetDraft => ({ ...emptyTarget(), name });

describe("pushOp", () => {
  it("merges consecutive updates of the same target", () => {
    let ops: EditOp[] = [];
    ops = pushOp(ops, { op: "update_target", index: 0, target: tgt("A") });
    ops = pushOp(ops, { op: "update_target", index: 0, target: tgt("AB") });
    expect(ops).toEqual([{ op: "update_target", index: 0, target: tgt("AB") }]);
    ops = pushOp(ops, { op: "update_target", index: 1, target: tgt("C") });
    expect(ops).toHaveLength(2);
  });

  it("follows a source through a renamed id", () => {
    let ops: EditOp[] = [];
    ops = pushOp(ops, { op: "update_source", id: "camera", source: src({ id: "c" }) });
    ops = pushOp(ops, { op: "update_source", id: "c", source: src({ id: "cam" }) });
    expect(ops).toEqual([{ op: "update_source", id: "camera", source: src({ id: "cam" }) }]);
  });

  it("keeps structural operations separate", () => {
    let ops: EditOp[] = [];
    ops = pushOp(ops, { op: "remove_target", index: 0 });
    ops = pushOp(ops, { op: "remove_target", index: 0 });
    expect(ops).toHaveLength(2);
  });

  it("does not merge an update into an earlier one across other operations", () => {
    let ops: EditOp[] = [];
    ops = pushOp(ops, { op: "update_target", index: 0, target: tgt("A") });
    ops = pushOp(ops, { op: "move_target", from: 0, to: 1 });
    ops = pushOp(ops, { op: "update_target", index: 0, target: tgt("B") });
    expect(ops).toHaveLength(3);
  });
});

describe("dirty state", () => {
  it("is dirty only when operations change the text", () => {
    expect(isDirty([], null)).toBe(false);
    expect(isDirty([{ op: "remove_source", id: "x" }], { changed: true })).toBe(true);
    // Edited back to the saved text: nothing to save.
    expect(isDirty([{ op: "remove_source", id: "x" }], { changed: false })).toBe(false);
    // Not checked yet: the operations count.
    expect(isDirty([{ op: "remove_source", id: "x" }], null)).toBe(true);
  });
});

describe("dropMove", () => {
  it("moves within the list to the final index", () => {
    // Drop item 0 before item 2: it ends at index 1.
    expect(dropMove(0, 2)).toEqual({ op: "move_target", from: 0, to: 1 });
    expect(dropMove(2, 0)?.to).toBe(0);
    // To the end of a list of 4.
    expect(dropMove(1, 4)?.to).toBe(3);
    // Dropped onto its own place: nothing.
    expect(dropMove(1, 1)).toBeNull();
    expect(dropMove(1, 2)).toBeNull();
  });
});

describe("issues by field path", () => {
  const warnings = [
    { path: "targets[0].outputs[1].dir", message: "a" },
    { path: "targets[0].outputs", message: "b" },
    { path: "targets[10]", message: "c" },
    { path: "", message: "d" },
  ];
  it("matches a path and what is below it, not a longer index", () => {
    expect(issuesUnder(warnings, "targets[0]").map((w) => w.message)).toEqual(["a", "b"]);
    expect(issuesUnder(warnings, "targets[0].outputs[1]").map((w) => w.message)).toEqual(["a"]);
    expect(issuesUnder(warnings, "targets[1]")).toEqual([]);
  });
  it("builds paths", () => {
    expect(pathOf(2)).toBe("targets[2]");
  });
});

describe("helpers", () => {
  it("labels a step with its mask", () => {
    expect(stepLabel({ preset: "avif-high", mask: null }, "@photo-original")).toEqual({
      preset: "avif-high",
      mask: "@photo-original",
      own: false,
    });
    expect(stepLabel({ preset: "avif-high", mask: ["*.png", "@x"] }, "@photo-original")).toEqual({
      preset: "avif-high",
      mask: "*.png | @x",
      own: true,
    });
  });

  it("makes a picked file relative to the config folder", () => {
    expect(relativeTo("/h/.config/gallery/prompts/a.md", "/h/.config/gallery/gallery.toml")).toBe("prompts/a.md");
    expect(relativeTo("/elsewhere/a.md", "/h/.config/gallery/gallery.toml")).toBe("/elsewhere/a.md");
  });

  it("knows the core's default skip list", () => {
    expect(DEFAULT_SKIP).toContain(".stfolder");
  });

  it("a new target places files and starts with one empty filter", () => {
    expect(emptyTarget().kind).toBe("folders");
    expect(emptyTarget("trash").kind).toBe("trash");
    expect(emptyTarget().filters).toEqual([{ sources: [], match: null, exclude: [], ai: null, expand: null, conditions: {} }]);
  });
});
