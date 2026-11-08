import { useTranslation } from "react-i18next";
import type { PendingState } from "../tray/usePending";
import { num } from "../i18n/format";

/// "Waiting to be sorted": what the background check counted, with the way on to the plan.
export function PendingBanner({ pending, onPlan }: { pending: PendingState; onPlan: () => void }) {
  const { t } = useTranslation();
  const s = pending.snapshot;
  if (!s || s.error || s.total === 0) return null;
  const targets = Object.entries(s.by_target)
    .map(([name, n]) => `${name}: ${num(n)}`)
    .join(", ");
  return (
    <div className="notice pending-banner" role="status">
      <p>
        <strong>{t("pending_to_sort", { n: s.total })}</strong>
        <span className="muted">
          {" "}
          — {t("pending_ready", { n: s.to_process })}
          {s.need_ai > 0 ? `, ${t("pending_waiting_ai", { n: s.need_ai })}` : ""}
          {targets ? ` (${targets})` : ""}
        </span>
      </p>
      <button type="button" className="btn" onClick={onPlan}>
        {t("pending_sort")}
      </button>
    </div>
  );
}
