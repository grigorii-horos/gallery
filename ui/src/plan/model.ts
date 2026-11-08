// Display logic of the Plan screen: grouping, summary and trace of a plan
// the core built. Nothing here decides anything about files.
import type {
  OutputOp,
  OverrideAction,
  Plan,
  PlanItem,
  PlannedOutput,
  PlanView,
  PlanWarning,
  ReviewItem,
  ScanStage,
  TraceStep,
} from "../api";

/** A group of files the summary lists and the grid shows. */
export type GroupId =
  | { kind: "target"; name: string }
  | { kind: "review" }
  | { kind: "unmatched" }
  | { kind: "unchanged" };

export interface GroupRow {
  id: GroupId;
  label: string;
  count: number;
  /** Not in the current config (renamed or removed since the scan). */
  unknown?: boolean;
}

export interface PlanSummary {
  /** Files the plan sees: planned + review + unmatched + unchanged. */
  total: number;
  targets: GroupRow[];
  others: GroupRow[];
  warnings: string[];
}

export function groupKey(g: GroupId): string {
  return g.kind === "target" ? `target:${g.name}` : g.kind;
}

export function sameGroup(a: GroupId | null, b: GroupId | null): boolean {
  return !!a && !!b && groupKey(a) === groupKey(b);
}

/**
 * Counts per target in config order (`order`: target names as the config
 * lists them), targets the config no longer has at the end; then review,
 * unmatched and unchanged; warnings as text.
 */
export function summarize(plan: Plan, order: string[] = []): PlanSummary {
  const counts = new Map<string, number>();
  for (const it of plan.items) counts.set(it.target, (counts.get(it.target) ?? 0) + 1);
  const known = order.filter((n) => counts.has(n));
  const extra = [...counts.keys()].filter((n) => !order.includes(n)).sort((a, b) => a.localeCompare(b));
  const targets: GroupRow[] = [
    ...known.map((name) => ({ id: { kind: "target", name } as GroupId, label: name, count: counts.get(name)! })),
    ...extra.map((name) => ({
      id: { kind: "target", name } as GroupId,
      label: name,
      count: counts.get(name)!,
      unknown: order.length > 0,
    })),
  ];
  const unchanged = plan.unchanged ?? [];
  const others: GroupRow[] = [
    { id: { kind: "review" }, label: "To decide", count: plan.review.length },
    { id: { kind: "unmatched" }, label: "Unmatched", count: plan.unmatched.length },
    { id: { kind: "unchanged" }, label: "Unchanged", count: unchanged.length },
  ];
  return {
    total: plan.items.length + plan.review.length + plan.unmatched.length + unchanged.length,
    targets,
    others,
    warnings: plan.warnings.map(warningText),
  };
}

export function warningText(w: PlanWarning): string {
  switch (w.kind) {
    case "series_split":
      return `A series was split across targets: ${baseName(w.a)} and ${baseName(w.b)}`;
    case "double_encode":
      return `Double re-encoding: ${baseName(w.source)} → ${w.dest}`;
    case "missing_server_path":
      return `No server path: ${w.what}`;
  }
}

/** Paths of the files in a group, in plan order. */
export function groupFiles(plan: Plan, g: GroupId): string[] {
  switch (g.kind) {
    case "target":
      return plan.items.filter((i) => i.target === g.name).map((i) => i.source);
    case "review":
      return plan.review.map((r) => r.source);
    case "unmatched":
      return plan.unmatched;
    case "unchanged":
      return plan.unchanged ?? [];
  }
}

export function groupLabel(g: GroupId): string {
  switch (g.kind) {
    case "target":
      return g.name;
    case "review":
      return "To decide";
    case "unmatched":
      return "Unmatched";
    case "unchanged":
      return "Unchanged";
  }
}

/** Why a file is where the plan puts it. */
export type Decision =
  | { kind: "mask"; target: string }
  | { kind: "ai"; target: string; reason: string; low: boolean }
  | { kind: "manual"; target: string }
  | { kind: "review"; suggested: string; reason: string }
  | { kind: "unmatched" }
  | { kind: "unchanged"; manualKeep: boolean };

export interface StepView {
  preset: string;
  matched: boolean;
  after: string;
  bundle?: string;
}

export interface OutputView {
  op: OutputOp;
  opLabel: string;
  dest: string;
  content: StepView[];
  rename: StepView[];
}

