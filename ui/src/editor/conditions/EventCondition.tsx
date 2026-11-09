import { useTranslation } from "react-i18next";
import { Field, SourceToggles } from "../fields";
import { NotToggle, OptNumber, OptText, asObj, str, strList, withKey, type CondFormProps } from "./shared";

export function EventCondition({ value, onChange, ctx }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  const away = str(o.away_from).replace(/^@/, "");
  return (
    <>
      <Field label={t("cond_event_sources")}>
        <SourceToggles all={ctx.sources} value={strList(o.sources)} label={t("cond_event_sources")} onChange={(s) => onChange(withKey(o, "sources", s))} />
      </Field>
      <OptText o={o} k="gap" label={t("cond_event_gap")} placeholder="4h" onChange={onChange} />
      <OptNumber o={o} k="km" label={t("cond_event_km")} placeholder="30" onChange={onChange} />
      <OptNumber o={o} k="min_files" label={t("cond_event_min_files")} onChange={onChange} />
      <OptText o={o} k="min_duration" label={t("cond_event_min_duration")} placeholder="1d" onChange={onChange} />
      <OptText o={o} k="max_duration" label={t("cond_event_max_duration")} placeholder="30d" onChange={onChange} />
      <Field label={t("cond_event_away")}>
        <select className="select" aria-label={t("cond_event_away")} value={away} onChange={(e) => onChange(withKey(o, "away_from", e.target.value ? `@${e.target.value}` : null))}>
          <option value="">-</option>
          {[...ctx.places, ...(away && !ctx.places.includes(away) ? [away] : [])].map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
      </Field>
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
