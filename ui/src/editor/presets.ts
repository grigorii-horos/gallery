// Presets screen: the typed fields of each content kind, form checks with
// the encoders' bounds (served by Rust: config_edit/limits.rs) and the
// little helpers around them. What an edit does to the file is Rust's.
import { BUNDLE_KIND, type ContentKind, type ContentPresetDraft, type NamePresetDraft, type PresetKind, type StepDraft } from "./model";

export interface Limit {
  kind: string;
  variant: string | null;
  key: string;
  min: number;
  max: number;
}

export type NameStatus = "renamed" | "same" | "not_matched" | "skipped" | "no_date";

export interface NameResult {
  name: string;
  result: string;
  status: NameStatus;
}

type Key = Exclude<keyof ContentPresetDraft, "name" | "kind" | "mask" | "steps">;

export interface FieldSpec {
  key: Key;
  label: string;
  type: "bool" | "int" | "choice";
  choices?: { value: string; label: string }[];
  hint?: string;
}

export const KINDS: { id: ContentKind; title: string }[] = [
  { id: "photo", title: "Photo" },
  { id: "video", title: "Video" },
  { id: "audio", title: "Audio" },
  { id: "png", title: "PNG" },
  { id: "metadata", title: "Metadata" },
];

/** The mask a preset of the kind runs on when it has none. */
export const KIND_MASK: Record<ContentKind, string> = {
  metadata: "*",
  photo: "@photo-original",
  video: "@video-original",
  audio: "@audio",
  png: "@png",
};

const FIELDS: Record<ContentKind, FieldSpec[]> = {
  photo: [
    {
      key: "format",
      label: "Format",
      type: "choice",
      choices: [
        { value: "avif", label: "AVIF" },
        { value: "webp", label: "WebP" },
      ],
    },
    { key: "quality", label: "Quality", type: "int", hint: "higher — better and larger" },
    { key: "speed", label: "Speed", type: "int", hint: "AVIF: speed, higher — faster and worse; WebP: method, higher — slower and better" },
    { key: "max_short_side", label: "Short side", type: "int", hint: "downscale to, px; empty — no downscaling" },
    { key: "ladder", label: "Ladder", type: "bool", hint: "if the file did not get smaller, try a lower quality" },
    { key: "min_saving_pct", label: "Min. saving", type: "int", hint: "%: less — keep the source" },
  ],
  video: [
    { key: "crf", label: "CRF", type: "int", hint: "lower — better and larger" },
    { key: "preset", label: "SVT preset", type: "int", hint: "lower — slower and better" },
    { key: "gop", label: "GOP", type: "int", hint: "frames between keyframes" },
    { key: "max_short_side", label: "Short side", type: "int", hint: "downscale to, px; 0 — no downscaling" },
    { key: "audio_kbps", label: "Audio", type: "int", hint: "Opus, kbit/s" },
    {
      key: "denoise",
      label: "Denoise",
      type: "choice",
      choices: [
        { value: "", label: "none" },
        { value: "verylow", label: "very light" },
        { value: "low", label: "light" },
        { value: "medium", label: "medium" },
        { value: "high", label: "strong" },
      ],
    },
    { key: "stabilize", label: "Stabilization", type: "bool" },
    { key: "ladder", label: "Ladder", type: "bool", hint: "if the file did not get smaller, try lower levels" },
  ],
  audio: [
    {
      key: "codec",
      label: "Codec",
      type: "choice",
      choices: [
        { value: "opus", label: "Opus" },
        { value: "mp3", label: "MP3" },
        { value: "aac", label: "AAC" },
        { value: "flac", label: "FLAC (lossless)" },
      ],
    },
    { key: "kbps", label: "Bitrate", type: "int", hint: "kbit/s; not needed for FLAC" },
  ],
  png: [{ key: "level", label: "Level", type: "int", hint: "optipng -oN: higher — slower and smaller" }],
  metadata: [
    { key: "write_capture_date", label: "Capture date", type: "bool", hint: "write to the file" },
    { key: "fix_gcam_make_model", label: "Make/Model GCam", type: "bool" },
    { key: "strip_gcamera_xmp", label: "XMP-GCamera", type: "bool", hint: "remove" },
    { key: "strip_gain_map_xmp_for_avif", label: "Gain map XMP", type: "bool", hint: "remove for AVIF" },
    { key: "fill_offset_time", label: "OffsetTime", type: "bool", hint: "fill in the time zone" },
    { key: "video_make_model", label: "Video Make/Model", type: "bool" },
    { key: "video_restore_gps", label: "Video GPS", type: "bool", hint: "restore" },
  ],
};

export function fieldsOf(kind: string): FieldSpec[] {
  return FIELDS[kind as ContentKind] ?? [];
}

/** What the core uses for an absent key (null: nothing / no limit). */
const DEFAULTS: Record<ContentKind, Partial<Record<Key, number | string | boolean | null>>> = {
  photo: { format: "avif", quality: 72, speed: 6, max_short_side: null, ladder: true, min_saving_pct: 0 },
  video: {
    crf: 30,
    preset: 3,
    gop: 240,
    max_short_side: 1080,
    audio_kbps: 160,
    denoise: null,
    stabilize: false,
    ladder: true,
  },
  // `codec` has no default in the core: it is required.
  audio: { codec: "opus", kbps: 128 },
  png: { level: 3 },
  metadata: {
    write_capture_date: true,
    fix_gcam_make_model: true,
    strip_gcamera_xmp: true,
    strip_gain_map_xmp_for_avif: true,
    fill_offset_time: true,
    video_make_model: true,
    video_restore_gps: true,
  },
};

