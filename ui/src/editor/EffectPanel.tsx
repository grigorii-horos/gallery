import { useState } from "react";
import { plural } from "./fields";
import { OUTCOME_TITLE, breakdown, targetLine, total, type EffectDiff, type ExampleGroup } from "./effect";
import type { EffectState } from "./useEffectPreview";

const base = (p: string) => p.split("/").filter(Boolean).pop() ?? p;

/// "Effect" under the save bar: what the unsaved edits do to the files.
export function EffectPanel({ state }: { state: EffectState }) {
  const { diff, loading, error } = state;
  return (
    <section className="effect" aria-label="Effect" aria-busy={loading}>
      <header className="effect-head">
        <h3>Effect</h3>
        {loading && <span className="muted">calculating…</span>}
      </header>
      {error && <p className="effect-error">{error}</p>}
      {diff ? <EffectView diff={diff} /> : !error && <p className="muted">Working out what will change…</p>}
    </section>
  );
}

export function EffectView({ diff }: { diff: EffectDiff }) {
  const n = diff.changed;
  return (
    <div className="effect-body">
      <p className="effect-total">
        {n === 0 ? (
          <>The changes affect none of the {diff.files} {plural(diff.files, "file", "files")}.</>
        ) : (
          <>
            {n} of {diff.files} {plural(diff.files, "file", "files")} will change. Now: {breakdown(diff.before) || "—"}. After: {breakdown(diff.after) || "—"}.
          </>
        )}
        {diff.after.unmatched !== diff.before.unmatched && (
          <>
            {" "}
            Unmatched: {diff.before.unmatched} → {diff.after.unmatched}.
          </>
        )}
        {diff.after.waiting_ai > 0 && <> Waiting for AI: {diff.after.waiting_ai}.</>}
      </p>
      {diff.targets.some((t) => total(t.after) > 0 || total(t.before) > 0) && (
        <ul className="effect-targets">
          {diff.targets
            .filter((t) => t.changed > 0 || total(t.after) > 0)
            .map((t) => (
              <li key={t.name} className={t.changed > 0 ? "changed" : ""}>
                <strong>{t.name}</strong> {targetLine(t)}
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
  const [open, setOpen] = useState(false);
  return (
    <div className="effect-group">
      <button type="button" className="btn quiet" aria-expanded={open} onClick={() => setOpen(!open)}>
        {OUTCOME_TITLE[group.outcome]}: {group.count} {open ? "▾" : "▸"}
      </button>
      {open && (
        <ul className="effect-files">
          {group.files.map((f) => (
            <li key={f.path} title={f.path}>
              <code>{base(f.path)}</code>: {f.before.label} → {f.after.label}
            </li>
          ))}
          {group.count > group.files.length && <li className="muted">…and {group.count - group.files.length} more</li>}
        </ul>
      )}
    </div>
  );
}
