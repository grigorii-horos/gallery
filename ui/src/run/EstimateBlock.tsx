import { useEffect, useState } from "react";
import { errorText, runApi, type Estimate } from "../api";
import { estimateView } from "./estimate";

/// "Estimate": saving, time range and file count of a run of the plan at
/// `planPath`, read from the core before anything moves. A failed estimate
/// is a quiet note: it never blocks the run.
export function EstimateBlock({
  planPath,
  pilot,
  title = "Estimate",
}: {
  planPath: string;
  /** Files per target when only a pilot slice of the plan runs. */
  pilot?: number;
  title?: string;
}) {
  // Keyed by the plan it was read for: another plan reads as "loading" again.
  const key = `${planPath}|${pilot ?? ""}`;
  const [loaded, setLoaded] = useState<{ key: string; est?: Estimate; error?: string } | null>(null);
  useEffect(() => {
    let live = true;
    runApi.estimatePlan(planPath, pilot).then(
      (est) => live && setLoaded({ key, est }),
      (e) => live && setLoaded({ key, error: errorText(e) }),
    );
    return () => {
      live = false;
    };
  }, [planPath, pilot, key]);
  const state = loaded?.key === key ? loaded : null;

  if (!state) return <p className="muted small">Estimating…</p>;
  if (!state.est) return <p className="muted small">Estimate unavailable: {state.error}</p>;
  const v = estimateView(state.est);
  return (
    <section className="estimate" aria-label={title}>
      <h3 className="estimate-title">{title}</h3>
      <dl className="estimate-facts">
        <div>
          <dt>Files</dt>
          <dd>{v.files.toLocaleString("en-US")}</dd>
        </div>
        <div>
          <dt>Saving</dt>
          <dd>{v.saving}</dd>
        </div>
        <div>
          <dt>Time</dt>
          <dd>{v.time}</dd>
        </div>
      </dl>
      {v.rows.length > 0 && (
        <ul className="estimate-rows small muted">
          {v.rows.map((r) => (
            <li key={r.name}>
              {r.name}: {r.files.toLocaleString("en-US")} files, {r.saving}, {r.time}
            </li>
          ))}
        </ul>
      )}
      <p className="muted small">Estimate: {v.source}; it is a range, not a promise.</p>
    </section>
  );
}
