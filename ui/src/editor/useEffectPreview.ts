// The effect of the pending edits: asked for 400 ms after the last edit; a
// newer request makes the older answer stale (the backend cancels it too).
import { useEffect, useState } from "react";
import type { EditOp } from "./model";
import { effectApi, type EffectDiff } from "./effect";

export const EFFECT_PAUSE_MS = 400;

export interface EffectState {
  diff: EffectDiff | null;
  loading: boolean;
  error: string | null;
}

interface Answer {
  diff: EffectDiff | null;
  error: string | null;
  /** The operations the answer is for. */
  forOps: EditOp[];
}

export function useEffectPreview(configPath: string | null, ops: EditOp[], active: boolean): EffectState {
  const [answer, setAnswer] = useState<Answer>({ diff: null, error: null, forOps: [] });
  useEffect(() => {
    if (!active) return;
    let stale = false;
    const timer = setTimeout(() => {
      effectApi.preview(configPath, ops).then(
        (diff) => {
          if (stale || diff === null) return;
          setAnswer({ diff, error: null, forOps: ops });
        },
        (e) => {
          if (stale) return;
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
  return { diff: answer.diff, error: answer.error, loading: active && answer.forOps !== ops };
}
