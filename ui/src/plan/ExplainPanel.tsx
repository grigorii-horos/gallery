import { useEffect, useState } from "react";
import { errorText, planApi, type Explanation,
  type ItemCheck, type FilterCheck, type OutputCheck, type StepCheck, type TargetCheck } from "../api";
import { Code } from "../components/ConfigGate";
import { baseName, dirName } from "./model";

const STAGE: Record<TargetCheck["stage"], string> = {
  before_ai: "Before AI",
  ai: "AI",
  after_ai: "After AI",
};

const OP: Record<OutputCheck["op"], string> = {
  write: "Writing a new file",
  rename: "Renaming in place",
  keep: "Unchanged",
};

/// Loads the explanation of `path` (the core reads, nothing is changed).
export function ExplainPanel({
  path,
  configPath,
  onClose,
  onTrace,
}: {
  path: string;
  configPath: string | null;
  onClose: () => void;
  /** Back to the file's trace, when the file is in the plan. */
  onTrace?: () => void;
}) {
  const [state, setState] = useState<{ path: string; ex?: Explanation; error?: string } | null>(null);
  useEffect(() => {
    let live = true;
    planApi
      .explainFile(configPath, path)
      .then((ex) => live && setState({ path, ex }))
      .catch((e) => live && setState({ path, error: errorText(e) }));
    return () => {
      live = false;
    };
  }, [path, configPath]);
  const cur = state?.path === path ? state : null;
  return (
    <aside className="trace explain" aria-label="Why this file">
      <header className="trace-head">
        <h2 title={path}>{baseName(path)}</h2>
        {onTrace && (
          <button type="button" className="btn quiet" onClick={onTrace}>
            Trace
          </button>
        )}
        <button type="button" className="btn quiet icon" aria-label="Close" onClick={onClose}>
          ×
        </button>
      </header>
      <div className="trace-scroll">
        {!cur && <p className="muted">Working it out…</p>}
        {cur?.error && <p className="bad-text">{cur.error}</p>}
        {cur?.ex && <ExplainView ex={cur.ex} />}
      </div>
    </aside>
  );
}

function scopeText(ex: Explanation): string {
  const s = ex.scope;
  switch (s.kind) {
    case "scanned":
      return `source “${s.source}”, the scan sees it`;
    case "missing":
      return "the file does not exist";
    case "not_file":
      return "not a regular file";
    case "no_source":
      return "no source of the config contains the file";
    case "hidden":
      return `source “${s.source}”, hidden element “${s.part}”`;
    case "skip":
      return `source “${s.source}”, “${s.part}” is under skip “${s.glob}”`;
    case "not_recursive":
      return `source “${s.source}” is not recursive`;
  }
}

function otherTargetsText(ex: Explanation): string {
  const n = ex.other_targets.length;
  const word = n === 1 ? "target" : "targets";
  const source = ex.scope.kind === "scanned" ? ex.scope.source : "";
  return `${n} more ${word} not for source ${source}`;
}

/// Per item why it did or did not take the name, as the core words it.
function ItemList({ items }: { items: ItemCheck[] }) {
  return (
    <ul className="explain-items">
      {items.map((it, i) => (
        <li key={i} className={it.hit ? "hit" : "miss"}>
          <code>{it.item}</code>
          {it.expanded && <span className="muted"> = {it.expanded}</span>} — {it.reason}
          {it.children.some((c) => c.hit) && <ItemList items={it.children} />}
        </li>
      ))}
    </ul>
  );
}

function maskText(m: { items: { item: string }[] }): string {
  return `[${m.items.map((i) => i.item).join(", ")}]`;
}

function filterText(stage: TargetCheck["stage"], f: FilterCheck): string {
  const what = stage === "ai" ? "prefilter" : `filter ${f.index + 1}`;
  if (!f.source_ok) return `${what}: the source is not in the list [${f.sources.join(", ")}]`;
  const m = f.matches ? `match ${maskText(f.matches)} ${f.matches.matched ? "took it" : "did not take it"}` : "match: any file";
  const x = f.exclude ? `; exclude ${maskText(f.exclude)} ${f.exclude.matched ? "fired" : "did not fire"}` : "";
  return `${what}: ${m}${x}`;
}