export function coreDefault(kind: string, key: Key): number | string | boolean | null {
  return DEFAULTS[kind as ContentKind]?.[key] ?? null;
}

/** A new preset of `kind` with every field written out. */
export function defaultContent(kind: ContentKind, name: string): ContentPresetDraft {
  return { name, kind, mask: null, ...(DEFAULTS[kind] as Partial<ContentPresetDraft>) };
}

/** A new bundle of the given preset names (each on its own mask). */
export function defaultBundle(name: string, steps: string[]): ContentPresetDraft {
  return { name, kind: BUNDLE_KIND, mask: null, steps: steps.map((preset) => ({ preset, mask: null })) };
}

export const isBundle = (p: { kind: string }): boolean => p.kind === BUNDLE_KIND;

/** Field errors of a bundle, by key: it needs steps, and has no mask of its own. */
export function validateBundle(p: ContentPresetDraft, known: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  const steps: StepDraft[] = p.steps ?? [];
  if (steps.length === 0) out.steps = "the set has no steps: add at least one";
  else if (steps.some((s) => s.preset === p.name)) out.steps = "a set cannot include itself";
  else {
    const unknown = steps.find((s) => !known.includes(s.preset));
    if (unknown) out.steps = `no preset “${unknown.preset}”`;
    const bad = steps.find((s) => s.mask && validateMask(s.mask));
    if (bad) out.steps = `mask of step “${bad.preset}”: ${validateMask(bad.mask!)}`;
  }
  if (p.mask) out.mask = "a set has no mask of its own: its steps define the masks";
  return out;
}

/** The preset as `kind`: name and mask kept, the kind's fields filled where unset. */
export function switchKind(p: ContentPresetDraft, kind: ContentKind): ContentPresetDraft {
  const next: ContentPresetDraft = { ...p, kind };
  for (const [k, v] of Object.entries(DEFAULTS[kind]) as [Key, never][]) {
    if (next[k] === null || next[k] === undefined) next[k] = v;
  }
  return next;
}

/** The bound of `key` for `p` (its kind and format / codec), as in Rust. */
export function limitFor(limits: Limit[], p: ContentPresetDraft, key: string): Limit | undefined {
  const variant = p.kind === "photo" ? (p.format ?? "avif") : p.kind === "audio" ? (p.codec ?? "opus") : "";
  return limits.find((l) => l.kind === p.kind && l.key === key && (l.variant === null || l.variant === variant));
}

/** Field errors of a content preset, by key. */
export function validateContent(p: ContentPresetDraft, limits: Limit[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const f of fieldsOf(p.kind)) {
    const v = p[f.key];
    if (v === null || v === undefined) continue;
    if (f.type === "int") {
      const n = v as number;
      if (!Number.isInteger(n)) {
        out[f.key] = "an integer is required";
        continue;
      }
      const l = limitFor(limits, p, f.key);
      if (l && (n < l.min || n > l.max)) out[f.key] = `from ${l.min} to ${l.max}`;
    } else if (f.type === "choice") {
      const ok = f.choices!.some((c) => c.value === v && c.value !== "");
      if (!ok) out[f.key] = `one of:  ${f.choices!.filter((c) => c.value).map((c) => c.value).join(", ")}`;
    }
  }
  if (p.mask) {
    const m = validateMask(p.mask);
    if (m) out.mask = m;
  }
  return out;
}

const DATES = ["capture", "file_created", "none"];

/** Field errors of a name preset, by key. */
export function validateName(p: NamePresetDraft): Record<string, string> {
  const out: Record<string, string> = {};
  if (!p.template.trim()) out.template = "a template is required, e.g. {date:YYYYMMDD_HHMMSS}.{ext}";
  else if (p.template.includes("\\")) out.template = "folders are separated by “/”, not “\\”";
  else if (p.date === "none" && p.template.includes("{date"))
    out.template = "the template has a date but the date source is “none”: the step will change nothing";
  if (p.date !== null && !DATES.includes(p.date)) out.date = "capture, file_created or none";
  for (const k of ["mask", "skip_if"] as const) {
    const m = p[k];
    if (m) {
      const e = validateMask(m);
      if (e) out[k] = e;
    }
  }
  return out;
}

export function validateMask(items: string[]): string | null {
  if (items.length === 0) return "empty mask: add at least one";
  if (items.some((i) => i === "@")) return "a mask name is required after “@”";
  return null;
}

/** Why `name` cannot be the name of a new (or renamed) item; null if it can. */
export function newNameError(kind: PresetKind, name: string, taken: string[], builtin: string[]): string | null {
  if (!name.trim()) return "a name is required";
  if (name.trim() !== name) return "no leading or trailing spaces";
  if (kind === "mask" && /[@\s]/.test(name)) return "a mask name has no “@” and no spaces";
  if (builtin.includes(name)) return kind === "mask" ? "this is a built-in mask name" : "this is a built-in preset name";
  if (taken.includes(name)) return "this name is already in the config";
  return null;
}

export function parseSamples(text: string): string[] {
  return text
    .split("\n")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** A free name like `base`, `base-2`… */
export function freeKey(base: string, taken: string[]): string {
  if (!taken.includes(base)) return base;
  for (let n = 2; ; n++) if (!taken.includes(`${base}-${n}`)) return `${base}-${n}`;
}