export interface Trace {
  source: string;
  group: GroupId;
  decision: Decision;
  outputs: OutputView[];
  /** The manual decision stored for this path, if any. */
  override: OverrideAction | null;
}

export const OP_LABEL: Record<OutputOp, string> = {
  write: "Write a new file",
  rename: "Move the source",
  keep: "Left as is",
};

/**
 * Content steps, then name steps. Plans record the pipe of each step; for
 * older plans the name pipe starts where the step index starts again at 0
 * (a single run is taken as the name pipe when the output has no content
 * step, else as the content pipe).
 */
export function splitTrace(o: PlannedOutput): { content: TraceStep[]; rename: TraceStep[] } {
  if (o.trace.every((s) => s.pipe)) {
    return {
      content: o.trace.filter((s) => s.pipe === "content"),
      rename: o.trace.filter((s) => s.pipe === "name"),
    };
  }
  const restart = o.trace.findIndex((s, i) => i > 0 && s.index === 0);
  if (restart > 0) return { content: o.trace.slice(0, restart), rename: o.trace.slice(restart) };
  return o.steps.length > 0 ? { content: o.trace, rename: [] } : { content: [], rename: o.trace };
}

function outputView(o: PlannedOutput): OutputView {
  const op = o.op ?? "write";
  const { content, rename } = splitTrace(o);
  const step = (s: TraceStep): StepView => ({ preset: s.preset, matched: s.matched, after: s.after, bundle: s.bundle });
  return { op, opLabel: OP_LABEL[op], dest: o.dest, content: content.map(step), rename: rename.map(step) };
}

function itemDecision(it: PlanItem): Decision {
  switch (it.via.kind) {
    case "mask":
      return { kind: "mask", target: it.target };
    case "ai":
      return { kind: "ai", target: it.target, reason: it.via.reason, low: !!it.via.low };
    case "manual":
      return { kind: "manual", target: it.target };
  }
}

/** Fast lookups of a plan by source path. */
export interface PlanIndex {
  items: Map<string, PlanItem>;
  review: Map<string, ReviewItem>;
  unmatched: Set<string>;
  unchanged: Set<string>;
  overrides: Map<string, OverrideAction>;
}

export function indexPlan(view: PlanView): PlanIndex {
  const p = view.plan;
  return {
    items: new Map(p.items.map((i) => [i.source, i])),
    review: new Map(p.review.map((r) => [r.source, r])),
    unmatched: new Set(p.unmatched),
    unchanged: new Set(p.unchanged ?? []),
    overrides: new Map(view.overrides.map((o) => [o.path, o.action])),
  };
}

/** The trace of one file, or null when the plan does not have it. */
export function traceOf(ix: PlanIndex, source: string): Trace | null {
  const override = ix.overrides.get(source) ?? null;
  const it = ix.items.get(source);
  if (it) {
    return {
      source,
      group: { kind: "target", name: it.target },
      decision: itemDecision(it),
      outputs: it.outputs.map(outputView),
      override,
    };
  }
  const r = ix.review.get(source);
  if (r) {
    return {
      source,
      group: { kind: "review" },
      decision: { kind: "review", suggested: r.suggested, reason: r.reason },
      outputs: [],
      override,
    };
  }
  if (ix.unmatched.has(source)) {
    return { source, group: { kind: "unmatched" }, decision: { kind: "unmatched" }, outputs: [], override };
  }
  if (ix.unchanged.has(source)) {
    return {
      source,
      group: { kind: "unchanged" },
      decision: { kind: "unchanged", manualKeep: override?.kind === "keep" },
      outputs: [],
      override,
    };
  }
  return null;
}

/** One line saying why the file is here. */
export function decisionText(d: Decision): string {
  switch (d.kind) {
    case "mask":
      return `Target mask “${d.target}”`;
    case "ai":
      return d.low ? `AI: “${d.target}”, low confidence (accepted)` : `AI: “${d.target}”`;
    case "manual":
      return `Manual decision: “${d.target}”`;
    case "review":
      return d.suggested === "—"
        ? "AI is unsure and suggested no target"
        : `AI is unsure, suggests “${d.suggested}”`;
    case "unmatched":
      return "No target fit";
    case "unchanged":
      return d.manualKeep
        ? "Manual decision: keep in source"
        : "Already in place or filtered out by an exclude mask";
  }
}

// ---------- scan progress ----------

