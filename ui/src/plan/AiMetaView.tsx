import { useTranslation } from "react-i18next";
import type { AiMeta } from "../api";

/// The AI's description and keywords (chips); nothing when it gave none.
export function AiMetaView({ meta }: { meta?: AiMeta | null }) {
  const { t } = useTranslation();
  if (!meta) return null;
  const tags = meta.tags ?? [];
  if (!meta.description && tags.length === 0) return null;
  return (
    <>
      {meta.description && <p className="ai-desc">{meta.description}</p>}
      {tags.length > 0 && (
        <ul className="ai-tags" aria-label={t("ai_tags_label")}>
          {tags.map((tag) => (
            <li key={tag}>{tag}</li>
          ))}
        </ul>
      )}
    </>
  );
}
