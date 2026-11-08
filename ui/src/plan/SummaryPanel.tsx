import { sameGroup, type GroupId, type GroupRow, type PlanSummary } from "./model";

interface Props {
  summary: PlanSummary;
  selected: GroupId | null;
  onSelect: (g: GroupId) => void;
}

/// Files per target with a bar of their share, then the groups that need a
/// look (review, unmatched, unchanged), then warnings.
export function SummaryPanel({ summary, selected, onSelect }: Props) {
  const share = (n: number) => (summary.total > 0 ? n / summary.total : 0);
  const row = (g: GroupRow, tone?: string) => (
    <li key={g.label}>
      <button
        type="button"
        className={`tray-row${tone ? ` ${tone}` : ""}`}
        aria-pressed={sameGroup(selected, g.id)}
        disabled={g.count === 0}
        onClick={() => onSelect(g.id)}
        title={g.unknown ? "This target is no longer in the config" : undefined}
      >
        <span className="tray-name">
          {g.label}
          {g.unknown && <span className="tray-flag">not in config</span>}
        </span>
        <span className="tray-count">{g.count.toLocaleString("en-US")}</span>
        <span className="tray-bar" aria-hidden>
          <span style={{ width: `${Math.max(share(g.count) * 100, g.count > 0 ? 1.5 : 0)}%` }} />
        </span>
      </button>
    </li>
  );
  const [review, unmatched, unchanged] = summary.others;

  return (
    <nav className="tray" aria-label="Plan summary">
      <h2>
        By target
        <span className="count">{summary.targets.reduce((n, t) => n + t.count, 0).toLocaleString("en-US")}</span>
      </h2>
      {summary.targets.length === 0 ? (
        <p className="empty small">No file landed in a target</p>
      ) : (
        <ul>{summary.targets.map((t) => row(t))}</ul>
      )}
      <h2>Everything else</h2>
      <ul>
        {row(review, review.count > 0 ? "attention" : undefined)}
        {row(unmatched)}
        {row(unchanged, "quiet")}
      </ul>
      {summary.warnings.length > 0 && (
        <details className="tray-warnings">
          <summary>
            Warnings <span className="count">{summary.warnings.length}</span>
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
