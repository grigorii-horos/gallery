import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import type { PilotFile, PilotOutput } from "../api";
import { Thumb } from "../plan/Thumb";
import { thumbQueue } from "../plan/queue";
import { EstimateBlock } from "../run/EstimateBlock";
import { factRows, pilotHeadline } from "../run/model";
import type { PilotRun } from "../run/useRun";

interface Props {
  pilot: PilotRun;
  /** The current plan, which holds what the pilot left (it is rebuilt after the pilot). */
  planPath: string | null;
  /** Another operation holds the slot (its text), or null. */
  busy: string | null;
  onContinue: () => void;
  onRollback: () => void;
}

/// What the pilot did, file by file (before → after), and the two ways on:
/// continue with the rest of the plan, or undo the pilot.
export function PilotPanel({ pilot, planPath, busy, onContinue, onRollback }: Props) {
  const { t } = useTranslation();
  const r = pilot.result;
  // A pilot with anomalies (or stopped by them) goes on only when the user accepts them.
  const [accepted, setAccepted] = useState(false);
  const flagged = !!r && (r.anomalies > 0 || r.stopped_by_anomalies);
  const head = r ? pilotHeadline(r) : null;
  return (
    <section className="pilot" aria-label={t("pilot_result_aria")}>
      {pilot.resultError && (
        <div className="notice bad">
          <p className="notice-title">{t("pilot_unreadable")}</p>
          <pre className="notice-text">{pilot.resultError}</pre>
        </div>
      )}
      {!r && !pilot.resultError && <p className="muted small">{t("pilot_reading")}</p>}
      {r && head && (
        <>
          <p className={`run-result-title pilot-title ${head.tone}`} role="status">
            {head.title}
          </p>
          <ul className="pilot-files">
            {r.files.map((f) => (
              <PilotCard key={f.source} file={f} />
            ))}
          </ul>
        </>
      )}
      {planPath && <EstimateBlock planPath={planPath} title={t("pilot_rest_estimate")} />}
      {busy && <p className="run-block">{busy}</p>}
      {flagged && (
        <label className="modal-check">
          <input type="checkbox" checked={accepted} onChange={(e) => setAccepted(e.target.checked)} />
          {t("pilot_accept")}
        </label>
      )}
      <div className="run-actions">
        {(!flagged || accepted) && (
          <button type="button" className="btn primary" disabled={!r || !!busy} onClick={onContinue}>
            {t("pilot_continue")}
          </button>
        )}
        <button type="button" className="btn" disabled={!r || !!busy} onClick={onRollback}>
          {t("pilot_undo")}
        </button>
      </div>
      <p className="muted small">{t("pilot_note")}</p>
    </section>
  );
}

function PilotCard({ file }: { file: PilotFile }) {
  const { t } = useTranslation();
  return (
    <li className="pilot-file">
      <p className="pilot-src" title={file.source}>
        <span className="muted">{file.target}</span> <bdi className="code">{file.source}</bdi>
      </p>
      {file.error && <p className="bad-text small">{file.error}</p>}
      {file.status !== "done" && !file.error && <p className="warn-text small">{statusText(file.status, t)}</p>}
      {file.outputs.map((o) => (
        <PilotOut key={o.planned} file={file} out={o} />
      ))}
    </li>
  );
}

function statusText(s: PilotFile["status"], t: TFunction): string {
  switch (s) {
    case "stale":
      return t("row_stale");
    case "missing":
      return t("file_missing");
    default:
      return t("not_processed");
  }
}

function PilotOut({ file, out }: { file: PilotFile; out: PilotOutput }) {
  const { t } = useTranslation();
  const rows = factRows(file.before, out.after);
  return (
    <div className="pilot-out">
      <span className="pilot-thumb">{out.actual && <Thumb path={out.actual} queue={thumbQueue} />}</span>
      <div className="pilot-detail">
        <p className="pilot-path" title={out.actual ?? out.planned}>
          <bdi className="code">{out.actual ?? out.planned}</bdi>
        </p>
        {out.error && <p className="bad-text small">{out.error}</p>}
        {out.actual && (
          <dl className="pilot-facts">
            {rows.map((row) => (
              <div key={row.label} className={row.changed && row.id !== "size" ? "changed" : undefined}>
                <dt>{row.label}</dt>
                <dd>
                  {row.before} → {row.after}
                </dd>
              </div>
            ))}
          </dl>
        )}
        {out.anomalies.map((a) => (
          <p key={a} className="bad-text small">
            {t("pilot_mismatch_line", { what: a })}
          </p>
        ))}
      </div>
    </div>
  );
}
