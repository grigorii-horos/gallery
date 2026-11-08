// Presets screen: the typed fields of each content kind, form checks with
// the encoders' bounds (served by Rust: config_edit/limits.rs) and the
// little helpers around them. What an edit does to the file is Rust's.
import { t, type MessageKey } from "../i18n";
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
  label: MessageKey;
  type: "bool" | "int" | "choice";
  choices?: { value: string; label: MessageKey }[];
  hint?: MessageKey;
}

export const KINDS: { id: ContentKind; title: MessageKey }[] = [
  { id: "photo", title: "kind_photo" },
  { id: "video", title: "kind_video" },
  { id: "audio", title: "kind_audio" },
  { id: "png", title: "kind_png" },
  { id: "metadata", title: "kind_metadata" },
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
      label: "fld_format",
      type: "choice",
      choices: [
        { value: "avif", label: "fld_avif" },
        { value: "webp", label: "fld_webp" },
      ],
    },
    { key: "quality", label: "fld_quality", type: "int", hint: "hint_higher_better_and_larger" },
    { key: "speed", label: "fld_speed", type: "int", hint: "hint_avif_speed_higher_faster_and_worse_webp_" },
    { key: "max_short_side", label: "fld_short_side", type: "int", hint: "hint_downscale_to_px_empty_no_downscaling" },
    { key: "ladder", label: "fld_ladder", type: "bool", hint: "hint_if_the_file_did_not_get_smaller_try_a_lo" },
    { key: "min_saving_pct", label: "fld_min_saving", type: "int", hint: "hint_less_keep_the_source" },
  ],
  video: [
    { key: "crf", label: "fld_crf", type: "int", hint: "hint_lower_better_and_larger" },
    { key: "preset", label: "fld_svt_preset", type: "int", hint: "hint_lower_slower_and_better" },
    { key: "gop", label: "fld_gop", type: "int", hint: "hint_frames_between_keyframes" },
    { key: "max_short_side", label: "fld_short_side", type: "int", hint: "hint_downscale_to_px_0_no_downscaling" },
    { key: "audio_kbps", label: "fld_audio", type: "int", hint: "hint_opus_kbit_s" },
    {
      key: "denoise",
      label: "fld_denoise",
      type: "choice",
      choices: [
        { value: "", label: "fld_none" },
        { value: "verylow", label: "fld_very_light" },
        { value: "low", label: "fld_light" },
        { value: "medium", label: "fld_medium" },
        { value: "high", label: "fld_strong" },
      ],
    },
    { key: "stabilize", label: "fld_stabilization", type: "bool" },
    { key: "ladder", label: "fld_ladder", type: "bool", hint: "hint_if_the_file_did_not_get_smaller_try_lowe" },
  ],
  audio: [
    {
      key: "codec",
      label: "fld_codec",
      type: "choice",
      choices: [
        { value: "opus", label: "fld_opus" },
        { value: "mp3", label: "fld_mp3" },
        { value: "aac", label: "fld_aac" },
        { value: "flac", label: "fld_flac_lossless" },
      ],
    },
    { key: "kbps", label: "fld_bitrate", type: "int", hint: "hint_kbit_s_not_needed_for_flac" },
  ],
  png: [{ key: "level", label: "fld_level", type: "int", hint: "hint_optipng_on_higher_slower_and_smaller" }],
  metadata: [
    { key: "write_capture_date", label: "fld_capture_date", type: "bool", hint: "hint_write_to_the_file" },
    { key: "fix_gcam_make_model", label: "fld_make_model_gcam", type: "bool" },
    { key: "strip_gcamera_xmp", label: "fld_xmp_gcamera", type: "bool", hint: "hint_remove" },
    { key: "strip_gain_map_xmp_for_avif", label: "fld_gain_map_xmp", type: "bool", hint: "hint_remove_for_avif" },
    { key: "fill_offset_time", label: "fld_offsettime", type: "bool", hint: "hint_fill_in_the_time_zone" },
    { key: "video_make_model", label: "fld_video_make_model", type: "bool" },
    { key: "video_restore_gps", label: "fld_video_gps", type: "bool", hint: "hint_restore" },
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
  if (steps.length === 0) out.steps = t("val_set_no_steps");
  else if (steps.some((s) => s.preset === p.name)) out.steps = t("val_set_self");
  else {
    const unknown = steps.find((s) => !known.includes(s.preset));
    if (unknown) out.steps = t("val_no_preset", { name: unknown.preset });
    const bad = steps.find((s) => s.mask && validateMask(s.mask));
    if (bad) out.steps = t("val_step_mask", { preset: bad.preset, error: validateMask(bad.mask!) ?? "" });
  }
  if (p.mask) out.mask = t("val_set_mask");
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
        out[f.key] = t("val_integer");
        continue;
      }
      const l = limitFor(limits, p, f.key);
      if (l && (n < l.min || n > l.max)) out[f.key] = t("val_range", { min: l.min, max: l.max });
    } else if (f.type === "choice") {
      const ok = f.choices!.some((c) => c.value === v && c.value !== "");
      if (!ok) out[f.key] = t("val_one_of", { choices: f.choices!.filter((c) => c.value).map((c) => c.value).join(", ") });
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
  if (!p.template.trim()) out.template = t("val_template_required");
  else if (p.template.includes("\\")) out.template = t("val_template_backslash");
  else if (p.date === "none" && p.template.includes("{date"))
    out.template = t("val_template_no_date");
  if (p.date !== null && !DATES.includes(p.date)) out.date = t("val_date");
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
  if (items.length === 0) return t("val_mask_empty");
  if (items.some((i) => i === "@")) return t("val_mask_ref");
  return null;
}

/** Why `name` cannot be the name of a new (or renamed) item; null if it can. */
export function newNameError(kind: PresetKind, name: string, taken: string[], builtin: string[]): string | null {
  if (!name.trim()) return t("val_name_required");
  if (name.trim() !== name) return t("val_name_spaces");
  if (kind === "mask" && /[@\s]/.test(name)) return t("val_mask_name_chars");
  if (builtin.includes(name)) return kind === "mask" ? t("val_builtin_mask") : t("val_builtin_preset");
  if (taken.includes(name)) return t("val_name_taken");
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
