import type { ConfigSummary, Stage } from "../api";
import type { TargetChoice } from "./TracePanel";

const STAGE_NAME: Record<Stage, string> = {
  before_ai: "masks before AI",
  ai: "AI",
  after_ai: "masks after AI",
};

/** Every target of the config, in config order, with its stage. */
export function targetChoices(cfg: ConfigSummary | null): TargetChoice[] {
  return cfg
    ? (["before_ai", "ai", "after_ai"] as Stage[]).flatMap((st) =>
        cfg.targets[st].map((t) => ({ name: t.name, stage: STAGE_NAME[st] })),
      )
    : [];
}
