import type { PendingState } from "../tray/usePending";

/// "Waiting to be sorted": what the background check counted, with the way on to the plan.
export function PendingBanner({ pending, onPlan }: { pending: PendingState; onPlan: () => void }) {
  const s = pending.snapshot;
  if (!s || s.error || s.total === 0) return null;
  const targets = Object.entries(s.by_target)
    .map(([name, n]) => `${name}: ${n}`)
    .join(", ");
  return (
    <div className="notice pending-banner" role="status">
      <p>
        <strong>To sort: {s.total}</strong>
        <span className="muted">
          {" "}
          — ready to run {s.to_process}
          {s.need_ai > 0 ? `, waiting for AI ${s.need_ai}` : ""}
          {targets ? ` (${targets})` : ""}
        </span>
      </p>
      <button type="button" className="btn" onClick={onPlan}>
        Sort
      </button>
    </div>
  );
}
