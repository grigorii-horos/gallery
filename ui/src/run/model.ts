// Run screen logic: the confirmation text, when a run is allowed, and the
// result summary. Counts come from the core; nothing here touches files.
import { t } from "../i18n";
import { bytes, num } from "../i18n/format";
import type { ApplyDone, ApplyPreview, ApplyProgress, ApplySummary, PauseState, PilotFacts, PilotResult, SyncCheck } from "../api";

/** Files per target in the pilot. */
export const PILOT_PER_TARGET = 5;

export { bytes, num };

/** "1 file", "5 files": the plural form of the language. */
export function filesWord(n: number): string {
  return t("files", { count: n });
}

export type FateId = "trash" | "renamed" | "kept" | "to_trash";

export interface Fate {
  id: FateId;
  label: string;
  count: number;
}

/** What happens to the sources: each processed file has exactly one fate. Files of trash targets
 *  (no copies) are a row of their own, shown only when the plan has some. */
export function fates(p: ApplyPreview): Fate[] {
  const rows: Fate[] = [
    { id: "trash", label: t("fate_trash"), count: p.trash },
    { id: "renamed", label: t("fate_renamed"), count: p.renamed },
    { id: "kept", label: t("fate_kept"), count: p.kept },
  ];
  if (p.to_trash > 0) rows.push({ id: "to_trash", label: t("fate_to_trash"), count: p.to_trash });
  return rows;
}

export type ConfirmKind = "all" | "pilot" | "rest";

export function confirmLines(p: ApplyPreview, kind: ConfirmKind = "all"): { title: string; lines: string[] } {
  const lines: string[] = [];
  if (p.writes > 0) {
    lines.push(t("confirm_writes", { count: p.writes }));
  }
  if (p.trash > 0) {
    lines.push(t("confirm_trash", { count: p.trash }));
  }
  if (p.renamed > 0) {
    lines.push(t("confirm_renamed", { count: p.renamed }));
  }
  if (p.kept > 0) lines.push(t("confirm_kept", { count: p.kept }));
  if (p.to_trash > 0) lines.push(t("confirm_to_trash", { n: p.to_trash }));
  if (p.kept_targets > 0) lines.push(t("confirm_kept_targets", { n: p.kept_targets }));
  const outside = p.review + p.unmatched + p.unchanged;
  if (outside > 0) lines.push(t("confirm_outside", { count: outside }));
  lines.push(t("confirm_skipped_undo"));
  lines.push(t("confirm_verified"));
  if (kind === "pilot") {
    lines.push(t("confirm_pilot_rest"));
    return { title: t("confirm_title_pilot", { count: p.files }), lines };
  }
  if (kind === "rest") return { title: t("confirm_title_rest", { count: p.files }), lines };
  return { title: t("confirm_title_all", { count: p.files }), lines };
}

export interface BlockInput {
  preview: ApplyPreview | null;
  /** Another operation runs (its text), or null. */
  busy: string | null;
  /** The plan is being brought up to date: it is not what a run would use. */
  updating?: boolean;
  /** A pilot of this plan ran and waits for "Continue" or "Undo". */
  pilotOpen?: boolean;
}

export interface Block {
  text: string;
  /** The message points to the History (failed files are retried there). */
  history: boolean;
}

/** Why the run (or the dry run) cannot start now; null when it can. */
export function applyBlock(s: BlockInput, dryRun = false): Block | null {
  const p = s.preview;
  const plain = (text: string): Block => ({ text, history: false });
  if (!p) return plain(t("block_no_plan"));
  if (p.side === "server") {
    return plain(t("block_server_plan"));
  }
  if (s.busy) return plain(s.busy);
  if (s.updating) return plain(t("block_updating"));
  if (p.files === 0) return plain(t("block_no_files"));
  if (dryRun) return null;
  if (s.pilotOpen) {
    return plain(t("block_pilot_open"));
  }
  return null;
}

export function progressFraction(p: ApplyProgress): number {
  return p.total > 0 ? Math.min(1, p.done / p.total) : 0;
}

export type Tone = "ok" | "warn" | "bad";

