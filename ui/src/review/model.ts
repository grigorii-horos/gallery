// Display logic of the Review screen: series of review items, checkbox
// selection, keyboard focus and target shortcuts. Decisions themselves are
// recorded by the core (`set_overrides`); nothing here touches files.
import { t } from "../i18n";
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
  const head = bad
    .slice(0, 2)
    .map((r) => r.error)
    .join("; ");
  return bad.length > 2
    ? t("review_failures_more", { bad: bad.length, total: results.length, errors: head, more: bad.length - 2 })
    : t("review_failures", { bad: bad.length, total: results.length, errors: head });
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

/** Neighbouring captures at most this far apart belong to one event (the core's default gap, "4h"). */
export const EVENT_GAP_S = 4 * 3600;

/** A series held back by a `series` condition: all its members in review, side by side. */
export interface SeriesReviewGroup {
  /** Stable within a plan: the suggested best shot's path. */
  id: string;
  /** The suggested best shot. */
  best: string;
  /** Target the other members go to once the user confirms. */
  target: string;
  /** Members in the plan's order; only files held in this review. */
  items: ReviewItem[];
}

/** Review items that carry a `series`, grouped by their series (members listed in each). */
export function seriesReviews(items: ReviewItem[]): SeriesReviewGroup[] {
  const byBest = new Map<string, SeriesReviewGroup>();
  for (const it of items) {
    if (!it.series) continue;
    const key = it.series.best;
    let g = byBest.get(key);
    if (!g) byBest.set(key, (g = { id: `series:${key}`, best: key, target: it.suggested, items: [] }));
    g.items.push(it);
  }
  for (const g of byBest.values()) {
    const order = g.items[0].series!.members;
    g.items.sort((a, b) => order.indexOf(a.source) - order.indexOf(b.source));
  }
  return [...byBest.values()];
}

/** The review items of one event; the undated ones form the last event with `start` null. */
export interface EventGroup {
  id: string;
  start: string | null;
  items: ReviewItem[];
}

/**
 * Review items clustered into events by capture time across all sources: a
 * new event starts where the gap to the previous capture exceeds `gap`
 * seconds. Files without a capture time share one last event.
 */
export function groupEvents(items: ReviewItem[], gap = EVENT_GAP_S): EventGroup[] {
  const timed = items
    .map((it) => ({ it, t: it.captured ? seconds(it.captured) : null }))
    .filter((x): x is { it: ReviewItem; t: number } => x.t !== null)
    .sort((x, y) => x.t - y.t || x.it.source.localeCompare(y.it.source));
  const out: EventGroup[] = [];
  let cur: EventGroup | null = null;
  let last = 0;
  for (const { it, t } of timed) {
    if (!cur || t - last > gap) {
      cur = { id: `event:${it.source}`, start: it.captured ?? null, items: [] };
      out.push(cur);
    }
    cur.items.push(it);
    last = t;
  }
  const undated = items.filter((it) => !it.captured || seconds(it.captured) === null);
  if (undated.length > 0) out.push({ id: "event:undated", start: null, items: undated });
  return out;
}

/**
 * What an event block shows: the series cards and ordinary series with a
 * file in the event (a card is shown in every event its members belong to,
 * whole), and the number of distinct files that are then on screen.
 */
export function eventContents(
  ev: EventGroup,
  cards: SeriesReviewGroup[],
  series: Series[],
): { cards: SeriesReviewGroup[]; series: Series[]; files: number } {
  const inEvent = new Set(ev.items.map((i) => i.source));
  const hit = (items: ReviewItem[]) => items.some((i) => inEvent.has(i.source));
  const shownCards = cards.filter((g) => hit(g.items));
  const shownSeries = series.filter((s) => hit(s.items));
  const files = new Set([...inEvent, ...shownCards.flatMap((g) => g.items.map((i) => i.source)), ...shownSeries.flatMap((s) => s.items.map((i) => i.source))]);
  return { cards: shownCards, series: shownSeries, files: files.size };
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
