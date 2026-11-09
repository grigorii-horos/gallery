import type { ConfigSummary } from "../api";
import { t as translate } from "../i18n";
import type { TFunction } from "i18next";
import type { TargetChoice } from "./TracePanel";

/** Every target of the config, in config order; AI targets are marked. */
export function targetChoices(cfg: ConfigSummary | null, t: TFunction = translate): TargetChoice[] {
  return cfg ? cfg.targets.map((x) => ({ name: x.name, stage: x.ai ? t("stage_ai") : "" })) : [];
}
