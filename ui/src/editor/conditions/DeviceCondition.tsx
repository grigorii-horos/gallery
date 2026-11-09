import { useTranslation } from "react-i18next";
import { Field, MaskChips } from "../fields";
import { NotToggle, asObj, oneOrList, strList, withKey, type CondFormProps } from "./shared";

export function DeviceCondition({ value, onChange, ctx }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  return (
    <>
      {(["make", "model", "software"] as const).map((k) => (
        <Field key={k} label={t(`cond_device_${k}`)} hint={k === "make" ? t("cond_device_hint") : undefined}>
          <MaskChips label={t(`cond_device_${k}`)} value={strList(o[k])} suggestions={ctx.suggestions} onChange={(l) => onChange(withKey(o, k, l.length ? oneOrList(l) : null))} />
        </Field>
      ))}
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
