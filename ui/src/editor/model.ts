// Config editor: the draft types of crates/gallery-gui/src/config_edit and
// the little UI logic around them (which operations to send, dirty state,
// drag-and-drop moves, field paths). What the edits do to the file is
// computed in Rust.
import type { PresetSet, TargetKind } from "../api";

export interface SourceDraft {
  id: string;
  path: string;
  server_path: string | null;
  recursive: boolean;
  /** null: the key is absent, the core's default list applies. */
  skip: string[] | null;
  ai_prefix: string | null;
  ai_prefix_file: string | null;
  ai_suffix: string | null;
  ai_suffix_file: string | null;
}

export interface FilterDraft {
  sources: string[];
  /** null: any file. */
  match: string[] | null;
  exclude: string[];
  /** AI category description; null: the filter does not ask the AI. */
  ai?: string | null;
  /** `"event"`: grow the match to whole events. */
  expand?: string | null;
  /** The other conditions as written in the file (TOML values as JSON), by key. */
  conditions?: Record<string, unknown>;
}

/** The keys of a filter that hold a condition. */
export type ConditionKey = "date" | "geo" | "device" | "props" | "path" | "event" | "near" | "duplicate" | "series";

/** One condition as the core reads it; `value` has the shape of the TOML value. */
export interface ConditionValue {
  key: ConditionKey;
  value: unknown;
}

/** A named point of `[places]`. */
export interface PlaceDraft {
  name: string;
  lat: number;
  lon: number;
  /** Radius in kilometres. */
  km: number;
}

export interface StepDraft {
  preset: string;
  /** null: the preset's own mask. */
  mask: string[] | null;
}

export interface OutputDraft {
  dir: string;
  server_dir: string | null;
  content: StepDraft[];
  rename: StepDraft[];
}

export interface TargetDraft {
  name: string;
  kind: TargetKind;
  filters: FilterDraft[];
  outputs: OutputDraft[];
}

export interface ConfigDraft {
  sources: SourceDraft[];
  places?: PlaceDraft[];
  /** In order: the first target with a matching filter takes a file. */
  targets: TargetDraft[];
  /** Names of the user's own masks. */
  masks: string[];
  name_presets?: NamePresetDraft[];
  content_presets?: ContentPresetDraft[];
  mask_defs?: MaskDraft[];
}

export type PresetKind = "name" | "content" | "mask";

/** A user name preset as written; null: the key is absent (core default). */
export interface NamePresetDraft {
  name: string;
  mask: string[] | null;
  template: string;
  /** capture | file_created | none */
  date: string | null;
  skip_if: string[] | null;
  normalize_ext: boolean | null;
}

export type ContentKind = "metadata" | "photo" | "video" | "audio" | "png";

/** The `kind` of a bundle: one name for an ordered list of presets. */
export const BUNDLE_KIND = "bundle";

/** A user content preset: `kind` and the keys of that kind (null: absent). */
export interface ContentPresetDraft {
  name: string;
  kind: string;
  mask: string[] | null;
  /** Bundles (`kind: "bundle"`) only: the ordered steps, same forms as a content pipe. */
  steps?: StepDraft[] | null;
  write_capture_date?: boolean | null;
  fix_gcam_make_model?: boolean | null;
  strip_gcamera_xmp?: boolean | null;
  strip_gain_map_xmp_for_avif?: boolean | null;
  fill_offset_time?: boolean | null;
  video_make_model?: boolean | null;
  video_restore_gps?: boolean | null;
  format?: string | null;
  quality?: number | null;
  speed?: number | null;
  min_saving_pct?: number | null;
  max_short_side?: number | null;
  ladder?: boolean | null;
  crf?: number | null;
  preset?: number | null;
  gop?: number | null;
  audio_kbps?: number | null;
  denoise?: string | null;
  stabilize?: boolean | null;
  codec?: string | null;
  kbps?: number | null;
  level?: number | null;
}

export interface MaskDraft {
  name: string;
  items: string[];
}

export type EditOp =
  | { op: "add_source"; source: SourceDraft }
  | { op: "update_source"; id: string; source: SourceDraft }
  | { op: "remove_source"; id: string }
  | { op: "add_target"; kind: TargetKind; target?: TargetDraft }
  | { op: "update_target"; index: number; target: TargetDraft }
  | { op: "remove_target"; index: number }
  | { op: "move_target"; from: number; to: number }
  | { op: "set_target_kind"; name: string; kind: TargetKind }
  | { op: "set_condition"; target: string; filter: number; condition: ConditionValue }
  | { op: "remove_condition"; target: string; filter: number; key: ConditionKey | "ai" | "expand" }
  | { op: "set_place"; name: string; place: { lat: number; lon: number; km: number } }
  | { op: "remove_place"; name: string }
  | { op: "add_name_preset"; preset: NamePresetDraft }
  | { op: "update_name_preset"; name: string; preset: NamePresetDraft }
  | { op: "remove_name_preset"; name: string }
  | { op: "add_content_preset"; preset: ContentPresetDraft }
  | { op: "update_content_preset"; name: string; preset: ContentPresetDraft }
  | { op: "remove_content_preset"; name: string }
  | { op: "add_mask"; mask: MaskDraft }
  | { op: "update_mask"; name: string; mask: MaskDraft }
  | { op: "remove_mask"; name: string }
  | { op: "rename_preset"; kind: PresetKind; from: string; to: string; update_refs: boolean }
  | { op: "copy_builtin"; kind: PresetKind; from: string; to: string };

