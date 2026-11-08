import { useEffect, useState } from "react";
import { errorText, examplesApi, type SourceExamples } from "../api";

/// "AI examples": the user's corrections of AI answers, kept per source and shown to the AI in
/// later requests. The examples are not part of the cache key: cached answers stay valid.
export function AiExamples() {
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
    <section className="ai-examples" aria-label="AI examples">
      <h3>AI examples</h3>
      <p className="muted">
        When you change an AI answer, the correction is remembered as text (no images) and shown to the AI in later requests
        for the same source. Examples affect new AI requests; answers already received stay valid.
      </p>
      {groups === null && !error && <p className="muted">Reading examples…</p>}
      {groups?.length === 0 && <p className="muted">No corrections yet.</p>}
      {groups?.map((g) => (
        <div key={g.source_id} className="ai-examples-source">
          <div className="ai-examples-head">
            <strong>{g.source_id}</strong> <span className="muted">— {g.count}</span>{" "}
            <button type="button" className="btn" onClick={() => void act(() => examplesApi.clear(g.source_id))}>
              Clear “{g.source_id}”
            </button>
          </div>
          <ul>
            {g.items.map((it) => (
              <li key={it.id}>
                <span className="muted">{it.file}:</span> {it.description} — AI chose {it.ai_suggested ?? "nothing"},
                correct: {it.user_choice ?? "leave in place"}{" "}
                <button
                  type="button"
                  className="btn"
                  aria-label={`Delete example ${it.file}: ${it.description}`}
                  onClick={() => void act(() => examplesApi.remove(it.id))}
                >
                  Delete
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
      {groups && groups.length > 0 && (
        <div className="form-actions">
          <button type="button" className="btn" onClick={() => void act(() => examplesApi.clear(null))}>
            Clear
          </button>
        </div>
      )}
      {error && <p className="bad-text">{error}</p>}
    </section>
  );
}
