import { describe, expect, it } from "vitest";
import {
  coreDefault,
  defaultBundle,
  defaultContent,
  fieldsOf,
  limitFor,
  newNameError,
  parseSamples,
  switchKind,
  validateBundle,
  validateContent,
  validateMask,
  validateName,
  type Limit,
} from "./presets";
import { pushOp, type ContentPresetDraft, type NamePresetDraft } from "./model";

const LIMITS: Limit[] = [
  { kind: "photo", variant: null, key: "quality", min: 0, max: 100 },
  { kind: "photo", variant: "avif", key: "speed", min: 0, max: 10 },
  { kind: "photo", variant: "webp", key: "speed", min: 0, max: 6 },
  { kind: "video", variant: null, key: "crf", min: 0, max: 63 },
  { kind: "video", variant: null, key: "preset", min: 0, max: 13 },
  { kind: "audio", variant: "mp3", key: "kbps", min: 8, max: 320 },
  { kind: "audio", variant: "opus", key: "kbps", min: 6, max: 510 },
];

const photo = (over: Partial<ContentPresetDraft> = {}): ContentPresetDraft => ({
  ...defaultContent("photo", "p"),
  ...over,
});

describe("content preset form", () => {
  it("accepts the defaults of every kind", () => {
    for (const k of ["metadata", "photo", "video", "audio", "png"] as const) {
      expect(validateContent(defaultContent(k, "x"), LIMITS)).toEqual({});
    }
  });

  it("checks ranges by kind and variant", () => {
    expect(validateContent(photo({ quality: 101 }), LIMITS).quality).toMatch(/0.*100/);
    expect(validateContent(photo({ speed: 10 }), LIMITS)).toEqual({});
    // WebP's method stops at 6.
    expect(validateContent(photo({ format: "webp", speed: 7 }), LIMITS).speed).toMatch(/0.*6/);
    const v = { ...defaultContent("video", "v"), crf: 64, preset: -1 };
    const e = validateContent(v, LIMITS);
    expect(e.crf).toMatch(/63/);
    expect(e.preset).toMatch(/13/);
    const a = { ...defaultContent("audio", "a"), codec: "mp3", kbps: 500 };
    expect(validateContent(a, LIMITS).kbps).toMatch(/320/);
    expect(validateContent({ ...a, codec: "opus" }, LIMITS)).toEqual({});
  });

  it("rejects fractions and unknown choices, allows absent values", () => {
    expect(validateContent(photo({ quality: 50.5 }), LIMITS).quality).toMatch(/integer/);
    expect(validateContent(photo({ format: "jpeg" }), LIMITS).format).toBeTruthy();
    expect(validateContent(photo({ quality: null, speed: null }), LIMITS)).toEqual({});
  });

  it("picks the variant's limit, with the core default variant when absent", () => {
    expect(limitFor(LIMITS, photo({ format: null }), "speed")?.max).toBe(10);
    expect(limitFor(LIMITS, photo({ format: "webp" }), "speed")?.max).toBe(6);
  });

  it("switching the kind keeps the name and mask and fills the new kind", () => {
    const p = photo({ mask: ["*.jpg"] });
    const v = switchKind(p, "video");
    expect(v.kind).toBe("video");
    expect(v.name).toBe("p");
    expect(v.mask).toEqual(["*.jpg"]);
    expect(v.crf).toBe(30);
    expect(fieldsOf("video").map((f) => f.key)).toContain("crf");
    expect(fieldsOf("png").map((f) => f.key)).toEqual(["level"]);
  });

  it("knows the core defaults of absent keys", () => {
    expect(coreDefault("video", "max_short_side")).toBe(1080);
    expect(coreDefault("photo", "max_short_side")).toBe(null);
    expect(coreDefault("metadata", "write_capture_date")).toBe(true);
  });
});

describe("name preset form", () => {
  const n: NamePresetDraft = {
    name: "n",
    mask: null,
    template: "{date:YYYYMMDD}.{ext}",
    date: "capture",
    skip_if: null,
    normalize_ext: true,
  };
  it("needs a template and a known date source", () => {
    expect(validateName(n)).toEqual({});
    expect(validateName({ ...n, template: "  " }).template).toBeTruthy();
    expect(validateName({ ...n, date: "mtime" }).date).toBeTruthy();
    expect(validateName({ ...n, template: "a\\b.{ext}" }).template).toMatch(/\//);
  });
  it("needs a date only when the template has one", () => {
    expect(validateName({ ...n, date: "none" }).template).toMatch(/date/);
    expect(validateName({ ...n, date: "none", template: "{stem}.{ext}" })).toEqual({});
  });
  it("an empty mask list is an error", () => {
    expect(validateName({ ...n, skip_if: [] }).skip_if).toBeTruthy();
    expect(validateMask([])).toBeTruthy();
    expect(validateMask(["@photo"])).toBeNull();
  });
});

describe("new names", () => {
  it("refuses empty, taken and built-in names", () => {
    expect(newNameError("content", "", [], [])).toBeTruthy();
    expect(newNameError("content", "tiny", ["tiny"], [])).toMatch(/already/);
    expect(newNameError("content", "avif-high", [], ["avif-high"])).toMatch(/built-in/);
    expect(newNameError("mask", "my mask", [], [])).toBeTruthy();
    expect(newNameError("mask", "@x", [], [])).toBeTruthy();
    expect(newNameError("name", "my-date", ["short"], ["capture-date"])).toBeNull();
  });
});

describe("samples and ops", () => {
  it("parses sample names one per line", () => {
    expect(parseSamples(" a.jpg\n\n b.png \n")).toEqual(["a.jpg", "b.png"]);
  });
  it("coalesces updates of the same preset", () => {
    const a = { op: "update_mask" as const, name: "m", mask: { name: "m", items: ["*.a"] } };
    const b = { op: "update_mask" as const, name: "m", mask: { name: "m", items: ["*.b"] } };
    const c = { op: "update_mask" as const, name: "k", mask: { name: "k", items: ["*.b"] } };
    expect(pushOp([a], b)).toEqual([b]);
    expect(pushOp([a], c)).toEqual([a, c]);
  });
});

describe("bundles", () => {
  it("starts with the given steps on their own masks", () => {
    expect(defaultBundle("b", ["avif-high", "png-avif"])).toEqual({
      name: "b",
      kind: "bundle",
      mask: null,
      steps: [
        { preset: "avif-high", mask: null },
        { preset: "png-avif", mask: null },
      ],
    });
  });

  it("needs steps and refuses a mask of its own or itself as a step", () => {
    const known = ["avif-high", "png-avif"];
    expect(validateBundle(defaultBundle("b", ["avif-high"]), known)).toEqual({});
    expect(validateBundle({ ...defaultBundle("b", []) }, known).steps).toMatch(/no steps/);
    expect(validateBundle({ ...defaultBundle("b", ["b"]) }, [...known, "b"]).steps).toMatch(/include itself/);
    expect(validateBundle({ ...defaultBundle("b", ["nope"]) }, known).steps).toMatch(/no preset “nope”/);
    expect(validateBundle({ ...defaultBundle("b", ["avif-high"]), mask: ["*.png"] }, known).mask).toMatch(/no mask of its own/);
  });
});
