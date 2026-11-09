// The effect of the pending edits: asked for 400 ms after the last edit; a
// newer request makes the older answer stale (the backend cancels it too).
import { useEffect, useState } from "react";
import type { EditOp } from "./model";
import { effectApi, type EffectDiff } from "./effect";

export const EFFECT_PAUSE_MS = 400;

/** Progress of the slow pass (content hashes, series, sharpness) of a request in flight. */
export interface Counting {
  done: number;
  total: number;
}

export interface EffectState {
  diff: EffectDiff | null;
  loading: boolean;
  error: string | null;
  /** Set while the backend counts duplicates and sharpness: the panel shows it instead of looking frozen. */
  counting?: Counting | null;
}

/** `effect-progress` payload (the backend's scan progress shape). */
interface EffectProgress {
  stage: string;
  done: number;
  total: number;
}

interface Answer {
  diff: EffectDiff | null;
  error: string | null;
  /** The operations the answer is for. */
  forOps: EditOp[];
}

export function useEffectPreview(configPath: string | null, ops: EditOp[], active: boolean): EffectState {
  const [answer, setAnswer] = useState<Answer>({ diff: null, error: null, forOps: [] });
  const [counting, setCounting] = useState<Counting | null>(null);
  // The backend reports only the slow group passes of a preview as "plan"
  // events (the scan and EXIF stages are quick or kept between requests).
  useEffect(() => {
    if (!active) return;
    let off: (() => void) | null = null;
    let gone = false;
    import("@tauri-apps/api/event")
      .then(({ listen }) =>
        listen<EffectProgress>("effect-progress", (e) => {
          const p = e.payload;
          if (p.stage === "plan" && p.total > 0) setCounting({ done: p.done, total: p.total });
        }),
      )
      .then((un) => (gone ? un() : (off = un)))
      .catch(() => {});
    return () => {
      gone = true;
      off?.();
    };
  }, [active]);
  useEffect(() => {
    if (!active) return;
    let stale = false;
    const timer = setTimeout(() => {
      setCounting(null);
      effectApi.preview(configPath, ops).then(
        (diff) => {
          if (stale || diff === null) return;
          setCounting(null);
          setAnswer({ diff, error: null, forOps: ops });
        },
        (e) => {
          if (stale) return;
          setCounting(null);
          const error = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
          setAnswer((a) => ({ ...a, error, forOps: ops }));
        },
      );
    }, EFFECT_PAUSE_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [configPath, ops, active]);
  const loading = active && answer.forOps !== ops;
  return { diff: answer.diff, error: answer.error, loading, counting: loading ? counting : null };
}
