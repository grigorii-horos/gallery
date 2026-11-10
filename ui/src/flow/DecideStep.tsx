import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { t } from "../i18n";
import { date as formatDate } from "../i18n/format";
import { errorText, type OverrideAction, type OverrideResult, type ReviewItem, type ReviewMember } from "../api";
import type { AppData } from "../store";
import { baseName } from "../plan/model";
import { thumbQueue as queue } from "../plan/queue";
import { targetChoices } from "../plan/targets";
import { Thumb } from "../plan/Thumb";
import { AiMetaView } from "../plan/AiMetaView";
import type { TargetChoice } from "../plan/TracePanel";
import type { PlanState } from "../plan/usePlan";
import {
  decisionState,
  eventContents,
  failureText,
  groupEvents,
  groupSeries,
  seriesReviews,
  groupState,
  initialSelection,
  nextFocus,
  pushRecent,
  seriesSpan,
  shortcutTargets,
  toggle,
  toggleGroup,
  type EventGroup,
  type FocusKey,
  type Series,
  type SeriesReviewGroup,
} from "../review/model";

const RECENT_KEY = "gallery.recentTargets";
/** The picker value of "keep in source"; never a target name. */
const KEEP = "\u0000keep";

function readRecent(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(RECENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

const toAction = (choice: string): OverrideAction =>
  choice === KEEP ? { kind: "keep" } : { kind: "target", name: choice };

function actionText(a: OverrideAction): string {
  return a.kind === "keep" ? t("decide_in_source") : a.kind === "series_best" ? t("decide_series_best") : a.name;
}

const memberOf = (i: ReviewItem): ReviewMember => ({ source_path: i.source, size: i.size, mtime_ns: i.mtime_ns });

/// Decide: the files the scan was not sure about (low-confidence AI
/// answers, names that did not fit), in series, with a target or "keep"
/// for each. A decision is saved as a manual decision and the plan follows it.
export function DecideStep({ data, plan }: { data: AppData; plan: PlanState }) {
  const { t } = useTranslation();
  const targets = useMemo(() => targetChoices(data.summary, t), [data.summary, t]);
  return <ReviewBody plan={plan} targets={targets} />;
}

function ReviewBody({ plan, targets }: { plan: PlanState; targets: TargetChoice[] }) {
  useTranslation();
  const view = plan.view!;
  // Series held by a `series` condition get their own side-by-side card.
  const series = useMemo(() => groupSeries(view.plan.review.filter((r) => !r.series)), [view.plan.review]);
  const cards = useMemo(() => seriesReviews(view.plan.review), [view.plan.review]);
  const events = useMemo(() => groupEvents(view.plan.review), [view.plan.review]);
  const [byEvent, setByEvent] = useState(false);
  // The best shot the user picked per series review; otherwise the suggested one.
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [eventChoice, setEventChoice] = useState<Record<string, string>>({});
  const overrides = useMemo(() => new Map(view.overrides.map((o) => [o.path, o])), [view.overrides]);
  // Resolved: a decision for this very version of the file. One for another
  // version is ignored by the core, so the file is not settled.
  const resolved = useMemo(
    () =>
      new Map(
        view.plan.review.flatMap((r) => {
          const o = overrides.get(r.source);
          return decisionState(r, o) === "resolved" ? [[r.source, o!.action] as const] : [];
        }),
      ),
    [view.plan.review, overrides],
  );
  const stale = useMemo(
    () => new Set(view.plan.review.filter((r) => decisionState(r, overrides.get(r.source)) === "stale").map((r) => r.source)),
    [view.plan.review, overrides],
  );
  const decided = resolved;
  const names = useMemo(() => targets.map((x) => x.name), [targets]);
  const [selected, setSelected] = useState(() => initialSelection(series, new Set(decided.keys())));
  const [focus, setFocus] = useState<string | null>(null);
  const [recent, setRecent] = useState(readRecent);
  // Explicit choices; otherwise the AI suggestion when the config has it.
  const [chosen, setChoice] = useState<Record<string, string>>({});
  const choiceOf = (s: Series): string =>
    chosen[s.id] ?? (s.kind === "name" ? KEEP : names.includes(s.items[0].suggested) ? s.items[0].suggested : "");
  const [busy, setBusy] = useState<string | null>(null);
  const [errors, setErrors] = useState<Map<string, string>>(new Map());
  const [failed, setFailed] = useState<Record<string, string>>({});
  const listRef = useRef<HTMLDivElement>(null);
  const shortcuts = useMemo(() => shortcutTargets(recent, names), [recent, names]);
  const seriesOf = useMemo(() => {
    const m = new Map<string, Series>();
    series.forEach((s) => s.items.forEach((i) => m.set(i.source, s)));
    return m;
  }, [series]);

  const done = view.plan.review.filter((r) => decided.has(r.source)).length;

  useEffect(() => {
    if (!focus) return;
    listRef.current?.querySelector<HTMLElement>(`[data-path="${CSS.escape(focus)}"]`)?.focus();
  }, [focus]);

  const apply = async (s: Series) => {
    const pick = choiceOf(s);
    const sources = s.items.map((i) => i.source).filter((p) => selected.has(p));
    if (!pick || sources.length === 0 || busy) return;
    if (s.kind === "name" && pick !== KEEP) return;
    setBusy(s.id);
    setFailed((f) => ({ ...f, [s.id]: "" }));
    try {
      const results = await plan.setOverrides(sources, toAction(pick));
      const ok = results.filter((r) => r.key).map((r) => r.source_path);
      setSelected((sel) => {
        const next = new Set(sel);
        ok.forEach((p) => next.delete(p));
        return next;
      });
      setErrors((prev) => {
        const next = new Map(prev);
        results.forEach((r) => (r.error ? next.set(r.source_path, r.error) : next.delete(r.source_path)));
        return next;
      });
      const text = failureText(results);
      if (text) setFailed((f) => ({ ...f, [s.id]: text }));
      if (pick !== KEEP && ok.length > 0) {
        const r = pushRecent(recent, pick);
        setRecent(r);
        localStorage.setItem(RECENT_KEY, JSON.stringify(r));
      }
    } catch (e) {
      setFailed((f) => ({ ...f, [s.id]: errorText(e) }));
    } finally {
      setBusy(null);
    }
  };

  const note = (id: string, results: OverrideResult[]) => {
    setFailed((f) => ({ ...f, [id]: failureText(results) ?? "" }));
    setErrors((prev) => {
      const next = new Map(prev);
      results.forEach((r) => (r.error ? next.set(r.source_path, r.error) : next.delete(r.source_path)));
      return next;
    });
  };

  const bestOf = (g: SeriesReviewGroup) =>
    g.items.some((i) => i.source === picked[g.id]) ? picked[g.id] : g.items.some((i) => i.source === g.best) ? g.best : g.items[0].source;

  const confirm = async (g: SeriesReviewGroup) => {
    if (busy || !names.includes(g.target)) return;
    setBusy(g.id);
    setFailed((f) => ({ ...f, [g.id]: "" }));
    try {
      note(g.id, await plan.confirmSeries(g.items.map(memberOf), bestOf(g), g.target));
    } catch (e) {
      setFailed((f) => ({ ...f, [g.id]: errorText(e) }));
    } finally {
      setBusy(null);
    }
  };

  /**
   * The files an event-wide `choice` covers: a target never takes a file
   * whose name cannot be built, nor a member of a held series (that keeps
   * the confirm-the-best-shot flow); "keep in source" covers every file.
   */
  const eventMembers = (ev: EventGroup, choice: string) => ev.items.filter((i) => choice === KEEP || (!i.name_error && !i.series));

  const decideEvent = async (ev: EventGroup) => {
    const choice = eventChoice[ev.id];
    const members = choice ? eventMembers(ev, choice) : [];
    if (!choice || members.length === 0 || busy) return;
    setBusy(ev.id);
    setFailed((f) => ({ ...f, [ev.id]: "" }));
    try {
      const results = await plan.decideEvent(members.map(memberOf), toAction(choice));
      note(ev.id, results);
      const ok = results.filter((r) => r.key).map((r) => r.source_path);
      setSelected((sel) => {
        const next = new Set(sel);
        ok.forEach((p) => next.delete(p));
        return next;
      });
      if (choice !== KEEP && ok.length > 0) {
        const r = pushRecent(recent, choice);
        setRecent(r);
        localStorage.setItem(RECENT_KEY, JSON.stringify(r));
      }
    } catch (e) {
      setFailed((f) => ({ ...f, [ev.id]: errorText(e) }));
    } finally {
      setBusy(null);
    }
  };

  const seriesNode = (s: Series) => (
    <SeriesBlock
      key={s.id}
      series={s}
      first={series[0]?.id === s.id}
      targets={targets}
      selected={selected}
      decided={decided}
      stale={stale}
      errors={errors}
      focus={focus}
      choice={choiceOf(s)}
      busy={busy === s.id}
      failed={failed[s.id] || null}
      onFocus={setFocus}
      onToggle={(p) => setSelected((sel) => toggle(sel, p))}
      onToggleAll={() => setSelected((sel) => toggleGroup(sel, s.items.map((i) => i.source)))}
      onChoice={(v) => setChoice((c) => ({ ...c, [s.id]: v }))}
      onApply={() => apply(s)}
    />
  );

  const cardNode = (g: SeriesReviewGroup) => (
    <SeriesReviewCard
      key={g.id}
      group={g}
      best={bestOf(g)}
      known={names.includes(g.target)}
      decided={decided}
      error={errors}
      busy={busy === g.id}
      failed={failed[g.id] || null}
      onPick={(p) => setPicked((x) => ({ ...x, [g.id]: p }))}
      onConfirm={() => confirm(g)}
    />
  );

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const el = e.target as HTMLElement;
    if (el.tagName === "SELECT" || e.altKey || e.ctrlKey || e.metaKey) return;
    const onCell = !!el.dataset.path;
    if (e.key.startsWith("Arrow") && onCell) {
      e.preventDefault();
      setFocus(nextFocus(series, el.dataset.path!, e.key as FocusKey));
      return;
    }
    const s = focus ? seriesOf.get(focus) : undefined;
    if (!s) return;
    if (/^[0-9]$/.test(e.key)) {
      const n = Number(e.key);
      const pick = n === 0 ? KEEP : shortcuts[n - 1];
      if (pick && (s.kind === "ai" || pick === KEEP)) {
        e.preventDefault();
        setChoice((c) => ({ ...c, [s.id]: pick }));
      }
    } else if (e.key === "Enter" && onCell) {
      e.preventDefault();
      void apply(s);
    }
  };

  const total = view.plan.review.length;
  return (
    <>
      <div className="review-head">
        <p>
          {t("decide_head", { files: t("files", { count: total }), series: t("decide_series", { count: series.length + cards.length }) })}
          {done > 0 && <span className="muted">, {t("decide_done", { n: done })}</span>}
        </p>
        <label className="check group-events">
          <input type="checkbox" checked={byEvent} onChange={(e) => setByEvent(e.target.checked)} />
          <span>{t("decide_group_events")}</span>
        </label>
        <ul className="keys" aria-label={t("decide_keys")}>
          {shortcuts.map((name, i) => (
            <li key={name}>
              <kbd>{i + 1}</kbd> {name}
            </li>
          ))}
          <li>
            <kbd>0</kbd> {t("decide_keep")}
          </li>
          <li className="muted">
            <kbd>←</kbd>
            <kbd>→</kbd> {t("decide_key_file")} <kbd>↑</kbd>
            <kbd>↓</kbd> {t("decide_key_series")} <kbd>{t("key_space")}</kbd> {t("decide_key_check")} <kbd>{t("key_enter")}</kbd>{" "}
            {t("decide_key_apply")}
          </li>
        </ul>
      </div>
      <div className="review-list" ref={listRef} onKeyDown={onKey}>
        {byEvent
          ? events.map((ev) => {
              const shown = eventContents(ev, cards, series);
              const choice = eventChoice[ev.id] ?? "";
              const covered = eventMembers(ev, choice || KEEP);
              const target = choice !== "" && choice !== KEEP;
              return (
                <EventBlock
                  key={ev.id}
                  event={ev}
                  targets={targets}
                  choice={choice}
                  files={shown.files}
                  covered={covered.length}
                  leftName={target ? ev.items.filter((i) => i.name_error && !i.series).length : 0}
                  leftSeries={target ? ev.items.filter((i) => i.series).length : 0}
                  busy={busy === ev.id}
                  failed={failed[ev.id] || null}
                  onChoice={(v) => setEventChoice((c) => ({ ...c, [ev.id]: v }))}
                  onApply={() => decideEvent(ev)}
                >
                  {shown.cards.map(cardNode)}
                  {shown.series.map(seriesNode)}
                </EventBlock>
              );
            })
          : [...cards.map(cardNode), ...series.map(seriesNode)]}
      </div>
    </>
  );
}

function spanText(seconds: number): string {
  if (seconds < 60) return t("duration_s", { n: seconds });
  const m = Math.floor(seconds / 60);
  const s = seconds % 60;
  return s ? t("duration_min_s", { min: m, s }) : t("duration_min", { n: m });
}

interface SeriesProps {
  series: Series;
  first: boolean;
  targets: TargetChoice[];
  selected: ReadonlySet<string>;
  decided: ReadonlyMap<string, OverrideAction>;
  /** Files with a decision made for another version of them. */
  stale: ReadonlySet<string>;
  errors: ReadonlyMap<string, string>;
  focus: string | null;
  choice: string;
  busy: boolean;
  failed: string | null;
  onFocus: (path: string) => void;
  onToggle: (path: string) => void;
  onToggleAll: () => void;
  onChoice: (v: string) => void;
  onApply: () => void;
}

function SeriesBlock(p: SeriesProps) {
  useTranslation();
  const s = p.series;
  const paths = s.items.map((i) => i.source);
  const state = groupState(p.selected, paths);
  const checked = paths.filter((x) => p.selected.has(x)).length;
  const allDone = paths.every((x) => p.decided.has(x));
  const span = seriesSpan(s);
  const head = s.items[0];
  const reasons = [...new Set(s.items.map((i) => i.reason))];
  const metas = [...new Map(s.items.filter((i) => i.ai_meta).map((i) => [i.ai_meta?.description ?? "", i.ai_meta])).values()].slice(0, 2);
  const suggestions = [...new Set(s.items.map((i) => i.suggested))];
  const groupBox = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (groupBox.current) groupBox.current.indeterminate = state === "some";
  }, [state]);
  const isName = s.kind === "name";
  const canApply = !!p.choice && checked > 0 && !p.busy && (!isName || p.choice === KEEP);

  return (
    <section className={`series${isName ? " name-error" : ""}${allDone ? " done" : ""}`} aria-label={t("decide_series_aria", { name: baseName(head.source) })}>
      <div className="series-time">
        {span ? (
          <>
            <span className="series-clock">{span.start.slice(11, 19)}</span>
            <span className="series-date">{formatDate(span.start)}</span>
            {s.items.length > 1 && <span className="series-span">{t("decide_over", { span: spanText(span.seconds) })}</span>}
          </>
        ) : (
          <span className="series-date">{t("decide_time_unknown")}</span>
        )}
      </div>
      <div className="series-main">
        <header className="series-head">
          <label className="check">
            <input ref={groupBox} type="checkbox" checked={state === "all"} onChange={p.onToggleAll} />
            <span>
              {t("files", { count: s.items.length })}
            </span>
          </label>
          <span className="series-source" title={s.source ?? undefined}>
            {s.source ? baseName(s.source) : t("decide_source_unknown")}
          </span>
          {allDone && <span className="series-done">{t("decide_decided")}</span>}
        </header>
        {isName ? (
          <div className="series-why">
            <p className="series-suggest">{t("decide_name_error", { targets: suggestions.join("”, “") })}</p>
            {reasons.map((r) => (
              <blockquote key={r} className="reason">
                {r}
              </blockquote>
            ))}
          </div>
        ) : (
          <div className="series-why">
            <p className="series-suggest">
              {t("decide_ai_suggests")} <strong>{suggestions.join(", ")}</strong>
            </p>
            {reasons.slice(0, 2).map((r) => (
              <blockquote key={r} className="reason">
                {r}
              </blockquote>
            ))}
            {reasons.length > 2 && <p className="muted small">{t("decide_more_reasons", { count: reasons.length - 2 })}</p>}
            {metas.map((m) => (
              <AiMetaView key={m?.description ?? m?.tags?.join(",")} meta={m} />
            ))}
          </div>
        )}
        <ul className="series-strip" role="group" aria-label={t("decide_series_files")}>
          {s.items.map((it, i) => (
            <li key={it.source}>
              <ReviewCell
                item={it}
                checked={p.selected.has(it.source)}
                decided={p.decided.get(it.source)}
                stale={p.stale.has(it.source)}
                error={p.errors.get(it.source)}
                tabbable={p.focus ? p.focus === it.source : p.first && i === 0}
                onFocus={() => p.onFocus(it.source)}
                onToggle={() => p.onToggle(it.source)}
              />
            </li>
          ))}
        </ul>
        <footer className="series-foot">
          {isName ? (
            <>
              <button type="button" className="btn primary" disabled={!canApply} onClick={p.onApply}>
                {p.busy ? t("saving") : checked ? t("decide_keep_n", { n: checked }) : t("trace_keep")}
              </button>
              <span className="muted small">{t("decide_name_hint")}</span>
            </>
          ) : (
            <>
              <select
                className="picker"
                aria-label={t("decide_target_for")}
                value={p.choice}
                onChange={(e) => p.onChoice(e.target.value)}
              >
                <option value="" disabled>
                  {t("decide_choose")}
                </option>
                {p.targets.map((x) => (
                  <option key={x.name} value={x.name}>
                    {x.name}
                  </option>
                ))}
                <option value={KEEP}>{t("trace_keep")}</option>
              </select>
              <button type="button" className="btn primary" disabled={!canApply} onClick={p.onApply}>
                {p.busy ? t("saving") : checked ? t("decide_apply_n", { n: checked }) : t("decide_apply")}
              </button>
              {head.suggested && !p.targets.some((x) => x.name === head.suggested) && (
                <span className="muted small">{t("decide_suggested_missing", { name: head.suggested })}</span>
              )}
            </>
          )}
          {p.failed && <p className="bad-text small series-failed">{p.failed}</p>}
        </footer>
      </div>
    </section>
  );
}

