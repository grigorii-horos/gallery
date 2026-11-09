import { useTranslation } from "react-i18next";
import { Field } from "../fields";
import { NotToggle, ValueToggles, asObj, strList, type CondFormProps } from "./shared";

const SCOPES = ["sources", "outputs"];

export function DuplicateCondition({ value, onChange }: CondFormProps) {
  const { t } = useTranslation();
  const o = asObj(value);
  // No `of` key: the core's default, other files of the scan.
  const of = "of" in o ? strList(o.of) : ["sources"];
  return (
    <>
      <Field label={t("cond_dup_of")}>
        <ValueToggles
          label={t("cond_dup_of")}
          options={[
            ["sources", t("dup_sources")],
            ["outputs", t("dup_outputs")],
          ]}
          value={of}
          // Kept even when empty: dropping the key would bring the default back.
          onChange={(l) => onChange({ ...o, of: SCOPES.filter((s) => l.includes(s)) })}
        />
      </Field>
      <NotToggle o={o} onChange={onChange} />
    </>
  );
}
