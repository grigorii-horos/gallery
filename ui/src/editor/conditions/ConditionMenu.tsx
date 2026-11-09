import { useEffect, useRef, useState, type ComponentType } from "react";
import { useTranslation } from "react-i18next";
import type { ConditionKey, FilterDraft } from "../model";
import { AiCondition } from "./AiCondition";
import { DateCondition } from "./DateCondition";
import { DeviceCondition } from "./DeviceCondition";
import { DuplicateCondition } from "./DuplicateCondition";
import { EventCondition } from "./EventCondition";
import { GeoCondition } from "./GeoCondition";
import { NearCondition } from "./NearCondition";
import { PathCondition } from "./PathCondition";
import { PropsCondition } from "./PropsCondition";
import { SeriesCondition } from "./SeriesCondition";
import type { CondContext, CondFormProps } from "./shared";

/** A condition of a filter: a table key, or the AI description. */
export type CondKey = ConditionKey | "ai";

interface Def {
  key: CondKey;
  /** Catalog key of the name. */
  name: `cond_${Exclude<CondKey, "ai">}` | "stage_ai";
  /** The value a new condition starts with. */
  fresh: (ctx: CondContext) => unknown;
  Form?: ComponentType<CondFormProps>;
}

/** The ten conditions, in the order of the menu. */
export const CONDITIONS: Def[] = [
  { key: "date", name: "cond_date", fresh: () => ({}), Form: DateCondition },
  { key: "geo", name: "cond_geo", fresh: (c) => (c.places.length > 0 ? `@${c.places[0]}` : { has_gps: true }), Form: GeoCondition },
  { key: "event", name: "cond_event", fresh: () => ({}), Form: EventCondition },
  { key: "near", name: "cond_near", fresh: () => ({ sources: [], time: "2h" }), Form: NearCondition },
  { key: "device", name: "cond_device", fresh: () => ({}), Form: DeviceCondition },
  { key: "props", name: "cond_props", fresh: () => ({}), Form: PropsCondition },
  { key: "duplicate", name: "cond_duplicate", fresh: () => ({}), Form: DuplicateCondition },
  { key: "series", name: "cond_series", fresh: () => ({}), Form: SeriesCondition },
  { key: "path", name: "cond_path", fresh: () => [] as string[], Form: PathCondition },
  { key: "ai", name: "stage_ai", fresh: () => "" },
];

/** The conditions a filter holds. */
export function activeKeys(f: FilterDraft): CondKey[] {
  return CONDITIONS.filter((c) => (c.key === "ai" ? f.ai != null : c.key in f.conditions)).map((c) => c.key);
}

/** `f` with condition `key` set to `value`. */
export function setCondition(f: FilterDraft, key: CondKey, value: unknown): FilterDraft {
  if (key === "ai") return { ...f, ai: String(value), expand: null };
  return { ...f, conditions: { ...f.conditions, [key]: value } };
}

/** `f` without condition `key`. */
export function dropCondition(f: FilterDraft, key: CondKey): FilterDraft {
  if (key === "ai") return { ...f, ai: null };
  const conditions = { ...f.conditions };
  delete conditions[key];
  return { ...f, conditions };
}

/** The "+ condition" button and its list of the conditions the filter does not have yet. */
export function ConditionMenu({ filter, ctx, onAdd }: { filter: FilterDraft; ctx: CondContext; onAdd: (key: CondKey, value: unknown) => void }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const have = activeKeys(filter);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLUListElement>(null);
  // Opening moves focus to the first choice, so the keyboard can pick one.
  useEffect(() => {
    if (open) list.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  }, [open]);
  const close = () => {
    setOpen(false);
    button.current?.focus();
  };
  return (
    <div
      className="cond-menu"
      onKeyDown={(e) => {
        if (open && e.key === "Escape") {
          e.stopPropagation();
          close();
        }
      }}
    >
      <button ref={button} type="button" className="link small" aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}>
        {t("cond_add")}
      </button>
      {open && (
        <ul ref={list} role="menu" className="cond-menu-list">
          {CONDITIONS.map((c) => (
            <li key={c.key} role="none">
              <button
                type="button"
                role="menuitem"
                className="btn quiet"
                disabled={have.includes(c.key)}
                onClick={() => {
                  close();
                  onAdd(c.key, c.fresh(ctx));
                }}
              >
                {t(c.name)}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** One small form per condition the filter has, each removable. */
export function ConditionForms({
  filter,
  ctx,
  aiError,
  onChange,
}: {
  filter: FilterDraft;
  ctx: CondContext;
  aiError?: string | null;
  onChange: (f: FilterDraft) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      {activeKeys(filter).map((key) => {
        const def = CONDITIONS.find((c) => c.key === key)!;
        const Form = def.Form;
        return (
          <fieldset key={key} className="cond" aria-label={t(def.name)}>
            <legend>{t(def.name)}</legend>
            {Form ? (
              <Form value={filter.conditions[key]} ctx={ctx} onChange={(v) => onChange(setCondition(filter, key, v))} />
            ) : (
              <AiCondition value={filter.ai ?? ""} error={aiError} onChange={(v) => onChange(setCondition(filter, "ai", v))} />
            )}
            <button type="button" className="link small danger" onClick={() => onChange(dropCondition(filter, key))}>
              {t("cond_remove", { name: t(def.name) })}
            </button>
          </fieldset>
        );
      })}
    </>
  );
}
