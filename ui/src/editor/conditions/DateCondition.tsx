import { useTranslation } from "react-i18next";
import { Field } from "../fields";
import { NotToggle, OptSelect, OptText, ValueToggles, asObj, withKey, type CondFormProps } from "./shared";

const WEEKDAYS = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/** Short names in the UI language: 2024-01-01 is a Monday. */
function names(lang: string, month: boolean): string[] {
  const f = new Intl.DateTimeFormat(lang, month ? { month: "short", timeZone: "UTC" } : { weekday: "short", timeZone: "UTC" });
  return Array.from({ length: month ? 12 : 7 }, (_, i) => f.format(new Date(Date.UTC(2024, month ? i : 0, month ? 1 : 1 + i))));
}

export function DateCondition({ value, onChange }: CondFormProps) {
  const { t, i18n } = useTranslation();
  const o = asObj(value);
  const months = Array.isArray(o.months) ? (o.months as number[]) : [];
  const days = Array.isArray(o.weekdays) ? (o.weekdays as string[]) : [];
  const monthNames = names(i18n.language, true);
  const dayNames = names(i18n.language, false);
  return (
    <>
      <OptSelect
        o={o}
        k="source"
        label={t("cond_date_source")}
        options={[
          ["file_mtime", t("date_mtime")],
          ["name", t("date_name")],
        ]}
        anyLabel={t("date_capture")}
        onChange={onChange}
      />
      <OptText o={o} k="range" label={t("cond_date_range")} placeholder="2026-08-12..2026-08-18" hint={t("cond_date_hint")} onChange={onChange} />
      <OptText o={o} k="older_than" label={t("cond_date_older")} placeholder="30d" onChange={onChange} />
      <OptText o={o} k="newer_than" label={t("cond_date_newer")} placeholder="1y" onChange={onChange} />
      <Field label={t("cond_date_months")}>
        <ValueToggles label={t("cond_date_months")} options={monthNames.map((n, i): [number, string] => [i + 1, n])} value={months} onChange={(m) => onChange(withKey(o, "months", [...m].sort((a, b) => a - b)))} />
      </Field>
      <Field label={t("cond_date_weekdays")}>
        <ValueToggles
          label={t("cond_date_weekdays")}
          options={WEEKDAYS.map((d, i): [string, string] => [d, dayNames[i]])}
          value={days}
          onChange={(d) => onChange(withKey(o, "weekdays", WEEKDAYS.filter((x) => d.includes(x))))}
        />
      </Field>
      <OptText o={o} k="time" label={t("cond_date_time")} placeholder="22:00..05:00" onChange={onChange} />
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
