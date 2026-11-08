// "Effect": what the unsaved edits do to the files (crates/gallery-gui/src/effect.rs).
import { invoke } from "@tauri-apps/api/core";
import type { EditOp } from "./model";

export type Outcome = "compressed" | "renamed" | "unchanged" | "excluded" | "review" | "unmatched" | "waiting_ai";

export interface Counts {
  compressed: number;
  renamed: number;
  unchanged: number;
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
};

export const OUTCOME_TITLE: Record<Outcome, string> = {
  compressed: "compressed",
  renamed: "renamed",
  unchanged: "unchanged",
  excluded: "excluded",
  review: "to decide",
  unmatched: "unmatched",
  waiting_ai: "waiting for AI",
};

export const total = (c: Counts): number =>
  c.compressed + c.renamed + c.unchanged + c.excluded + c.review + c.unmatched + c.waiting_ai;

/// Files a target takes, by outcome: "2 compressed, 1 unchanged".
export function breakdown(c: Counts): string {
  const parts = (["compressed", "renamed", "unchanged", "review", "excluded", "unmatched", "waiting_ai"] as Outcome[])
    .filter((o) => c[o] > 0)
    .map((o) => `${c[o]} ${OUTCOME_TITLE[o]}`);
  return parts.join(", ");
}

/// A target's line: "takes 4 (was 3): 2 compressed".
export function targetLine(t: TargetDelta): string {
  const n = total(t.after);
  const was = total(t.before);
  const touched: string[] = [];
  if (t.after.compressed) touched.push(`${t.after.compressed} ${OUTCOME_TITLE.compressed}`);
  if (t.after.renamed) touched.push(`${t.after.renamed} ${OUTCOME_TITLE.renamed}`);
  if (t.after.unchanged) touched.push(`${t.after.unchanged} ${OUTCOME_TITLE.unchanged}`);
  const rest = touched.length ? `: ${touched.join(", ")}` : "";
  return `takes ${n} (was ${was})${rest}`;
}
