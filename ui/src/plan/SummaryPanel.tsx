import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import { sameGroup, type GroupId, type GroupRow, type PlanSummary } from "./model";

interface Props {
  summary: PlanSummary;
  selected: GroupId | null;
  onSelect: (g: GroupId) => void;
}

/// Files per target with a bar of their share, then the groups that need a
/// look (review, unmatched, unchanged), then warnings.
export function SummaryPanel({ summary, selected, onSelect }: Props) {
  const { t } = useTranslation();
  const share = (n: number) => (summary.total > 0 ? n / summary.total : 0);
  const row = (g: GroupRow, tone?: string) => (
    <li key={g.label}>
      <button
        type="button"
        className={`tray-row${tone ? ` ${tone}` : ""}`}
        aria-pressed={sameGroup(selected, g.id)}
        disabled={g.count === 0}
        onClick={() => onSelect(g.id)}
        title={g.unknown ? t("summary_unknown_title") : undefined}
      >
        <span className="tray-name">
          {g.label}
          {g.unknown && <span className="tray-flag">{t("summary_not_in_config")}</span>}
        </span>
        <span className="tray-count">{num(g.count)}</span>
        <span className="tray-bar" aria-hidden>
          <span style={{ width: `${Math.max(share(g.count) * 100, g.count > 0 ? 1.5 : 0)}%` }} />
        </span>
      </button>
    </li>
  );
  const [review, unmatched, unchanged] = summary.others;

  return (
    <nav className="tray" aria-label={t("summary_aria")}>
      <h2>
        {t("summary_by_target")}
        <span className="count">{num(summary.targets.reduce((n, x) => n + x.count, 0))}</span>
      </h2>
      {summary.targets.length === 0 ? (
        <p className="empty small">{t("summary_none")}</p>
      ) : (
        <ul>{summary.targets.map((x) => row(x))}</ul>
      )}
      <h2>{t("summary_else")}</h2>
      <ul>
        {row(review, review.count > 0 ? "attention" : undefined)}
        {row(unmatched)}
        {row(unchanged, "quiet")}
      </ul>
      {summary.warnings.length > 0 && (
        <details className="tray-warnings">
          <summary>
            {t("summary_warnings")} <span className="count">{num(summary.warnings.length)}</span>
          </summary>
          <ul>
            {summary.warnings.map((w, i) => (
              <li key={i}>{w}</li>
            ))}
          </ul>
        </details>
      )}
    </nav>
  );
}
