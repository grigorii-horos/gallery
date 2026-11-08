import { useState } from "react";
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
  const r = pilot.result;
  // A pilot with anomalies (or stopped by them) goes on only when the user accepts them.
  const [accepted, setAccepted] = useState(false);
  const flagged = !!r && (r.anomalies > 0 || r.stopped_by_anomalies);
  const head = r ? pilotHeadline(r) : null;
  return (
    <section className="pilot" aria-label="Pilot result">
      {pilot.resultError && (
        <div className="notice bad">
          <p className="notice-title">The pilot result could not be read</p>
          <pre className="notice-text">{pilot.resultError}</pre>
        </div>
      )}
      {!r && !pilot.resultError && <p className="muted small">Reading the pilot result…</p>}
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
      {planPath && <EstimateBlock planPath={planPath} title="The rest: estimate" />}
      {busy && <p className="run-block">{busy}</p>}
      {flagged && (
        <label className="modal-check">
          <input type="checkbox" checked={accepted} onChange={(e) => setAccepted(e.target.checked)} />
          Continue despite mismatches with the sources
        </label>
      )}
      <div className="run-actions">
        {(!flagged || accepted) && (
          <button type="button" className="btn primary" disabled={!r || !!busy} onClick={onContinue}>
            Continue with the rest
          </button>
        )}
        <button type="button" className="btn" disabled={!r || !!busy} onClick={onRollback}>
          Undo pilot
        </button>
      </div>
      <p className="muted small">
        The pilot did not touch the other files of the plan; the plan refreshes by itself. Continuing asks for confirmation, like
        a regular run.
      </p>
    </section>
  );
}

function PilotCard({ file }: { file: PilotFile }) {
  return (
    <li className="pilot-file">
      <p className="pilot-src" title={file.source}>
        <span className="muted">{file.target}</span> <bdi className="code">{file.source}</bdi>
      </p>
      {file.error && <p className="bad-text small">{file.error}</p>}
      {file.status !== "done" && !file.error && <p className="warn-text small">{statusText(file.status)}</p>}
      {file.outputs.map((o) => (
        <PilotOut key={o.planned} file={file} out={o} />
      ))}
    </li>
  );
}

function statusText(s: PilotFile["status"]): string {
  switch (s) {
    case "stale":
      return "changed after the plan — left untouched";
    case "missing":
      return "file is missing";
    default:
      return "not processed";
  }
}

function PilotOut({ file, out }: { file: PilotFile; out: PilotOutput }) {
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
              <div key={row.label} className={row.changed && row.label !== "Size" ? "changed" : undefined}>
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
            Mismatch: {a}
          </p>
        ))}
      </div>
    </div>
  );
}
