import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";


/** The declarations of the last rule whose selector list contains `selector`. */
function rule(selector: string): string {
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let found = "";
  for (const m of css.matchAll(re)) {
    if (m[1].split(/,(?![^(]*\))/).some((s: string) => s.trim() === selector)) found += m[2];
  }
  return found;
}

const css = readFileSync(join(process.cwd(), "src/styles.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

describe("two-pane screens scroll per pane", () => {
  it.each([".editor-screen .preset-edit > *", ".editor-screen .targets > *", ".editor-screen .split > *", ".history > *"])(
    "%s is its own scroll container",
    (sel) => {
      const r = rule(sel);
      expect(r).toContain("min-height: 0");
      expect(r).toContain("overflow-y: auto");
    },
  );

  it.each([".editor-screen .preset-edit", ".editor-screen .targets", ".editor-screen .split"])("%s fills the screen", (sel) => {
    const r = rule(sel);
    expect(r).toContain("min-height: 0");
    expect(r).toContain("grid-template-rows: minmax(0, 1fr)");
  });

  it("the content area does not scroll when it holds such a screen", () => {
    expect(rule(".content:has(> .editor-screen, > .history)")).toContain("overflow: hidden");
  });

  it("the save bar stays in view", () => {
    expect(rule(".content > .editor-screen .editor-bar")).toContain("position: static");
  });
});
