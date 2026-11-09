import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { t, type MessageKey } from "../i18n";
import { num } from "../i18n/format";
import { errorText, planApi, type Explanation,
  type ItemCheck, type FilterCheck, type OutputCheck, type StepCheck } from "../api";
import { Code } from "../components/ConfigGate";
import { baseName, dirName } from "./model";

const OP: Record<OutputCheck["op"], MessageKey> = {
  write: "explain_op_write",
  rename: "explain_op_rename",
  keep: "explain_op_keep",
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
  useTranslation();
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
    <aside className="trace explain" aria-label={t("explain_aria")}>
      <header className="trace-head">
        <h2 title={path}>{baseName(path)}</h2>
        {onTrace && (
          <button type="button" className="btn quiet" onClick={onTrace}>
            {t("explain_trace")}
          </button>
        )}
        <button type="button" className="btn quiet icon" aria-label={t("close")} onClick={onClose}>
          ×
        </button>
      </header>
      <div className="trace-scroll">
        {!cur && <p className="muted">{t("explain_working")}</p>}
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
      return t("explain_scope_scanned", { source: s.source });
    case "missing":
      return t("explain_scope_missing");
    case "not_file":
      return t("explain_scope_not_file");
    case "no_source":
      return t("explain_scope_no_source");
    case "hidden":
      return t("explain_scope_hidden", { source: s.source, part: s.part });
    case "skip":
      return t("explain_scope_skip", { source: s.source, part: s.part, glob: s.glob });
    case "not_recursive":
      return t("explain_scope_not_recursive", { source: s.source });
  }
}

function otherTargetsText(ex: Explanation): string {
  const n = ex.other_targets.length;
  const source = ex.scope.kind === "scanned" ? ex.scope.source : "";
  return t("explain_other_targets", { count: n, source });
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

function filterText(f: FilterCheck): string {
  const what = f.ai ? t("explain_prefilter") : t("explain_filter_n", { n: f.index + 1 });
  if (!f.source_ok) return t("explain_filter_source", { what, sources: f.sources.join(", ") });
  const m = f.matches
    ? t(f.matches.matched ? "explain_match_took" : "explain_match_not_took", { items: maskText(f.matches) })
    : t("explain_match_any");
  const x = f.exclude
    ? t(f.exclude.matched ? "explain_exclude_fired" : "explain_exclude_not_fired", { items: maskText(f.exclude) })
    : "";
  return t("explain_filter_line", { what, m, x });
}

/// The core's explanation as a panel: the verdict first, then the checks in
/// the order the scan makes them. Display only.
export function ExplainView({ ex }: { ex: Explanation }) {
  useTranslation();
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
        <h3>{t("explain_source")}</h3>
        <p>{scopeText(ex)}</p>
      </section>

      {ex.targets.length > 0 && (
        <section>
          <h3>{t("explain_targets")}</h3>
          <ol className="explain-targets">
            {ex.targets.map((tg, i) => (
              <li key={i} className={tg.taken ? "taken" : tg.candidate ? "cand" : "no"}>
                <p>
                  <strong>{tg.name}</strong> {tg.ai && <span className="muted small">{t("stage_ai")}</span>}{" "}
                  <span className="small">{tg.taken ? t("explain_took") : tg.candidate ? t("explain_fits") : t("explain_no_fit")}</span>
                </p>
                {tg.miss && (
                  <p className="muted small explain-miss">
                    {t("explain_miss", { condition: tg.miss.condition, detail: tg.miss.detail })}
                  </p>
                )}
                <ul className="explain-filters">
                  {tg.filters.map((f) => (
                    <li key={f.index} className={f.fit}>
                      {filterText(f)}
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
          <h3>{t("stage_ai")}</h3>
          <p>{t("explain_ai_not_asked", { candidates: ex.ai.candidates.join(", ") })}</p>
        </section>
      )}
      {ex.ai.kind === "cached" && (
        <section>
          <h3>{t("explain_ai_cached")}</h3>
          <p>
            {ex.ai.category ?? t("explain_no_category")}
            {ex.ai.low ? `, ${t("explain_unsure")}` : ""}
          </p>
          {ex.ai.reason && <blockquote className="reason">{ex.ai.reason}</blockquote>}
        </section>
      )}

      {d && (
        <section>
          <h3>{t("trace_decision")}</h3>
          <p className={`decision ${d.kind}`}>{decisionLine(d)}</p>
        </section>
      )}

      {ex.outputs.length > 0 && (
        <section>
          <h3>
            {t("trace_outputs")} <span className="count">{num(ex.outputs.length)}</span>
          </h3>
          <ol className="outputs">
            {ex.outputs.map((o, i) => (
              <li key={i} className="output-block">
                <p className={`op op-${o.op}`}>{t(OP[o.op])}</p>
                <StepList title={t("pipe_content")} steps={o.content} empty={t("explain_no_steps_copy")} />
                <StepList title={t("pipe_name")} steps={o.rename} empty={t("explain_no_steps")} />
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
      const via = d.via?.kind === "ai" ? t("explain_via_ai") : d.via?.kind === "manual" ? t("explain_via_manual") : t("explain_via_mask");
      return t("explain_d_target", { target: d.target, via });
    }
    case "review":
      return t("explain_d_review", { target: d.target });
    case "excluded":
      return t("explain_d_excluded");
    case "kept":
      return t("explain_d_kept");
    case "unmatched":
      return t("explain_d_unmatched");
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
                  ? t("explain_skipped", { why: s.skipped })
                  : s.matched
                    ? s.before === s.after
                      ? t("explain_applied_same")
                      : t("explain_applied")
                    : t("explain_mask_no_match")}
              </span>
              <span className="muted small step-mask">
                {t("explain_mask", { mask: s.mask_expanded && s.mask_expanded !== s.mask ? `${s.mask} = ${s.mask_expanded}` : s.mask })}
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
