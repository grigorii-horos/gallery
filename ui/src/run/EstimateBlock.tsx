import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import { errorText, runApi, type Estimate } from "../api";
import { estimateView } from "./estimate";

/// "Estimate": saving, time range and file count of a run of the plan at
/// `planPath`, read from the core before anything moves. A failed estimate
/// is a quiet note: it never blocks the run.
export function EstimateBlock({
  planPath,
  pilot,
  title,
}: {
  planPath: string;
  /** Files per target when only a pilot slice of the plan runs. */
  pilot?: number;
  title?: string;
}) {
  const { t } = useTranslation();
  const heading = title ?? t("estimate_title");
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

  if (!state) return <p className="muted small">{t("estimate_loading")}</p>;
  if (!state.est) return <p className="muted small">{t("estimate_unavailable", { error: state.error })}</p>;
  const v = estimateView(state.est);
  return (
    <section className="estimate" aria-label={heading}>
      <h3 className="estimate-title">{heading}</h3>
      <dl className="estimate-facts">
        <div>
          <dt>{t("estimate_files")}</dt>
          <dd>{num(v.files)}</dd>
        </div>
        <div>
          <dt>{t("estimate_saving")}</dt>
          <dd>{v.saving}</dd>
        </div>
        <div>
          <dt>{t("estimate_time")}</dt>
          <dd>{v.time}</dd>
        </div>
      </dl>
      {v.rows.length > 0 && (
        <ul className="estimate-rows small muted">
          {v.rows.map((r) => (
            <li key={r.name}>
              {t("estimate_row", { name: r.name, files: t("files", { count: r.files }), saving: r.saving, time: r.time })}
            </li>
          ))}
        </ul>
      )}
      <p className="muted small">{t("estimate_note", { source: v.source })}</p>
    </section>
  );
}