export function resultHeadline(d: ApplyDone): { tone: Tone; title: string } {
  const s = d.summary;
  if (s.stopped_by_anomalies) {
    return {
      tone: "bad",
      title: t("result_stopped_anomalies", { anomalies: s.anomalies ?? 0, notStarted: s.not_started }),
    };
  }
  if (s.cancelled) {
    return { tone: "warn", title: t("result_stopped", { done: s.done, total: s.total, notStarted: s.not_started }) };
  }
  if (s.failed > 0) {
    return { tone: "bad", title: t(d.dry_run ? "result_check_errors" : "result_errors", { failed: s.failed, total: s.total }) };
  }
  const skipped = s.stale + s.missing;
  if (skipped > 0) {
    return { tone: "warn", title: t(d.dry_run ? "result_checked_skipped" : "result_done_skipped", { done: s.done, total: s.total, skipped }) };
  }
  return d.dry_run
    ? { tone: "ok", title: t("result_check_passed", { count: s.done }) }
    : { tone: "ok", title: t("result_ok", { count: s.done }) };
}

export interface ResultRow {
  label: string;
  count: number;
  tone: Tone | undefined;
}

export function resultRows(s: ApplySummary, dryRun: boolean): ResultRow[] {
  const rows: ResultRow[] = [{ label: dryRun ? t("row_can_process") : t("row_processed"), count: s.done, tone: "ok" }];
  const add = (count: number, label: string, tone?: Tone) => {
    if (count > 0) rows.push({ label, count, tone });
  };
  add(s.renamed, t("row_renamed"));
  add(s.failed, t("row_errors"), "bad");
  add(s.stale, t("row_stale"), "warn");
  add(s.missing, t("row_missing"), "warn");
  add(s.not_started, t("row_not_started"));
  return rows;
}

export interface FactRow {
  id: "size" | "capture" | "mtime" | "gps";
  label: string;
  before: string;
  after: string;
  /** The value changed (size always may; date, mtime and GPS should not). */
  changed: boolean;
}

const dash = "—";

/** Before→after lines of one pilot output: size, capture date, mtime, GPS. */
export function factRows(before: PilotFacts, after: PilotFacts): FactRow[] {
  const text = (v: string | null) => v ?? dash;
  const gps = (v: boolean | null) => (v === null ? dash : v ? t("yes") : t("no"));
  return [
    {
      id: "size",
      label: t("fact_size"),
      before: before.size === null ? dash : bytes(before.size),
      after: after.size === null ? dash : bytes(after.size),
      changed: before.size !== after.size,
    },
    { id: "capture", label: t("fact_capture"), before: text(before.capture), after: text(after.capture), changed: before.capture !== after.capture },
    { id: "mtime", label: t("fact_mtime"), before: text(before.mtime), after: text(after.mtime), changed: before.mtime !== after.mtime },
    { id: "gps", label: t("fact_gps"), before: gps(before.gps), after: gps(after.gps), changed: before.gps !== after.gps },
  ];
}

export function pilotHeadline(r: PilotResult): { tone: Tone; title: string } {
  const n = r.files.length;
  if (r.stopped_by_anomalies) {
    return { tone: "bad", title: t("pilot_stopped", { anomalies: r.anomalies }) };
  }
  if (r.anomalies > 0) {
    return { tone: "bad", title: t("pilot_mismatch", { count: n, anomalies: r.anomalies }) };
  }
  const failed = r.files.filter((f) => f.status !== "done").length;
  if (failed > 0) return { tone: "warn", title: t("pilot_errors", { count: n, failed }) };
  return { tone: "ok", title: t("pilot_clean", { count: n }) };
}

/** The "Pause Syncthing" option of the confirmation. */
export interface SyncOption {
  disabled: boolean;
  /** Initial state: on when Syncthing answers and the plan touches synced folders. */
  checked: boolean;
  /** What stands under the checkbox: the folders, or why it is off. */
  note: string;
}

export function syncOption(check: SyncCheck | null): SyncOption {
  if (!check) return { disabled: true, checked: false, note: t("sync_checking") };
  if (!check.reachable) {
    return { disabled: true, checked: false, note: t("sync_unavailable", { reason: check.reason ?? t("sync_no_answer") }) };
  }
  if (check.folders.length === 0) {
    return { disabled: true, checked: false, note: t("sync_no_folders") };
  }
  return { disabled: false, checked: true, note: t("sync_folders", { folders: check.folders.join(", ") }) };
}

/** The banner text while Gallery holds folders paused. */
export function pausedText(state: PauseState): string {
  return t("sync_paused", { folders: state.folders.map((f) => f.label).join(", ") });
}
