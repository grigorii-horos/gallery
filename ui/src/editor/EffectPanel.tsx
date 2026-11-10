import { useState } from "react";
import { useTranslation } from "react-i18next";
import { t } from "../i18n";
import { num } from "../i18n/format";
import { outcomeTitle, breakdown, targetLine, total, type EffectDiff, type ExampleGroup } from "./effect";
import type { EffectState } from "./useEffectPreview";

const base = (p: string) => p.split("/").filter(Boolean).pop() ?? p;

/// "Effect" under the save bar: what the unsaved edits do to the files.
export function EffectPanel({ state }: { state: EffectState }) {
  useTranslation();
  const { diff, loading, error, counting, stopped, stop, recalc } = state;
  return (
    <section className="effect" aria-label={t("effect_title")} aria-busy={loading}>
      <header className="effect-head">
        <h3>{t("effect_title")}</h3>
        {loading && !counting && <span className="muted">{t("effect_calculating")}</span>}
      </header>
      {stopped && (
        <p className="effect-stopped muted" role="status">
          <span>{t("effect_stopped")}</span>
          <button type="button" className="btn quiet" onClick={recalc}>
            {t("effect_recalculate")}
          </button>
        </p>
      )}
      {loading && counting && (
        <p className="effect-counting muted" role="status">
          <span>{t("effect_counting", { done: counting.done, total: counting.total })}</span>
          <progress value={counting.done} max={counting.total} aria-label={t("effect_counting_aria")} />
          <button type="button" className="btn quiet" onClick={stop}>
            {t("effect_stop")}
          </button>
        </p>
      )}
      {error && <p className="effect-error">{error}</p>}
      {diff ? <EffectView diff={diff} /> : !error && !stopped && <p className="muted">{t("effect_working")}</p>}
    </section>
  );
}

export function EffectView({ diff }: { diff: EffectDiff }) {
  useTranslation();
  const n = diff.changed;
  return (
    <div className="effect-body">
      <p className="effect-total">
        {n === 0 ? (
          <>{t("effect_none", { count: diff.files })}</>
        ) : (
          <>
            {t("effect_some", { changed: n, count: diff.files, before: breakdown(diff.before) || "—", after: breakdown(diff.after) || "—" })}
          </>
        )}
        {diff.after.unmatched !== diff.before.unmatched && (
          <>
            {" "}
            {t("effect_unmatched", { before: diff.before.unmatched, after: diff.after.unmatched })}
          </>
        )}
        {diff.after.waiting_ai > 0 && <> {t("effect_waiting", { n: diff.after.waiting_ai })}</>}
      </p>
      {diff.targets.some((x) => total(x.after) > 0 || total(x.before) > 0) && (
        <ul className="effect-targets">
          {diff.targets
            .filter((x) => x.changed > 0 || total(x.after) > 0)
            .map((x) => (
              <li key={x.name} className={x.changed > 0 ? "changed" : ""}>
                <strong>{x.name}</strong> {targetLine(x)}
              </li>
            ))}
        </ul>
      )}
      {diff.examples.map((g) => (
        <Examples key={g.outcome} group={g} />
      ))}
    </div>
  );
}

function Examples({ group }: { group: ExampleGroup }) {
  useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <div className="effect-group">
      <button type="button" className="btn quiet" aria-expanded={open} onClick={() => setOpen(!open)}>
        {outcomeTitle(group.outcome)}: {num(group.count)} {open ? "▾" : "▸"}
      </button>
      {open && (
        <ul className="effect-files">
          {group.files.map((f) => (
            <li key={f.path} title={f.path}>
              <code>{base(f.path)}</code>: {f.before.label} → {f.after.label}
            </li>
          ))}
          {group.count > group.files.length && <li className="muted">{t("triage_and_more", { n: group.count - group.files.length })}</li>}
        </ul>
      )}
    </div>
  );
}
