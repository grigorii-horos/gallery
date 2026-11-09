import { useTranslation } from "react-i18next";
import { NotToggle, OptSelect, OptText, asObj, withKey, type CondFormProps } from "./shared";

export function SeriesCondition({ value, onChange }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  return (
    <>
      <OptText o={o} k="gap" label={t("cond_series_gap")} placeholder="5s" hint={t("cond_series_hint")} onChange={onChange} />
      <label className="cond-not">
        <input type="checkbox" checked={o.similar !== false} onChange={(e) => onChange(e.target.checked ? withKey(o, "similar", null) : { ...o, similar: false })} /> {t("cond_series_similar")}
      </label>
      <OptSelect
        o={o}
        k="by"
        label={t("cond_series_by")}
        options={[
          ["size", t("by_size")],
          ["resolution", t("by_resolution")],
        ]}
        anyLabel={t("by_sharpness")}
        onChange={onChange}
      />
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
