// History screen logic: chips, tallies, filters, when undo and retry are
// offered and the texts of their confirmations. Counts come from the core;
// nothing here touches files.
import type { ItemView, ReportEntry, UndoPreview } from "../api";
import { t, type MessageKey } from "../i18n";
import { dateTimeLong } from "../i18n/format";

/** "November 5, 2026 at 10:15" — the wall clock the report recorded, in the language in force. */
export function runTime(rfc3339: string): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(rfc3339);
  if (!m) return null;
  const at = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), Number(m[4]), Number(m[5]));
  return dateTimeLong(at);
}

export type ChipTone = "ok" | "warn" | "bad" | "muted" | "accent";

export interface Chip {
  label: string;
  tone: ChipTone;
}

export function entryChips(e: ReportEntry): Chip[] {
  if (e.error) return [{ label: t("chip_unreadable"), tone: "bad" }];
  const chips: Chip[] = [];
  const s = e.summary;
  if (e.side === "server") chips.push({ label: t("chip_server"), tone: "accent" });
  if (e.undone) {
    chips.push({ label: t("chip_undone"), tone: "muted" });
    return chips;
  }
  if (e.partially_undone) chips.push({ label: t("chip_partly_undone"), tone: "muted" });
  if (!e.finished) chips.push({ label: t("chip_unfinished"), tone: "warn" });
  else if (s.cancelled) chips.push({ label: t("chip_stopped"), tone: "warn" });
  if (s.failed > 0) chips.push({ label: t("chip_errors", { count: s.failed }), tone: "bad" });
  const skipped = s.stale + s.missing;
  if (skipped > 0) {
    chips.push({ label: t("chip_skipped", { count: skipped }), tone: "warn" });
  }
  if (e.finished && !s.cancelled && s.failed === 0 && skipped === 0) chips.push({ label: t("chip_no_errors"), tone: "ok" });
  if (e.archived) chips.push({ label: t("chip_earlier"), tone: "muted" });
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
    { id: "done", label: t("tally_processed"), count: s.done },
    { id: "failed", label: t("tally_errors"), count: s.failed },
    { id: "skipped", label: t("tally_skipped"), count: s.stale + s.missing },
    { id: "not_started", label: t("tally_not_started"), count: s.not_started },
  ];
}

export type ItemFilter = "all" | "failed" | "stale" | "missing";

export const FILTERS: { id: ItemFilter; label: MessageKey }[] = [
  { id: "all", label: "filter_all" },
  { id: "failed", label: "filter_errors" },
  { id: "stale", label: "filter_changed" },
  { id: "missing", label: "filter_missing" },
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

export const STATUS_LABEL: Record<ItemView["status"], MessageKey> = {
  done: "status_done",
  failed: "status_error",
  stale: "row_stale",
  missing: "file_missing",
  dry_run: "status_check",
};

export interface Block {
  text: string;
}

/** Why "Undo…" is not offered; null when it is. `busy`: another operation's text. */
export function undoBlock(e: ReportEntry, busy: string | null): Block | null {
  if (e.error) return { text: t("block_report_unreadable") };
  if (e.side === "server") return { text: t("block_server_run") };
  if (e.undone) return { text: t("block_already_undone") };
  if (!e.plan_exists) return { text: t("block_plan_missing_undo") };
  if (busy) return { text: busy };
  return null;
}

/** Why "Retry failed…" is not offered; null when it is. */
export function retryBlock(e: ReportEntry, busy: string | null): Block | null {
  if (e.error) return { text: t("block_report_unreadable") };
  if (e.side === "server") return { text: t("block_server_run") };
  if (e.undone) return { text: t("block_undone_retry") };
  if (!e.finished) return { text: t("block_unfinished") };
  if (e.summary.failed === 0) return { text: t("block_no_failed") };
  if (!e.plan_exists) return { text: t("block_plan_missing_retry") };
  if (busy) return { text: busy };
  return null;
}

export function undoConfirmLines(e: ReportEntry, u: UndoPreview): { title: string; lines: string[] } {
  const lines: string[] = [];
  if (u.restore > 0) {
    lines.push(t("undo_restore", { count: u.restore }));
  }
  if (u.rename_back > 0) {
    lines.push(t("undo_rename_back", { count: u.rename_back }));
  }
  if (u.remove_outputs > 0) {
    lines.push(t("undo_remove_outputs", { count: u.remove_outputs }));
  }
  if (lines.length === 0) lines.push(t("undo_nothing"));
  lines.push(t("undo_nothing_overwritten"));
  const when = runTime(e.started);
  return { title: when ? t("undo_title_when", { when }) : t("undo_title"), lines };
}

export function retryConfirmLines(e: ReportEntry): string[] {
  const n = e.summary.failed;
  return [
    t("retry_failed", { count: n }),
    t("retry_new_plan"),
    t("retry_nothing_runs"),
  ];
}

export function undoResultText(restored: number, removed: number): string {
  return t("undo_result", { restored, removed });
}
