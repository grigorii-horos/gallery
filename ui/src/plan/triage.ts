// "Unmatched triage": types and helpers of the unmatched triage. The
// groups, reasons and the config edits come from Rust (crates/gallery-gui/src/triage.rs).
import { invoke } from "@tauri-apps/api/core";
import type { EditOp, FieldError, FieldIssue } from "../editor/model";

export type ActionKind = "add_to_target" | "skip_folder" | "skip_source" | "create_target";

export interface SuggestedAction {
  kind: ActionKind;
  label: string;
  /** add_to_target: the target; create_target: the proposed name. */
  target: string | null;
  /** create_target: the proposed output folder. */
  dir: string | null;
  ops: EditOp[];
}

export interface TriageGroup {
  source: string;
  /** Relative to the source root; "" is the root. */
  dir: string;
  /** Lower-case, no dot; "" for files without an extension. */
  ext: string;
  count: number;
  size: number;
  samples: string[];
  reason: string;
  actions: SuggestedAction[];
}

export interface TriageView {
  /** Config version the actions were made for: save with it. */
  version: string;
  total: number;
  groups: TriageGroup[];
}

export interface DiffLine {
  kind: "ctx" | "add" | "del";
  text: string;
}

export interface ChangePreview {
  version: string;
  changed: boolean;
  diff: DiffLine[];
  error: FieldError | null;
  warnings: FieldIssue[];
}

export const triageApi = {
  /** The unmatched files of the current plan. */
  view: (configPath: string | null) => invoke<TriageView | null>("triage_view", { configPath }),
  createOps: (configPath: string | null, version: string, g: TriageGroup, name: string, dir: string) =>
    invoke<SuggestedAction>("triage_create_ops", {
      configPath,
      version,
      source: g.source,
      relDir: g.dir,
      ext: g.ext,
      name,
      dir,
    }),
  preview: (path: string | null, version: string, ops: EditOp[]) =>
    invoke<ChangePreview>("config_preview_change", { path, version, ops }),
};

export function groupId(g: TriageGroup): string {
  return `${g.source}\u0000${g.dir}\u0000${g.ext}`;
}

/** `source/folder`, or just the source for its root. */
export function placeLabel(g: Pick<TriageGroup, "source" | "dir">): string {
  return g.dir ? `${g.source}/${g.dir}` : g.source;
}

export function extLabel(ext: string): string {
  return ext ? `.${ext}` : "no extension";
}

export function formatSize(bytes: number): string {
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${bytes} B` : `${v.toLocaleString("en-US", { maximumFractionDigits: 1, minimumFractionDigits: 1 })} ${units[i]}`;
}

/** Why a change cannot be saved, or null when it can. */
export function blocker(p: ChangePreview | null): string | null {
  if (!p) return "Checking…";
  if (p.error) return p.error.message;
  if (!p.changed) return "The config already contains this change";
  return null;
}
