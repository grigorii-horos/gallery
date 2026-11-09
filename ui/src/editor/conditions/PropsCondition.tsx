import { useTranslation } from "react-i18next";
import { Field } from "../fields";
import { NotToggle, OptList, OptSelect, OptText, asObj, type CondFormProps, type Raw } from "./shared";

const RANGES: [`${"size" | "width" | "height" | "megapixels" | "duration" | "fps" | "sharpness"}`, string][] = [
  ["size", "1MB..20MB"],
  ["width", ">=3000"],
  ["height", "..1080"],
  ["megapixels", ">=12"],
  ["duration", "..10s"],
  ["fps", ">=50"],
  ["sharpness", "<40"],
];

export function PropsCondition({ value, onChange }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  const audio = o.has_audio === true ? "true" : o.has_audio === false ? "false" : "";
  return (
    <>
      {RANGES.map(([k, ph], i) => (
        <OptText key={k} o={o} k={k} label={t(`cond_props_${k}`)} placeholder={ph} hint={i === 0 ? t("cond_props_hint") : undefined} onChange={onChange} />
      ))}
      <OptSelect
        o={o}
        k="orientation"
        label={t("cond_props_orientation")}
        options={[
          ["portrait", t("orient_portrait")],
          ["landscape", t("orient_landscape")],
          ["square", t("orient_square")],
        ]}
        anyLabel={t("props_any")}
        onChange={onChange}
      />
      <OptText o={o} k="aspect" label={t("cond_props_aspect")} placeholder="16:9" onChange={onChange} />
      <OptList o={o} k="video_codec" label={t("cond_props_video_codec")} placeholder="hevc, h264" onChange={onChange} />
      <OptList o={o} k="audio_codec" label={t("cond_props_audio_codec")} placeholder="aac" onChange={onChange} />
      <OptList o={o} k="container" label={t("cond_props_container")} placeholder="mp4" onChange={onChange} />
      <Field label={t("cond_props_has_audio")}>
        <select
          className="select"
          aria-label={t("cond_props_has_audio")}
          value={audio}
          onChange={(e) => {
            const next: Raw = { ...o, has_audio: e.target.value === "true" };
            if (!e.target.value) delete next.has_audio;
            onChange(next);
          }}
        >
          <option value="">{t("props_any")}</option>
          <option value="true">{t("props_yes")}</option>
          <option value="false">{t("props_no")}</option>
        </select>
      </Field>
      <OptList o={o} k="has_tag" label={t("cond_props_has_tag")} placeholder="GPSLatitude" onChange={onChange} />
      <OptList o={o} k="missing_tag" label={t("cond_props_missing_tag")} placeholder="DateTimeOriginal" onChange={onChange} />
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
