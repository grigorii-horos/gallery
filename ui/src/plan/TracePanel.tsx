import { useState } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
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
  const { t } = useTranslation();
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
    <aside className="trace" aria-label={t("trace_aria")}>
      <header className="trace-head">
        <h2 title={trace.source}>{baseName(trace.source)}</h2>
        <button type="button" className="btn quiet icon" aria-label={t("close")} onClick={onClose}>
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
          <h3>{t("trace_decision")}</h3>
          <p className={`decision ${d.kind}`}>{decisionText(d)}</p>
          {(d.kind === "ai" || d.kind === "review") && d.reason && <blockquote className="reason">{d.reason}</blockquote>}
          {d.kind === "ai" && (
            <p className="muted small">{t("trace_confidence", { value: d.low ? t("trace_conf_low") : t("trace_conf_normal") })}</p>
          )}
        </section>

        {trace.outputs.length > 0 && (
          <section>
            <h3>
              {t("trace_outputs")} <span className="count">{num(trace.outputs.length)}</span>
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
              {t("trace_why")}
            </button>
          </section>
        )}

        <section className="decide">
          {saved && (
            <p className="saved">
              {saved} {t("trace_rebuild")}
            </p>
          )}
          {error && <p className="bad-text">{error}</p>}
          {!choosing ? (
            <div className="decide-row">
              <button type="button" className="btn" onClick={() => setChoosing(true)}>
                {t("trace_change_target")}
              </button>
              {current && (
                <button
                  type="button"
                  className="btn quiet"
                  onClick={() => act(onRemove, t("trace_removed"))}
                >
                  {t("trace_remove")}
                </button>
              )}
            </div>
          ) : (
            <div className="choose" role="group" aria-label={t("trace_new_target")}>
              <ul>
                <li>
                  <button
                    type="button"
                    className="choice keep"
                    aria-pressed={current?.kind === "keep"}
                    onClick={() => act(() => onSet({ kind: "keep" }), t("trace_saved_keep"))}
                  >
                    <span>{t("trace_keep")}</span>
                    <span className="muted small">{t("trace_keep_hint")}</span>
                  </button>
                </li>
                {targets.map((choice) => {
                  const on = current?.kind === "target" && current.name === choice.name;
                  return (
                    <li key={choice.name}>
                      <button
                        type="button"
                        className="choice"
                        aria-pressed={on}
                        onClick={() =>
                          act(() => onSet({ kind: "target", name: choice.name }), t("trace_saved_target", { name: choice.name }))
                        }
                      >
                        <span>{choice.name}</span>
                        <span className="muted small">{choice.stage}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
              <button type="button" className="btn quiet" onClick={() => setChoosing(false)}>
                {t("trace_keep_as_is")}
              </button>
            </div>
          )}
        </section>
      </div>
    </aside>
  );
}

function OutputBlock({ o }: { o: OutputView }) {
  const { t } = useTranslation();
  return (
    <li className="output-block">
      <p className={`op op-${o.op}`}>{o.opLabel}</p>
      <Pipe title={t("pipe_content")} steps={o.content} empty={t("pipe_content_empty")} />
      <Pipe title={t("pipe_name")} steps={o.rename} empty={t("pipe_name_empty")} />
      <p className="dest">
        <Code title={o.dest}>{o.dest}</Code>
      </p>
    </li>
  );
}

function Pipe({ title, steps, empty }: { title: string; steps: StepView[]; empty: string }) {
  const { t } = useTranslation();
  return (
    <div className="pipe-row">
      <span className="pipe-title">{title}</span>
      {steps.length === 0 ? (
        <span className="muted small">{empty}</span>
      ) : (
        <ol className="pipe-steps">
          {steps.map((s, i) => (
            <li key={i} className={s.matched ? "hit" : "miss"} title={s.matched ? t("pipe_after", { after: s.after }) : t("pipe_no_match")}>
              <span className="step-name">{s.bundle ? `${s.bundle} › ${s.preset}` : s.preset}</span>
              {s.matched && <span className="step-after">{s.after}</span>}
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