function ReviewCell({
  item,
  checked,
  decided,
  stale,
  error,
  tabbable,
  onFocus,
  onToggle,
}: {
  item: ReviewItem;
  checked: boolean;
  decided: OverrideAction | undefined;
  stale: boolean;
  error: string | undefined;
  tabbable: boolean;
  onFocus: () => void;
  onToggle: () => void;
}) {
  useTranslation();
  const name = baseName(item.source);
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      data-path={item.source}
      tabIndex={tabbable ? 0 : -1}
      className={`rcell${decided ? " decided" : ""}${error ? " failed" : ""}`}
      title={
        error
          ? `${item.source}\n${error}`
          : stale
            ? `${item.source}\n${t("decide_stale_title")}`
            : `${item.source}\n${item.reason}`
      }
      onFocus={onFocus}
      onClick={onToggle}
    >
      <span className="rcell-thumb">
        <Thumb path={item.source} queue={queue} />
        <span className="rcell-box" aria-hidden />
        {decided && <span className="rcell-decided">→ {actionText(decided)}</span>}
        {stale && !decided && <span className="rcell-decided stale">{t("decide_stale")}</span>}
      </span>
      <span className="rcell-name">{name}</span>
    </button>
  );
}

function EventBlock({
  event,
  targets,
  choice,
  files,
  covered,
  leftName,
  leftSeries,
  busy,
  failed,
  onChoice,
  onApply,
  children,
}: {
  event: EventGroup;
  targets: TargetChoice[];
  choice: string;
  /** The files shown in the block (a series card shows all its members). */
  files: number;
  /** How many of the event's files the chosen action covers. */
  covered: number;
  /** Of the event's files, those a target cannot take: a name that cannot be built; a held series. */
  leftName: number;
  leftSeries: number;
  busy: boolean;
  failed: string | null;
  onChoice: (v: string) => void;
  onApply: () => void;
  children: ReactNode;
}) {
  useTranslation();
  const start = event.start ? `${formatDate(event.start)} ${event.start.slice(11, 16)}` : t("decide_event_undated");
  return (
    <section className="event" aria-label={t("decide_event_aria", { start })}>
      <header className="event-head">
        <strong>{start}</strong>
        <span className="muted">{t("files", { count: files })}</span>
        <select className="picker" aria-label={t("decide_event_target")} value={choice} onChange={(e) => onChoice(e.target.value)}>
          <option value="" disabled>
            {t("decide_choose")}
          </option>
          {targets.map((x) => (
            <option key={x.name} value={x.name}>
              {x.name}
            </option>
          ))}
          <option value={KEEP}>{t("trace_keep")}</option>
        </select>
        <button type="button" className="btn primary" disabled={!choice || covered === 0 || busy} onClick={onApply}>
          {busy ? t("saving") : t("decide_event_apply", { n: covered })}
        </button>
        {leftName > 0 && <span className="muted small">{t("decide_event_left_out", { n: leftName })}</span>}
        {leftSeries > 0 && <span className="muted small">{t("decide_event_series_left_out", { n: leftSeries })}</span>}
        {failed && <p className="bad-text small series-failed">{failed}</p>}
      </header>
      {children}
    </section>
  );
}

