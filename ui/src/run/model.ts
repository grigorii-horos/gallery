// Run screen logic: the confirmation text, when a run is allowed, and the
// result summary. Counts come from the core; nothing here touches files.
import type { ApplyDone, ApplyPreview, ApplyProgress, ApplySummary, PauseState, PilotFacts, PilotResult, SyncCheck } from "../api";

/** Files per target in the pilot. */
export const PILOT_PER_TARGET = 5;

/** English plural: 1 file, 2 files. */
export function plural(n: number, one: string, other: string): string {
  return n === 1 ? one : other;
}

export const num = (n: number) => n.toLocaleString("en-US");

export function filesWord(n: number): string {
  return `${num(n)} ${plural(n, "file", "files")}`;
}

export type FateId = "trash" | "renamed" | "kept";

export interface Fate {
  id: FateId;
  label: string;
  count: number;
}

/** What happens to the sources: each processed file has exactly one fate. */
export function fates(p: ApplyPreview): Fate[] {
  return [
    { id: "trash", label: "to the trash", count: p.trash },
    { id: "renamed", label: "renamed in place", count: p.renamed },
    { id: "kept", label: "left where they are", count: p.kept },
  ];
}

export type ConfirmKind = "all" | "pilot" | "rest";

export function confirmLines(p: ApplyPreview, kind: ConfirmKind = "all"): { title: string; lines: string[] } {
  const lines: string[] = [];
  if (p.writes > 0) {
    lines.push(`${num(p.writes)} new ${plural(p.writes, "file", "files")} will be written to the target folders.`);
  }
  if (p.trash > 0) {
    const verb = plural(p.trash, "source goes", "sources go");
    lines.push(`${num(p.trash)} ${verb} to the trash — each only after its copies are in place.`);
  }
  if (p.renamed > 0) {
    lines.push(`${num(p.renamed)} will be renamed in place (not copied and not sent to the trash).`);
  }
  if (p.kept > 0) lines.push(`${num(p.kept)} will stay where they are.`);
  const outside = p.review + p.unmatched + p.unchanged;
  if (outside > 0) lines.push(`${filesWord(outside)} outside the plan (to sort, unmatched, unchanged) will not be touched.`);
  lines.push("Files changed after the scan are skipped. The run can be undone from its report.");
  lines.push(
    "Each file's result is checked against its source (capture date, mtime, GPS, size, duration): on a mismatch the file is not placed and the source is left alone.",
  );
  if (kind === "pilot") {
    lines.push("The other files of the plan stay untouched until you continue yourself.");
    return { title: `Pilot: ${filesWord(p.files)}?`, lines };
  }
  if (kind === "rest") return { title: `Run the rest: ${filesWord(p.files)}?`, lines };
  return { title: `Run the plan: ${filesWord(p.files)}?`, lines };
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
  if (!p) return plain("No plan yet: it builds itself once the config can be read.");
  if (p.side === "server") {
    return plain("This is a server plan: it is not run here. Run it on the server with gallery apply.");
  }
  if (s.busy) return plain(s.busy);
  if (s.updating) return plain("The plan is refreshing for changed files, config or decisions — wait for it to finish.");
  if (p.files === 0) return plain("The plan has no files to process.");
  if (dryRun) return null;
  if (s.pilotOpen) {
    return plain("The pilot is done and waits for a decision: continue with the rest or undo the pilot.");
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
      title: `Stopped: results differ from the sources (${num(s.anomalies ?? 0)}), ${num(s.not_started)} not started`,
    };
  }
  if (s.cancelled) {
    return { tone: "warn", title: `Stopped: ${num(s.done)} of ${num(s.total)} processed, ${num(s.not_started)} not started` };
  }
  if (s.failed > 0) {
    return { tone: "bad", title: `${d.dry_run ? "Check found errors" : "Finished with errors"}: ${num(s.failed)} of ${num(s.total)}` };
  }
  const skipped = s.stale + s.missing;
  if (skipped > 0) {
    return { tone: "warn", title: `${d.dry_run ? "Checked" : "Done"}: ${num(s.done)} of ${num(s.total)}, skipped ${num(skipped)}` };
  }
  return d.dry_run
    ? { tone: "ok", title: `Check passed: ${filesWord(s.done)} can be processed` }
    : { tone: "ok", title: `Done: ${filesWord(s.done)} processed` };
}

export interface ResultRow {
  label: string;
  count: number;
  tone: Tone | undefined;
}

export function resultRows(s: ApplySummary, dryRun: boolean): ResultRow[] {
  const rows: ResultRow[] = [{ label: dryRun ? "can be processed" : "processed", count: s.done, tone: "ok" }];
  const add = (count: number, label: string, tone?: Tone) => {
    if (count > 0) rows.push({ label, count, tone });
  };
  add(s.renamed, "of them renamed in place");
  add(s.failed, "errors", "bad");
  add(s.stale, "changed after the plan — left untouched", "warn");
  add(s.missing, "file missing", "warn");
  add(s.not_started, "not started (cancelled) — left untouched");
  return rows;
}

export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export interface FactRow {
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
  const gps = (v: boolean | null) => (v === null ? dash : v ? "yes" : "no");
  return [
    {
      label: "Size",
      before: before.size === null ? dash : bytes(before.size),
      after: after.size === null ? dash : bytes(after.size),
      changed: before.size !== after.size,
    },
    { label: "Capture date", before: text(before.capture), after: text(after.capture), changed: before.capture !== after.capture },
    { label: "mtime", before: text(before.mtime), after: text(after.mtime), changed: before.mtime !== after.mtime },
    { label: "GPS", before: gps(before.gps), after: gps(after.gps), changed: before.gps !== after.gps },
  ];
}

export function pilotHeadline(r: PilotResult): { tone: Tone; title: string } {
  const n = r.files.length;
  if (r.stopped_by_anomalies) {
    return { tone: "bad", title: `Pilot stopped: results differ from the sources (${num(r.anomalies)})` };
  }
  if (r.anomalies > 0) {
    return { tone: "bad", title: `Pilot: ${filesWord(n)}, mismatches with the sources: ${num(r.anomalies)}` };
  }
  const failed = r.files.filter((f) => f.status !== "done").length;
  if (failed > 0) return { tone: "warn", title: `Pilot: ${filesWord(n)}, with errors: ${num(failed)}` };
  return { tone: "ok", title: `Pilot: ${filesWord(n)}, no mismatches with the sources` };
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
  if (!check) return { disabled: true, checked: false, note: "Checking Syncthing…" };
  if (!check.reachable) {
    return { disabled: true, checked: false, note: `Syncthing unavailable: ${check.reason ?? "no answer"}. The run is not blocked.` };
  }
  if (check.folders.length === 0) {
    return { disabled: true, checked: false, note: "The plan does not touch synced folders (or they are already paused)." };
  }
  return { disabled: false, checked: true, note: `Folders: ${check.folders.join(", ")}` };
}

/** The banner text while Gallery holds folders paused. */
export function pausedText(state: PauseState): string {
  return `Syncthing paused: ${state.folders.map((f) => f.label).join(", ")}`;
}