export interface FieldIssue {
  /** Field path: `sources[1].id`, `targets[0].outputs[1].dir`; "" for the whole config. */
  path: string;
  message: string;
}

export interface FieldError {
  message: string;
  path: string | null;
}

/// A target renamed by the edit; overrides name targets, so some may still
/// point at `from`.
export interface TargetRename {
  index: number;
  from: string;
  to: string;
  overrides: number;
}

export interface EditOutcome {
  version: string;
  draft: ConfigDraft;
  changed: boolean;
  saved: boolean;
  /** The file is in the old three-stage shape: saving writes the new one after a backup. */
  old_shape?: boolean;
  /** File name of the backup this save wrote. */
  backup?: string | null;
  error: FieldError | null;
  warnings: FieldIssue[];
  renames: TargetRename[];
  /** Where each user preset / mask is used (only used ones). */
  usage?: Record<"name" | "content" | "masks", Record<string, string[]>>;
  /** The user's presets with descriptions and expanded masks (when the draft parses). */
  own?: PresetSet | null;
}

export type FailureKind = "changed" | "read" | "op" | "io";

export interface EditFailure {
  kind: FailureKind;
  message: string;
  op: number | null;
}

export interface PromptPreview {
  prompt: string;
  categories: string[];
}

export interface MaskTestResult {
  name: string;
  matched: boolean;
  excluded: boolean;
  taken: boolean;
}

/** The core's skip list for a source without `skip`. */
export const DEFAULT_SKIP = [".stfolder", ".stversions", ".gallery-trash", ".gallery-tmp-*"];

/// Add `op` to the pending operations. An update right after an update of
/// the same source or target replaces it, so typing in a field keeps one
/// operation per thing edited.
/** How a mask item reads: `!…` excludes, `@…` refers to a named mask. */
export function maskItemKind(item: string): "not" | "ref" | "pattern" {
  if (item.startsWith("!")) return "not";
  return item.startsWith("@") ? "ref" : "pattern";
}

export function pushOp(ops: EditOp[], op: EditOp): EditOp[] {
  const last = ops[ops.length - 1];
  if (last && op.op === "update_target" && last.op === "update_target") {
    if (last.index === op.index) return [...ops.slice(0, -1), op];
  }
  if (
    last &&
    (op.op === "update_name_preset" || op.op === "update_content_preset" || op.op === "update_mask") &&
    last.op === op.op &&
    last.name === op.name
  ) {
    return [...ops.slice(0, -1), op];
  }
  if (last && op.op === "update_source" && last.op === "update_source") {
    // The last update produced the id this one starts from.
    if (last.source.id === op.id) return [...ops.slice(0, -1), { ...op, id: last.id }];
  }
  return [...ops, op];
}

/// Whether there is something to save: operations that change the text
/// (or not checked yet).
export function isDirty(ops: EditOp[], checked: { changed: boolean } | null): boolean {
  if (ops.length === 0) return false;
  return checked ? checked.changed : true;
}

/// The move for target `from` dragged and dropped before item `before`
/// (`before` = length: at the end); null when it would not move.
export function dropMove(from: number, before: number): Extract<EditOp, { op: "move_target" }> | null {
  if (before === from || before === from + 1) return null;
  return { op: "move_target", from, to: before > from ? before - 1 : before };
}

export function pathOf(index: number): string {
  return `targets[${index}]`;
}

/// Issues about `path` or a field below it.
export function issuesUnder<T extends { path: string | null }>(issues: T[], path: string): T[] {
  return issues.filter((w) => {
    const p = w.path ?? "";
    return p === path || p.startsWith(`${path}.`) || p.startsWith(`${path}[`);
  });
}

/// A pipe step as shown on its chip: the preset and the mask it runs on.
export function stepLabel(step: StepDraft, presetMask: string | null): { preset: string; mask: string | null; own: boolean } {
  return step.mask
    ? { preset: step.preset, mask: step.mask.join(" | "), own: true }
    : { preset: step.preset, mask: presetMask, own: false };
}

/// `file` relative to the folder of `config` when it lies inside it (the
/// core reads `ai_*_file` relative to the config).
export function relativeTo(file: string, config: string): string {
  const dir = config.slice(0, config.lastIndexOf("/") + 1);
  return dir && file.startsWith(dir) ? file.slice(dir.length) : file;
}

export function emptyFilter(): FilterDraft {
  return { sources: [], match: null, exclude: [], ai: null, expand: null, conditions: {} };
}

export function emptyTarget(kind: TargetKind = "folders"): TargetDraft {
  return { name: "", kind, filters: [emptyFilter()], outputs: [] };
}

export function emptySource(): SourceDraft {
  return {
    id: "",
    path: "",
    server_path: null,
    recursive: false,
    skip: null,
    ai_prefix: null,
    ai_prefix_file: null,
    ai_suffix: null,
    ai_suffix_file: null,
  };
}

/// A name not taken yet: `base`, `base 2`, `base 3`…
export function freeName(base: string, taken: string[]): string {
  if (!taken.includes(base)) return base;
  for (let n = 2; ; n++) {
    const name = `${base} ${n}`;
    if (!taken.includes(name)) return name;
  }
}
