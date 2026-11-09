import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Field } from "../fields";
import { NotToggle, OptNumber, ValueToggles, asObj, oneOrList, strList, type CondFormProps, type Raw } from "./shared";

type Mode = "places" | "circle" | "gps";

/** `"@home"`, `"!@home"` or a list of those: the place names and whether they are negated. */
function placeNames(v: unknown): { names: string[]; negated: boolean } {
  const items = strList(v);
  return { names: items.map((s) => s.replace(/^!?@/, "")), negated: items.length > 0 && items.every((s) => s.startsWith("!")) };
}

/** The mode a value shows; `null` for a table with no circle or GPS key (a circle just cleared). */
const modeOf = (v: unknown): Mode | null => {
  const o = asObj(v);
  if (typeof v === "string" || Array.isArray(v)) return "places";
  if ("lat" in o || "lon" in o || "km" in o) return "circle";
  return "has_gps" in o ? "gps" : null;
};

export function GeoCondition({ value, onChange, ctx }: CondFormProps) {
  const { t } = useTranslation();
  // A circle with every number cleared has no keys left to tell it from the
  // GPS form: it stays in the mode it was in.
  const [kept, setKept] = useState<Mode>(modeOf(value) ?? "gps");
  const shown = modeOf(value);
  if (shown && shown !== kept) setKept(shown);
  const mode = shown ?? kept;
  const o = asObj(value);
  const choose = (m: Mode) => {
    if (m === mode) return;
    setKept(m);
    if (m === "places") onChange(ctx.places.length > 0 ? `@${ctx.places[0]}` : []);
    else if (m === "circle") onChange({ lat: 0, lon: 0, km: 1 });
    else onChange({ has_gps: true });
  };
  return (
    <>
      <Field label={t("cond_geo_mode")}>
        <select className="select" aria-label={t("cond_geo_mode")} value={mode} onChange={(e) => choose(e.target.value as Mode)}>
          <option value="places">{t("geo_mode_places")}</option>
          <option value="circle">{t("geo_mode_circle")}</option>
          <option value="gps">{t("geo_mode_gps")}</option>
        </select>
      </Field>
      {mode === "places" && <PlacesMode value={value} onChange={onChange} places={ctx.places} />}
      {mode === "circle" && <Circle o={o} onChange={onChange} />}
      {mode === "gps" && <Gps o={o} onChange={onChange} />}
    </>
  );
}

function PlacesMode({ value, onChange, places }: { value: unknown; onChange: (v: unknown) => void; places: string[] }) {
  const { t } = useTranslation();
  const { names, negated } = placeNames(value);
  const all = [...places, ...names.filter((n) => !places.includes(n))];
  const write = (ns: string[], neg: boolean) => onChange(oneOrList(ns.map((n) => `${neg ? "!" : ""}@${n}`)));
  return (
    <>
      <Field label={t("cond_geo_places")} hint={places.length === 0 ? t("cond_geo_no_places") : undefined}>
        <ValueToggles label={t("cond_geo_places")} options={all.map((n): [string, string] => [n, n])} value={names} onChange={(ns) => write(ns, negated)} />
      </Field>
      {/* Several negated places would mean "outside any one of them": only one can be inverted. */}
      <label className="cond-not" title={names.length > 1 ? t("cond_geo_not_single") : undefined}>
        <input type="checkbox" checked={negated} disabled={names.length !== 1} onChange={(e) => write(names, e.target.checked)} /> {t("cond_not")}
      </label>
    </>
  );
}

function Circle({ o, onChange }: { o: Raw; onChange: (o: Raw) => void }) {
  const { t } = useTranslation();
  return (
    <>
      <OptNumber o={o} k="lat" label={t("place_lat")} min={null} onChange={onChange} />
      <OptNumber o={o} k="lon" label={t("place_lon")} min={null} onChange={onChange} />
      <OptNumber o={o} k="km" label={t("place_km")} onChange={onChange} />
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}

function Gps({ o, onChange }: { o: Raw; onChange: (o: Raw) => void }) {
  const { t } = useTranslation();
  return (
    <>
      <Field label={t("geo_mode_gps")}>
        <select className="select" aria-label={t("geo_mode_gps")} value={o.has_gps === false ? "false" : "true"} onChange={(e) => onChange({ ...o, has_gps: e.target.value === "true" })}>
          <option value="true">{t("geo_has_gps")}</option>
          <option value="false">{t("geo_no_gps")}</option>
        </select>
      </Field>
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
