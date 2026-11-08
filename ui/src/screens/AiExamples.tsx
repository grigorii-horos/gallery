import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { errorText, examplesApi, type SourceExamples } from "../api";

/// "AI examples": the user's corrections of AI answers, kept per source and shown to the AI in
/// later requests. The examples are not part of the cache key: cached answers stay valid.
export function AiExamples() {
  const { t } = useTranslation();
  const [groups, setGroups] = useState<SourceExamples[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Bumped after every change so the list is read again.
  const [version, setVersion] = useState(0);

  useEffect(() => {
    let live = true;
    examplesApi
      .list()
      .then((g) => {
        if (!live) return;
        setGroups(g);
        setError(null);
      })
      .catch((e) => live && setError(errorText(e)));
    return () => {
      live = false;
    };
  }, [version]);

  const act = async (run: () => Promise<unknown>) => {
    try {
      await run();
    } catch (e) {
      setError(errorText(e));
      return;
    }
    setVersion((v) => v + 1);
  };

  return (
    <section className="ai-examples" aria-label={t("examples_title")}>
      <h3>{t("examples_title")}</h3>
      <p className="muted">
        {t("examples_text")}
      </p>
      {groups === null && !error && <p className="muted">{t("examples_reading")}</p>}
      {groups?.length === 0 && <p className="muted">{t("examples_none")}</p>}
      {groups?.map((g) => (
        <div key={g.source_id} className="ai-examples-source">
          <div className="ai-examples-head">
            <strong>{g.source_id}</strong> <span className="muted">— {g.count}</span>{" "}
            <button type="button" className="btn" onClick={() => void act(() => examplesApi.clear(g.source_id))}>
              {t("examples_clear_source", { id: g.source_id })}
            </button>
          </div>
          <ul>
            {g.items.map((it) => (
              <li key={it.id}>
                <span className="muted">{it.file}:</span>{" "}
                {t("examples_line", {
                  description: it.description,
                  chose: it.ai_suggested ?? t("examples_nothing"),
                  right: it.user_choice ?? t("examples_leave"),
                })}{" "}
                <button
                  type="button"
                  className="btn"
                  aria-label={t("examples_delete_aria", { file: it.file, description: it.description })}
                  onClick={() => void act(() => examplesApi.remove(it.id))}
                >
                  {t("delete")}
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
      {groups && groups.length > 0 && (
        <div className="form-actions">
          <button type="button" className="btn" onClick={() => void act(() => examplesApi.clear(null))}>
            {t("examples_clear")}
          </button>
        </div>
      )}
      {error && <p className="bad-text">{error}</p>}
    </section>
  );
}
