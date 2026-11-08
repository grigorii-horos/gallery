import { describe, expect, it } from "vitest";
import { blocker, extLabel, formatSize, placeLabel, type ChangePreview } from "./triage";

describe("triage helpers", () => {
  it("labels places and extensions", () => {
    expect(placeLabel({ source: "shots", dir: "" })).toBe("shots");
    expect(placeLabel({ source: "shots", dir: "Screenshots" })).toBe("shots/Screenshots");
    expect(extLabel("webm")).toBe(".webm");
    expect(extLabel("")).toBe("no extension");
  });

  it("formats sizes", () => {
    expect(formatSize(5)).toBe("5 B");
    expect(formatSize(1536)).toMatch(/^1\.5 KB$/);
    expect(formatSize(3 * 1024 * 1024)).toMatch(/^3\.0 MB$/);
  });

  it("explains what stops a save", () => {
    const ok: ChangePreview = { version: "v", changed: true, diff: [], error: null, warnings: [] };
    expect(blocker(null)).toBe("Checking…");
    expect(blocker(ok)).toBeNull();
    expect(blocker({ ...ok, changed: false })).toMatch(/already contains/);
    expect(blocker({ ...ok, error: { message: "name is empty", path: null } })).toBe("name is empty");
  });
});
