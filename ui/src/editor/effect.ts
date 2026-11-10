// "Effect": what the unsaved edits do to the files (crates/gallery-gui/src/effect.rs).
import { invoke } from "@tauri-apps/api/core";
import { t, type MessageKey } from "../i18n";
import { num } from "../i18n/format";
import type { EditOp } from "./model";

export type Outcome =
  | "compressed"
  | "renamed"
  | "unchanged"
  | "trash"
  | "kept"
  | "excluded"
  | "review"
  | "unmatched"
  | "waiting_ai";

export interface Counts {
  compressed: number;
  renamed: number;
  unchanged: number;
  trash: number;
  kept: number;
  excluded: number;
  review: number;
  unmatched: number;
  waiting_ai: number;
}

export interface Fate {
  outcome: Outcome;
  target: string | null;
  label: string;
}

export interface FileChange {
  path: string;
  before: Fate;
  after: Fate;
}

export interface ExampleGroup {
  outcome: Outcome;
  count: number;
  files: FileChange[];
}

export interface TargetDelta {
  name: string;
  before: Counts;
  after: Counts;
  changed: number;
}

export interface EffectDiff {
  files: number;
  before: Counts;
  after: Counts;
  changed: number;
  targets: TargetDelta[];
  examples: ExampleGroup[];
  rescanned: boolean;
  scan_ms: number;
  diff_ms: number;
}

export const effectApi = {
  /// `null` when a newer request cancelled this one.
  preview: (configPath: string | null, ops: EditOp[], refresh = false) =>
    invoke<EffectDiff | null>("effect_preview", { configPath, ops, refresh }),
  /// Stops the request in flight (it answers `null`).
  cancel: () => invoke<void>("effect_cancel"),
};

export const OUTCOME_TITLE: Record<Outcome, MessageKey> = {
  compressed: "outcome_compressed",
  renamed: "outcome_renamed",
  unchanged: "outcome_unchanged",
  trash: "outcome_trash",
  kept: "outcome_kept",
  excluded: "outcome_excluded",
  review: "outcome_review",
  unmatched: "outcome_unmatched",
  waiting_ai: "outcome_waiting_ai",
};

/** The outcome's name in the language in force. */
export const outcomeTitle = (o: Outcome): string => t(OUTCOME_TITLE[o]);

export const total = (c: Counts): number =>
  c.compressed + c.renamed + c.unchanged + c.trash + c.kept + c.excluded + c.review + c.unmatched + c.waiting_ai;

/// Files a target takes, by outcome: "2 compressed, 1 unchanged".
export function breakdown(c: Counts): string {
  const parts = (["compressed", "renamed", "unchanged", "trash", "kept", "review", "excluded", "unmatched", "waiting_ai"] as Outcome[])
    .filter((o) => c[o] > 0)
    .map((o) => `${num(c[o])} ${outcomeTitle(o)}`);
  return parts.join(", ");
}

/// A target's line: "takes 4 (was 3): 2 compressed".
export function targetLine(d: TargetDelta): string {
  const n = total(d.after);
  const was = total(d.before);
  const touched: string[] = [];
  if (d.after.compressed) touched.push(`${num(d.after.compressed)} ${outcomeTitle("compressed")}`);
  if (d.after.renamed) touched.push(`${num(d.after.renamed)} ${outcomeTitle("renamed")}`);
  if (d.after.unchanged) touched.push(`${num(d.after.unchanged)} ${outcomeTitle("unchanged")}`);
  if (d.after.trash) touched.push(`${num(d.after.trash)} ${outcomeTitle("trash")}`);
  if (d.after.kept) touched.push(`${num(d.after.kept)} ${outcomeTitle("kept")}`);
  const rest = touched.length ? touched.join(", ") : "";
  return t(rest ? "effect_takes_rest" : "effect_takes", { n, was, rest });
}
