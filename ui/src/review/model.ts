// Display logic of the Review screen: series of review items, checkbox
// selection, keyboard focus and target shortcuts. Decisions themselves are
// recorded by the core (`set_overrides`); nothing here touches files.
import type { OverrideResult, OverrideView, ReviewItem } from "../api";

/** Neighbouring captures at most this far apart form one series. */
export const SERIES_GAP_S = 60;

export type SeriesKind = "ai" | "name";

export interface Series {
  /** Stable within a plan: the first file's path and the kind. */
  id: string;
  kind: SeriesKind;
  /** Source root; null for plans written before it was recorded. */
  source: string | null;
  items: ReviewItem[];
}

/** A file held back because its name could not be built: a target change does not help. */
export function isNameError(it: ReviewItem): boolean {
  return it.name_error === true;
}

export type DecisionState = "none" | "resolved" | "stale";

/**
 * A stored decision settles a file only for the version it was made for:
 * the core ignores it once size or mtime differ from the plan's, so such a
 * file stays in review after a rebuild.
 */
export function decisionState(it: ReviewItem, o: OverrideView | undefined): DecisionState {
  if (!o) return "none";
  return o.size === it.size && o.mtime_ns === it.mtime_ns ? "resolved" : "stale";
}

/** What went wrong in a batch: the count and the first two errors; null when all went in. */
export function failureText(results: OverrideResult[]): string | null {
  const bad = results.filter((r) => r.error);
  if (bad.length === 0) return null;
  const more = bad.length > 2 ? ` and ${bad.length - 2} more` : "";
  return `Not accepted ${bad.length} of ${results.length}: ${bad
    .slice(0, 2)
    .map((r) => r.error)
    .join("; ")}${more}`;
}

/** Seconds since an arbitrary epoch of a `YYYY-MM-DDTHH:MM:SS` local time. */
function seconds(t: string): number | null {
  const ms = Date.parse(`${t}Z`);
  return Number.isNaN(ms) ? null : ms / 1000;
}

/**
 * Review items in series: per kind (AI answers, then name errors) and
 * source (in plan order), sorted by capture time; a new series starts where
 * the gap to the previous file exceeds `gap` seconds. Files without a
 * capture time stand alone, after the dated ones of their source.
 */
export function groupSeries(items: ReviewItem[], gap = SERIES_GAP_S): Series[] {
  const buckets = new Map<string, { kind: SeriesKind; source: string | null; items: ReviewItem[] }>();
  for (const kind of ["ai", "name"] as const) {
    for (const it of items) {
      if ((kind === "name") !== isNameError(it)) continue;
      const source = it.source_root ?? null;
      const key = `${kind}\u0000${source ?? ""}`;
      let b = buckets.get(key);
      if (!b) buckets.set(key, (b = { kind, source, items: [] }));
      b.items.push(it);
    }
  }
  const out: Series[] = [];
  for (const b of buckets.values()) {
    const timed = b.items
      .map((it) => ({ it, t: it.captured ? seconds(it.captured) : null }))
      .sort((x, y) => {
        if (x.t === null || y.t === null) return x.t === y.t ? x.it.source.localeCompare(y.it.source) : x.t === null ? 1 : -1;
        return x.t - y.t || x.it.source.localeCompare(y.it.source);
      });
    let cur: Series | null = null;
    let last: number | null = null;
    for (const { it, t } of timed) {
      if (!cur || t === null || last === null || t - last > gap) {
        cur = { id: `${b.kind}:${it.source}`, kind: b.kind, source: b.source, items: [] };
        out.push(cur);
      }
      cur.items.push(it);
      last = t;
    }
  }
  return out;
}

/** First capture time and the seconds the series spans; null when undated. */
export function seriesSpan(s: Series): { start: string; seconds: number } | null {
  const first = s.items[0]?.captured;
  const lastC = s.items[s.items.length - 1]?.captured;
  if (!first || !lastC) return null;
  const a = seconds(first);
  const b = seconds(lastC);
  return a === null || b === null ? null : { start: first, seconds: b - a };
}

export function toggle(sel: ReadonlySet<string>, path: string): Set<string> {
  const next = new Set(sel);
  if (!next.delete(path)) next.add(path);
  return next;
}

export type GroupState = "all" | "some" | "none";

export function groupState(sel: ReadonlySet<string>, paths: string[]): GroupState {
  const n = paths.filter((p) => sel.has(p)).length;
  return n === 0 ? "none" : n === paths.length ? "all" : "some";
}

/** The group checkbox: checks every file of the group unless all are checked, then clears them. */
export function toggleGroup(sel: ReadonlySet<string>, paths: string[]): Set<string> {
  const next = new Set(sel);
  if (groupState(sel, paths) === "all") paths.forEach((p) => next.delete(p));
  else paths.forEach((p) => next.add(p));
  return next;
}

/** Every file without a decision yet. */
export function initialSelection(series: Series[], decided: ReadonlySet<string>): Set<string> {
  return new Set(series.flatMap((s) => s.items.map((i) => i.source)).filter((p) => !decided.has(p)));
}

export type FocusKey = "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown";

/** Left/right: previous/next file; up/down: first file of the previous/next series. */
export function nextFocus(series: Series[], current: string | null, key: FocusKey): string | null {
  const flat = series.flatMap((s, si) => s.items.map((i) => ({ path: i.source, si })));
  if (flat.length === 0) return null;
  const at = flat.findIndex((f) => f.path === current);
  if (at < 0) return flat[0].path;
  const si = flat[at].si;
  switch (key) {
    case "ArrowLeft":
      return flat[Math.max(0, at - 1)].path;
    case "ArrowRight":
      return flat[Math.min(flat.length - 1, at + 1)].path;
    case "ArrowUp":
      return series[Math.max(0, si - 1)].items[0].source;
    case "ArrowDown":
      return series[Math.min(series.length - 1, si + 1)].items[0].source;
  }
}

/** Targets on the number keys: recently used ones still in the config, then the rest in config order. */
export function shortcutTargets(recent: string[], targets: string[], max = 9): string[] {
  const head = recent.filter((r) => targets.includes(r));
  return [...head, ...targets.filter((t) => !head.includes(t))].slice(0, max);
}

export function pushRecent(recent: string[], name: string, max = 9): string[] {
  return [name, ...recent.filter((r) => r !== name)].slice(0, max);
}
