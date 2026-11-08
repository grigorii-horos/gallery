import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import {
  groupFiles,
  groupKey,
  groupLabel,
  allPlanFiles,
  indexPlan,
  summarize,
  traceOf,
  type GroupId,
  type PlanSummary,
} from "../plan/model";
import { ExplainPanel } from "../plan/ExplainPanel";
import { FileSearch } from "../plan/FileSearch";
import { SummaryPanel } from "../plan/SummaryPanel";
import { ThumbGrid } from "../plan/ThumbGrid";
import { TriagePanel } from "../plan/TriagePanel";
import { TracePanel, type TargetChoice } from "../plan/TracePanel";
import { thumbQueue as queue } from "../plan/queue";
import { targetChoices } from "../plan/targets";
import type { PlanState } from "../plan/usePlan";
import type { AppData } from "../store";

function firstGroup(s: PlanSummary): GroupId | null {
  return (s.targets.find((t) => t.count > 0) ?? s.others.find((g) => g.count > 0))?.id ?? null;
}

/// "Check": the plan by target with thumbnails and traces, the triage of
/// what did not match and the "Why?" search. Decisions made here are kept
/// as manual decisions; the plan follows them by itself.
export function CheckStep({
  data,
  plan,
  onConfigSaved,
}: {
  data: AppData;
  plan: PlanState;
  onConfigSaved: () => void;
}) {
  const { t } = useTranslation();
  const view = plan.view!;
  const configPath = data.configPath;
  const targets: TargetChoice[] = useMemo(() => targetChoices(data.summary, t), [data.summary, t]);
  const order = useMemo(() => targets.map((x) => x.name), [targets]);
  const summary = useMemo(() => summarize(view.plan, order, t), [view.plan, order, t]);
  const index = useMemo(() => indexPlan(view), [view]);
  // "Unmatched" opens the triage by default; the plain file grid is a tab away.
  const [flat, setFlat] = useState(false);
  const [group, setGroup] = useState<GroupId | null>(() => firstGroup(summary));
  const [file, setFile] = useState<string | null>(null);
  const files = useMemo(() => (group ? groupFiles(view.plan, group) : []), [view.plan, group]);
  const trace = file ? traceOf(index, file) : null;
  // The file "Why?" is open for: any path, not only one of the plan.
  const [explain, setExplain] = useState<string | null>(null);
  const planFiles = useMemo(() => allPlanFiles(view.plan), [view.plan]);

  const mark = (p: string) => {
    if (index.overrides.has(p)) return { text: t("mark_manual_short"), title: t("mark_manual") };
    const it = index.items.get(p);
    if (it?.via.kind === "ai" && it.via.low) return { text: "?", title: t("mark_ai_unsure") };
    return null;
  };

  return (
    <div className={`plan-body${trace || explain ? " with-trace" : ""}`}>
      <SummaryPanel
        summary={summary}
        selected={group}
        onSelect={(g) => {
          setGroup(g);
          setFlat(false);
          setFile(null);
          setExplain(null);
        }}
      />
      <section className="plan-files" aria-label={group ? groupLabel(group) : t("check_files")}>
        {group ? (
          <>
            <header className="files-head">
              <h2>{groupLabel(group)}</h2>
              <span className="count">{num(files.length)}</span>
              <FileSearch files={planFiles} onPick={setExplain} />
            </header>
            {group.kind === "unmatched" && (
              <div className="triage-tabs" role="group" aria-label={t("check_list_view")}>
                <button type="button" className="btn quiet" aria-pressed={!flat} onClick={() => setFlat(false)}>
                  {t("check_triage")}
                </button>
                <button type="button" className="btn quiet" aria-pressed={flat} onClick={() => setFlat(true)}>
                  {t("check_all_files")}
                </button>
              </div>
            )}
            {group.kind === "unmatched" && !flat ? (
              <TriagePanel
                configPath={configPath}
                version={view.built_at}
                queue={queue}
                onConfigSaved={onConfigSaved}
              />
            ) : (
            <ThumbGrid
              key={groupKey(group)}
              files={files}
              selected={file}
              onSelect={(p) => {
                setFile(p);
                setExplain(null);
              }}
              queue={queue}
              mark={mark}
              label={groupLabel(group)}
            />
            )}
          </>
        ) : (
          <p className="empty">{t("check_no_files")}</p>
        )}
      </section>
      {explain && (
        <ExplainPanel
          key={explain}
          path={explain}
          configPath={configPath}
          onClose={() => setExplain(null)}
          onTrace={
            traceOf(index, explain)
              ? () => {
                  setFile(explain);
                  setExplain(null);
                }
              : undefined
          }
        />
      )}
      {trace && !explain && (
        <TracePanel
          key={trace.source}
          trace={trace}
          queue={queue}
          targets={targets}
          onSet={(a) => plan.setOverride(trace.source, a)}
          onRemove={() => plan.removeOverride(trace.source)}
          onClose={() => setFile(null)}
          onExplain={() => setExplain(trace.source)}
        />
      )}
    </div>
  );
}
