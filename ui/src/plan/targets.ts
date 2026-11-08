import type { ConfigSummary, Stage } from "../api";
import { t as translate, type MessageKey } from "../i18n";
import type { TFunction } from "i18next";
import type { TargetChoice } from "./TracePanel";

const STAGE_NAME: Record<Stage, MessageKey> = {
  before_ai: "stage_masks_before",
  ai: "stage_ai",
  after_ai: "stage_masks_after",
};

/** Every target of the config, in config order, with its stage. */
export function targetChoices(cfg: ConfigSummary | null, t: TFunction = translate): TargetChoice[] {
  return cfg
    ? (["before_ai", "ai", "after_ai"] as Stage[]).flatMap((st) =>
        cfg.targets[st].map((x) => ({ name: x.name, stage: t(STAGE_NAME[st]) })),
      )
    : [];
}
