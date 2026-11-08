// Presentation of the estimate (the figures come from the core).
import type { Estimate, Range, TargetEstimate } from "../api";
import { formatSize } from "../plan/triage";

/** `45 s`, `12 min`, `2 h 05 min`. */
export function formatDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor((s + 30) / 60)} min`;
  return `${Math.floor(s / 3600)} h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")} min`;
}

/** `−1.2 MB` for a negative size. */
export function signedSize(bytes: number): string {
  return bytes < 0 ? `−${formatSize(-bytes)}` : formatSize(bytes);
}

/** `≈ 4.2 GB (3.1 GB – 5.0 GB)`; the range is dropped when it is one number. */
export function rangeText(r: Range, fmt: (n: number) => string): string {
  const [lo, mid, hi] = [fmt(r.low), fmt(r.mid), fmt(r.high)];
  return lo === hi ? `≈ ${mid}` : `≈ ${mid} (${lo} – ${hi})`;
}

export interface EstimateRow {
  name: string;
  files: number;
  saving: string;
  time: string;
}

export interface EstimateView {
  files: number;
  saving: string;
  time: string;
  /** "from past runs (N files)" or "rough estimate". */
  source: string;
  /** Per target; empty when the plan has one target (the total says it all). */
  rows: EstimateRow[];
}

const row = (t: TargetEstimate): EstimateRow => ({
  name: t.name || "total",
  files: t.files,
  saving: rangeText(t.saving_bytes, signedSize),
  time: rangeText(t.seconds, formatDuration),
});

export function estimateView(e: Estimate): EstimateView {
  const total = row(e.total);
  return {
    files: e.total.files,
    saving: total.saving,
    time: total.time,
    source: e.source_label,
    rows: e.targets.length > 1 ? e.targets.map(row) : [],
  };
}