/// A series the `series` conditions held back: every member side by side,
/// the suggested best marked; picking another best and confirming records
/// the best as such and sends the others to the suggested target.
function SeriesReviewCard({
  group,
  best,
  known,
  decided,
  error,
  busy,
  failed,
  onPick,
  onConfirm,
}: {
  group: SeriesReviewGroup;
  best: string;
  known: boolean;
  decided: ReadonlyMap<string, OverrideAction>;
  error: ReadonlyMap<string, string>;
  busy: boolean;
  failed: string | null;
  onPick: (path: string) => void;
  onConfirm: () => void;
}) {
  useTranslation();
  const head = group.items[0];
  const allDone = group.items.every((i) => decided.has(i.source));
  return (
    <section className={`series series-review${allDone ? " done" : ""}`} aria-label={t("decide_series_card_aria", { name: baseName(group.best) })}>
      <div className="series-time">
        <span className="series-clock">{head.captured ? head.captured.slice(11, 19) : ""}</span>
        <span className="series-date">{head.captured ? formatDate(head.captured) : t("decide_time_unknown")}</span>
      </div>
      <div className="series-main">
        <header className="series-head">
          <strong>{t("files", { count: group.items.length })}</strong>
          {allDone && <span className="series-done">{t("decide_decided")}</span>}
        </header>
        <p className="muted small">{t("decide_series_hint")}</p>
        <ul
          className="series-strip"
          role="radiogroup"
          aria-label={t("decide_series_files")}
          onKeyDown={(e) => {
            // Radio group keys: the arrows move the pick (and focus) round the shots.
            const step = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0;
            if (!step || e.altKey || e.ctrlKey || e.metaKey) return;
            e.preventDefault();
            e.stopPropagation();
            const at = group.items.findIndex((i) => i.source === best);
            const next = group.items[(at + step + group.items.length) % group.items.length];
            onPick(next.source);
            e.currentTarget.querySelectorAll<HTMLElement>('[role="radio"]')[group.items.indexOf(next)]?.focus();
          }}
        >
          {group.items.map((it) => {
            const mine = decided.get(it.source);
            const err = error.get(it.source);
            return (
              <li key={it.source}>
                <button
                  type="button"
                  role="radio"
                  aria-checked={it.source === best}
                  tabIndex={it.source === best ? 0 : -1}
                  className={`rcell${mine ? " decided" : ""}${err ? " failed" : ""}`}
                  title={err ? `${it.source}\n${err}` : `${it.source}\n${it.reason}`}
                  onClick={() => onPick(it.source)}
                >
                  <span className="rcell-thumb">
                    <Thumb path={it.source} queue={queue} />
                    {it.source === best && <span className="rcell-best">{t("decide_series_best")}</span>}
                    {it.source === group.best && <span className="rcell-suggested">{t("decide_series_suggested")}</span>}
                    {mine && <span className="rcell-decided">→ {actionText(mine)}</span>}
                  </span>
                  <span className="rcell-name">{baseName(it.source)}</span>
                </button>
              </li>
            );
          })}
        </ul>
        <footer className="series-foot">
          <button type="button" className="btn primary" disabled={!known || busy || allDone} onClick={onConfirm}>
            {busy ? t("saving") : t("decide_series_confirm", { target: group.target })}
          </button>
          {!known && <span className="muted small">{t("decide_suggested_missing", { name: group.target })}</span>}
          {failed && <p className="bad-text small series-failed">{failed}</p>}
        </footer>
      </div>
    </section>
  );
}
