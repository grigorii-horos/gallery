import { useTranslation } from "react-i18next";
import { Field, MaskChips } from "../fields";
import { strList, oneOrList, type CondFormProps } from "./shared";

/** A glob (or several, any one) on the path relative to the source folder. */
export function PathCondition({ value, onChange, ctx }: CondFormProps) {
  const { t } = useTranslation();
  return (
    <Field label={t("cond_path_masks")} hint={t("cond_path_hint")}>
      <MaskChips label={t("cond_path_masks")} value={strList(value)} suggestions={ctx.suggestions} onChange={(l) => onChange(oneOrList(l))} />
    </Field>
  );
}
