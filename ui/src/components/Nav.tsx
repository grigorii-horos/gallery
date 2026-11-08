import { useTranslation } from "react-i18next";
import type { MessageKey } from "../i18n";

export type ScreenId =
  | "flow"
  | "sources"
  | "targets"
  | "presets"
  | "history"
  | "settings";

export interface ScreenDef {
  id: ScreenId;
  title: MessageKey;
  group: "setup" | "flow" | "log";
}

export const SCREENS: ScreenDef[] = [
  { id: "flow", title: "nav_sort", group: "flow" },
  { id: "sources", title: "nav_sources", group: "setup" },
  { id: "targets", title: "nav_targets", group: "setup" },
  { id: "presets", title: "nav_presets", group: "setup" },
  { id: "history", title: "nav_history", group: "log" },
  { id: "settings", title: "nav_settings", group: "log" },
];

const GROUPS: { id: ScreenDef["group"]; label: MessageKey }[] = [
  { id: "flow", label: "nav_group_flow" },
  { id: "setup", label: "nav_group_setup" },
  { id: "log", label: "nav_group_log" },
];

interface Props {
  current: ScreenId;
  onSelect: (id: ScreenId) => void;
}

/// Left navigation. Sort is the main screen: one flow over the current plan.
export function Nav({ current, onSelect }: Props) {
  const { t } = useTranslation();
  return (
    <nav className="nav" aria-label={t("nav_aria")}>
      <div className="brand">
        <img src="/icon.svg" alt="" width={20} height={20} />
        <span>Gallery</span>
      </div>
      {GROUPS.map((g) => (
        <div key={g.id} className={`nav-group nav-${g.id}`}>
          <div className="nav-label">{t(g.label)}</div>
          <ul>
            {SCREENS.filter((s) => s.group === g.id).map((s) => (
              <li key={s.id}>
                <button
                  type="button"
                  className="nav-item"
                  aria-current={current === s.id ? "page" : undefined}
                  onClick={() => onSelect(s.id)}
                >
                  {t(s.title)}
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
}
