import { useTranslation } from "react-i18next";
import { Field, Issues } from "../fields";

/** The AI category description of a filter (not a table: plain text). */
export function AiCondition({ value, onChange, error }: { value: string; onChange: (v: string) => void; error?: string | null }) {
  const { t } = useTranslation();
  return (
    <Field label={t("targets_description")}>
      <textarea className="input" aria-label={t("targets_description")} aria-invalid={!!error || undefined} rows={4} value={value} onChange={(e) => onChange(e.target.value)} />
      <Issues items={[]} error={error} />
    </Field>
  );
}
