import { useState } from "react";
import { errorText, type OverrideAction } from "../api";
import { Code } from "../components/ConfigGate";
import { baseName, decisionText, dirName, type OutputView, type StepView, type Trace } from "./model";
import { Thumb } from "./Thumb";
import type { ThumbQueue } from "./thumbQueue";

export interface TargetChoice {
  name: string;
  stage: string;
}

interface Props {
  trace: Trace;
  queue: ThumbQueue;
  targets: TargetChoice[];
  onSet: (action: OverrideAction) => Promise<void>;
  onRemove: () => Promise<void>;
  onClose: () => void;
  /** Open "Why this file?" for the file. */
  onExplain?: () => void;
}

/// Why a file goes where it goes: the decision, every output with its
/// operation, the content and name pipes, the final path; and a way to
/// decide by hand.
export function TracePanel({ trace, queue, targets, onSet, onRemove, onClose, onExplain }: Props) {
  const [choosing, setChoosing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const d = trace.decision;

  const act = async (f: () => Promise<void>, done: string) => {
    setError(null);
    try {
      await f();
      setSaved(done);
      setChoosing(false);
    } catch (e) {
      setError(errorText(e));
    }
  };
  const current = trace.override;

  return (
    <aside className="trace" aria-label="File trace">
      <header className="trace-head">
        <h2 title={trace.source}>{baseName(trace.source)}</h2>
        <button type="button" className="btn quiet icon" aria-label="Close" onClick={onClose}>
          ×
        </button>
      </header>
      <div className="trace-scroll">
        <div className="trace-thumb">
          <Thumb path={trace.source} queue={queue} fit="contain" />
        </div>
        <p className="trace-dir">
          <Code title={trace.source}>{dirName(trace.source)}</Code>
        </p>

        <section>
          <h3>Decision</h3>
          <p className={`decision ${d.kind}`}>{decisionText(d)}</p>
          {(d.kind === "ai" || d.kind === "review") && d.reason && <blockquote className="reason">{d.reason}</blockquote>}
          {d.kind === "ai" && (
            <p className="muted small">Confidence: {d.low ? "low, accepted by the “Accept uncertain” flag" : "normal"}</p>
          )}
        </section>

        {trace.outputs.length > 0 && (
          <section>
            <h3>
              Outputs <span className="count">{trace.outputs.length}</span>
            </h3>
            <ol className="outputs">
              {trace.outputs.map((o, i) => (
                <OutputBlock key={i} o={o} />
              ))}
            </ol>
          </section>
        )}

        {onExplain && (
          <section>
            <button type="button" className="btn" onClick={onExplain}>
              Why?
            </button>
          </section>
        )}

        <section className="decide">
          {saved && (
            <p className="saved">
              {saved} Rebuild the plan to see the file in its new place.
            </p>
          )}
          {error && <p className="bad-text">{error}</p>}
          {!choosing ? (
            <div className="decide-row">
              <button type="button" className="btn" onClick={() => setChoosing(true)}>
                Change target…
              </button>
              {current && (
                <button
                  type="button"
                  className="btn quiet"
                  onClick={() => act(onRemove, "Manual decision removed.")}
                >
                  Remove manual decision
                </button>
              )}
            </div>
          ) : (
            <div className="choose" role="group" aria-label="New target">
              <ul>
                <li>
                  <button
                    type="button"
                    className="choice keep"
                    aria-pressed={current?.kind === "keep"}
                    onClick={() => act(() => onSet({ kind: "keep" }), "Decision saved: keep in source.")}
                  >
                    <span>Keep in source</span>
                    <span className="muted small">leave the file alone</span>
                  </button>
                </li>
                {targets.map((t) => {
                  const on = current?.kind === "target" && current.name === t.name;
                  return (
                    <li key={t.name}>
                      <button
                        type="button"
                        className="choice"
                        aria-pressed={on}
                        onClick={() =>
                          act(() => onSet({ kind: "target", name: t.name }), `Decision saved: “${t.name}”.`)
                        }
                      >
                        <span>{t.name}</span>
                        <span className="muted small">{t.stage}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
              <button type="button" className="btn quiet" onClick={() => setChoosing(false)}>
                Keep as is
              </button>
            </div>
          )}
        </section>
      </div>
    </aside>
  );
}

function OutputBlock({ o }: { o: OutputView }) {
  return (
    <li className="output-block">
      <p className={`op op-${o.op}`}>{o.opLabel}</p>
      <Pipe title="Content" steps={o.content} empty="no processing, byte-for-byte copy" />
      <Pipe title="Name" steps={o.rename} empty="name unchanged" />
      <p className="dest">
        <Code title={o.dest}>{o.dest}</Code>
      </p>
    </li>
  );
}

function Pipe({ title, steps, empty }: { title: string; steps: StepView[]; empty: string }) {
  return (
    <div className="pipe-row">
      <span className="pipe-title">{title}</span>
      {steps.length === 0 ? (
        <span className="muted small">{empty}</span>
      ) : (
        <ol className="pipe-steps">
          {steps.map((s, i) => (
            <li key={i} className={s.matched ? "hit" : "miss"} title={s.matched ? `after the step: ${s.after}` : "the step's mask did not match"}>
              <span className="step-name">{s.bundle ? `${s.bundle} › ${s.preset}` : s.preset}</span>
              {s.matched && <span className="step-after">{s.after}</span>}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
