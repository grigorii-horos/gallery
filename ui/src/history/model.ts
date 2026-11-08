// History screen logic: chips, tallies, filters, when undo and retry are
// offered and the texts of their confirmations. Counts come from the core;
// nothing here touches files.
import type { ItemView, ReportEntry, UndoPreview } from "../api";
import { num, plural } from "../run/model";


const MONTHS = [
  "January", "February", "March", "April", "May", "June",
  "July", "August", "September", "October", "November", "December",
];

/** "November 5, 2026, 10:15" — the wall clock the report recorded. */
export function runTime(rfc3339: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(rfc3339);
  if (!m) return null;
  return `${MONTHS[Number(m[2]) - 1]} ${Number(m[3])}, ${m[1]}, ${m[4]}:${m[5]}`;
}

export type ChipTone = "ok" | "warn" | "bad" | "muted" | "accent";

export interface Chip {
  label: string;
  tone: ChipTone;
}

export function entryChips(e: ReportEntry): Chip[] {
  if (e.error) return [{ label: "unreadable", tone: "bad" }];
  const chips: Chip[] = [];
  const s = e.summary;
  if (e.side === "server") chips.push({ label: "server", tone: "accent" });
  if (e.undone) {
    chips.push({ label: "undone", tone: "muted" });
    return chips;
  }
  if (e.partially_undone) chips.push({ label: "partly undone", tone: "muted" });
  if (!e.finished) chips.push({ label: "unfinished", tone: "warn" });
  else if (s.cancelled) chips.push({ label: "stopped", tone: "warn" });
  if (s.failed > 0) chips.push({ label: `${num(s.failed)} ${plural(s.failed, "error", "errors")}`, tone: "bad" });
  const skipped = s.stale + s.missing;
  if (skipped > 0) {
    chips.push({ label: `${num(skipped)} ${plural(skipped, "skipped", "skipped")}`, tone: "warn" });
  }
  if (e.finished && !s.cancelled && s.failed === 0 && skipped === 0) chips.push({ label: "no errors", tone: "ok" });
  if (e.archived) chips.push({ label: "earlier run", tone: "muted" });
  return chips;
}

export type TallyId = "done" | "failed" | "skipped" | "not_started";

export interface TallySegment {
  id: TallyId;
  label: string;
  count: number;
}

/** The plan's files by outcome: one strip per run in the list. */
export function entryTally(e: ReportEntry): TallySegment[] {
  const s = e.summary;
  return [
    { id: "done", label: "processed", count: s.done },
    { id: "failed", label: "errors", count: s.failed },
    { id: "skipped", label: "skipped", count: s.stale + s.missing },
    { id: "not_started", label: "not started", count: s.not_started },
  ];
}

export type ItemFilter = "all" | "failed" | "stale" | "missing";

export const FILTERS: { id: ItemFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "failed", label: "Errors" },
  { id: "stale", label: "Changed" },
  { id: "missing", label: "Missing" },
];

export function filterItems(items: ItemView[], f: ItemFilter): ItemView[] {
  return f === "all" ? items : items.filter((i) => i.status === f);
}

export function filterCounts(items: ItemView[]): Record<ItemFilter, number> {
  const c: Record<ItemFilter, number> = { all: items.length, failed: 0, stale: 0, missing: 0 };
  for (const i of items) {
    if (i.status === "failed" || i.status === "stale" || i.status === "missing") c[i.status] += 1;
  }
  return c;
}

export const STATUS_LABEL: Record<ItemView["status"], string> = {
  done: "done",
  failed: "error",
  stale: "changed after the plan — left untouched",
  missing: "file is missing",
  dry_run: "check",
};

export interface Block {
  text: string;
}

const SERVER = "This is a run on a server: undo it there, with the command below.";

/** Why "Undo…" is not offered; null when it is. `busy`: another operation's text. */
export function undoBlock(e: ReportEntry, busy: string | null): Block | null {
  if (e.error) return { text: "The report could not be read." };
  if (e.side === "server") return { text: SERVER };
  if (e.undone) return { text: "This run was already undone." };
  if (!e.plan_exists) return { text: "The report's plan is not next to it: you can undo with the gallery undo command." };
  if (busy) return { text: busy };
  return null;
}

/** Why "Retry failed…" is not offered; null when it is. */
export function retryBlock(e: ReportEntry, busy: string | null): Block | null {
  if (e.error) return { text: "The report could not be read." };
  if (e.side === "server") return { text: SERVER };
  if (e.undone) return { text: "The run was undone: nothing to retry; the files will go back into the current plan." };
  if (!e.finished) return { text: "The run is not finished: undo it first." };
  if (e.summary.failed === 0) return { text: "This run has no failed files." };
  if (!e.plan_exists) return { text: "The report's plan is not next to it." };
  if (busy) return { text: busy };
  return null;
}

export function undoConfirmLines(e: ReportEntry, u: UndoPreview): { title: string; lines: string[] } {
  const lines: string[] = [];
  if (u.restore > 0) {
    lines.push(
      `${num(u.restore)} ${plural(u.restore, "source is", "sources are")} restored from the trash.`,
    );
  }
  if (u.rename_back > 0) {
    lines.push(
      `${num(u.rename_back)} renamed ${plural(u.rename_back, "file", "files")} will get the old ${plural(u.rename_back, "name", "names")} back.`,
    );
  }
  if (u.remove_outputs > 0) {
    lines.push(
      `${num(u.remove_outputs)} ${plural(u.remove_outputs, "copy will be deleted", "copies will be deleted")} from the target folders.`,
    );
  }
  if (lines.length === 0) lines.push("Nothing to restore per the report: the undo will only mark the run as undone.");
  lines.push(
    "Nothing is overwritten: an occupied spot, a modified copy or a file missing from the trash stays as it is and goes to the problem list.",
  );
  const when = runTime(e.started);
  return { title: when ? `Undo the run from ${when}?` : "Undo the run?", lines };
}

export function retryConfirmLines(e: ReportEntry): string[] {
  const n = e.summary.failed;
  return [
    `${num(n)} ${plural(n, "failed file", "failed files")}: the copies they managed to place are deleted (their sources were not touched).`,
    "A new plan is built next to the old one from those that have not changed since.",
    "Nothing runs right away: the plan opens on the Sort screen, where you run it.",
  ];
}

export function undoResultText(restored: number, removed: number): string {
  return `Sources restored: ${num(restored)}, copies deleted: ${num(removed)}.`;
}
