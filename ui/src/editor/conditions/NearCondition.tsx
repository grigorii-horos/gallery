import { useTranslation } from "react-i18next";
import { Field, SourceToggles } from "../fields";
import { NotToggle, OptNumber, OptText, asObj, strList, withKey, type CondFormProps } from "./shared";

export function NearCondition({ value, onChange, ctx }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  return (
    <>
      <Field label={t("cond_near_sources")}>
        {/* `sources` stays in the table even when empty: the core wants the key. */}
        <SourceToggles all={ctx.sources} value={strList(o.sources)} label={t("cond_near_sources")} onChange={(s) => onChange({ ...o, sources: s })} />
      </Field>
      <OptText o={o} k="time" label={t("cond_near_time")} placeholder="2h" onChange={onChange} />
      <OptNumber o={o} k="km" label={t("cond_near_km")} placeholder="1" onChange={onChange} />
      <OptNumber o={o} k="min" label={t("cond_near_min")} placeholder="1" onChange={onChange} />
      <label className="cond-not">
        <input type="checkbox" checked={o.require_gps === true} onChange={(e) => onChange(withKey(o, "require_gps", e.target.checked))} /> {t("cond_near_gps")}
      </label>
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