/// The core's explanation as a panel: the verdict first, then the checks in
/// the order the scan makes them. Display only.
export function ExplainView({ ex }: { ex: Explanation }) {
  const d = ex.decision;
  return (
    <>
      <p className={`verdict ${ex.outcome}`} role="status">
        {ex.verdict}
      </p>
      <p className="trace-dir">
        <Code title={ex.path}>{dirName(ex.path)}</Code>
      </p>

      <section>
        <h3>Source</h3>
        <p>{scopeText(ex)}</p>
      </section>

      {ex.stages.length > 0 && (
        <section>
          <h3>Targets in order</h3>
          <ol className="explain-targets">
            {ex.stages.map((t, i) => (
              <li key={i} className={t.taken ? "taken" : t.candidate ? "cand" : "no"}>
                <p>
                  <span className="muted small">{STAGE[t.stage]}</span> <strong>{t.name}</strong>{" "}
                  <span className="small">{t.taken ? "took the file" : t.candidate ? "fits" : "does not fit"}</span>
                </p>
                <ul className="explain-filters">
                  {t.filters.map((f) => (
                    <li key={f.index} className={f.fit}>
                      {filterText(t.stage, f)}
                      {[f.matches, f.exclude].map(
                        (m, k) => m && <ItemList key={k} items={m.items} />,
                      )}
                    </li>
                  ))}
                </ul>
              </li>
            ))}
          </ol>
          {ex.other_targets.length > 0 && (
            <details className="other-targets">
              <summary>{otherTargetsText(ex)}</summary>
              <p className="muted small">{ex.other_targets.join(", ")}</p>
            </details>
          )}
        </section>
      )}

      {ex.ai.kind === "not_asked" && (
        <section>
          <h3>AI</h3>
          <p>AI was not asked: no saved answer (candidates: {ex.ai.candidates.join(", ")}).</p>
        </section>
      )}
      {ex.ai.kind === "cached" && (
        <section>
          <h3>AI (from cache)</h3>
          <p>
            {ex.ai.category ?? "no category"}
            {ex.ai.low ? ", unsure" : ""}
          </p>
          {ex.ai.reason && <blockquote className="reason">{ex.ai.reason}</blockquote>}
        </section>
      )}

      {d && (
        <section>
          <h3>Decision</h3>
          <p className={`decision ${d.kind}`}>{decisionLine(d)}</p>
        </section>
      )}

      {ex.outputs.length > 0 && (
        <section>
          <h3>
            Outputs <span className="count">{ex.outputs.length}</span>
          </h3>
          <ol className="outputs">
            {ex.outputs.map((o, i) => (
              <li key={i} className="output-block">
                <p className={`op op-${o.op}`}>{OP[o.op]}</p>
                <StepList title="Content" steps={o.content} empty="no steps, byte-for-byte copy" />
                <StepList title="Name" steps={o.rename} empty="no steps" />
                <p className="dest">
                  <Code title={o.dest}>{o.dest}</Code>
                </p>
              </li>
            ))}
          </ol>
        </section>
      )}
    </>
  );
}

function decisionLine(d: NonNullable<Explanation["decision"]>): string {
  switch (d.kind) {
    case "target": {
      const via = d.via?.kind === "ai" ? "AI" : d.via?.kind === "manual" ? "manual decision" : "mask";
      return `Target “${d.target}” (${via})`;
    }
    case "review":
      return `To decide, AI suggests “${d.target}”`;
    case "excluded":
      return "Excluded by an exclude mask";
    case "kept":
      return "Keep in source (manual decision)";
    case "unmatched":
      return "No target took the file";
  }
}

function StepList({ title, steps, empty }: { title: string; steps: StepCheck[]; empty: string }) {
  return (
    <div className="pipe-row">
      <span className="pipe-title">{title}</span>
      {steps.length === 0 ? (
        <span className="muted small">{empty}</span>
      ) : (
        <ol className="explain-steps">
          {steps.map((s, i) => (
            <li key={i} className={s.skipped ? "skip" : s.matched ? "hit" : "miss"}>
              <span className="step-name">{s.bundle ? `${s.bundle} › ${s.preset}` : s.preset}</span>{" "}
              <span className="small">
                {s.skipped
                  ? `skipped: ${s.skipped}`
                  : s.matched
                    ? s.before === s.after
                      ? "applied, same name"
                      : "applied"
                    : "mask did not match"}
              </span>
              <span className="muted small step-mask">
                mask {s.mask_expanded && s.mask_expanded !== s.mask ? `${s.mask} = ${s.mask_expanded}` : s.mask}
              </span>
              <span className="muted small step-names">
                {s.before} → {s.after}
              </span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