export const SCAN_STAGES: { id: ScanStage; label: string }[] = [
  { id: "scan", label: "Finding files" },
  { id: "exif", label: "EXIF" },
  { id: "ai", label: "AI" },
  { id: "plan", label: "Plan" },
];

export interface StageState {
  done: number;
  total: number;
}

export type StageProgress = Partial<Record<ScanStage, StageState>>;

/** 0…1 of a stage; null when not started or the total is not known. */
export function stageFraction(s: StageState | undefined): number | null {
  if (!s || s.total <= 0) return null;
  return Math.min(1, s.done / s.total);
}

/** `done`/`total` as shown under a stage. */
export function stageCount(s: StageState | undefined): string {
  if (!s) return "";
  return s.total > 0 ? `${s.done} of ${s.total}` : `${s.done}`;
}

export function formatCost(ai: { input: number; output: number; usd: number | null }): string {
  const tokens = `${ai.input.toLocaleString("en-US")} in, ${ai.output.toLocaleString("en-US")} out`;
  if (ai.usd === null) return `AI: ${tokens} tokens`;
  return `AI: $${ai.usd.toFixed(ai.usd < 1 ? 4 : 2)} (${tokens})`;
}

// ---------- paths ----------

export function baseName(p: string): string {
  const i = p.lastIndexOf("/");
  return i >= 0 ? p.slice(i + 1) : p;
}

export function dirName(p: string): string {
  const i = p.lastIndexOf("/");
  return i > 0 ? p.slice(0, i) : i === 0 ? "/" : "";
}

/** `20261105-101500.plan.json` → `Nov 5, 10:15`; null when not that shape. */
export function planTime(path: string): string | null {
  const m = /(\d{4})(\d{2})(\d{2})-(\d{2})(\d{2})(\d{2})(?:-\d+)?(-retry(?:-\d+)?)?\.plan\.json$/.exec(path);
  if (!m) return null;
  const months = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
  ];
  const when = `${months[Number(m[2]) - 1]} ${Number(m[3])}, ${m[1]}, ${m[4]}:${m[5]}`;
  return m[7] ? `${when} (retry of failed)` : when;
}

// ---------- virtual grid ----------

export interface GridWindow {
  columns: number;
  /** First and one-past-last index to render. */
  start: number;
  end: number;
  /** Top of the first rendered row. */
  offsetY: number;
  totalHeight: number;
}

/**
 * Which cells of a grid of `count` square-ish cells (`cell` wide, `rowH`
 * tall, `gap` apart) are visible in a viewport of `width`×`height`
 * scrolled to `scrollTop`, plus `overscan` rows on each side.
 */
export function gridWindow(
  count: number,
  width: number,
  height: number,
  scrollTop: number,
  cell: number,
  rowH: number,
  gap: number,
  overscan = 2,
): GridWindow {
  const columns = Math.max(1, Math.floor((width + gap) / (cell + gap)));
  const rows = Math.ceil(count / columns);
  const pitch = rowH + gap;
  const totalHeight = rows > 0 ? rows * pitch - gap : 0;
  const firstRow = Math.max(0, Math.floor(scrollTop / pitch) - overscan);
  const lastRow = Math.min(rows, Math.ceil((scrollTop + height) / pitch) + overscan);
  return {
    columns,
    start: Math.min(count, firstRow * columns),
    end: Math.min(count, lastRow * columns),
    offsetY: firstRow * pitch,
    totalHeight,
  };
}

/**
 * Plan files whose path contains every word of `query` (case-insensitive),
 * names that start with the query first; at most `limit`.
 */
export function suggestFiles(paths: string[], query: string, limit = 8): string[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return [];
  const hits = paths.filter((p) => {
    const low = p.toLowerCase();
    return words.every((w) => low.includes(w));
  });
  const q = query.trim().toLowerCase();
  const rank = (p: string) => (baseName(p).toLowerCase().startsWith(q) ? 0 : 1);
  return hits
    .map((p, i) => ({ p, i }))
    .sort((a, b) => rank(a.p) - rank(b.p) || a.i - b.i)
    .slice(0, limit)
    .map((x) => x.p);
}

/** Every file path the plan lists, in group order. */
export function allPlanFiles(plan: Plan): string[] {
  return [
    ...plan.items.map((i) => i.source),
    ...plan.review.map((r) => r.source),
    ...plan.unmatched,
    ...(plan.unchanged ?? []),
  ];
}
