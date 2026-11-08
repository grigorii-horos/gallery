export type ScreenId =
  | "flow"
  | "sources"
  | "targets"
  | "presets"
  | "history"
  | "settings";

export interface ScreenDef {
  id: ScreenId;
  title: string;
  group: "setup" | "flow" | "log";
}

export const SCREENS: ScreenDef[] = [
  { id: "flow", title: "Sort", group: "flow" },
  { id: "sources", title: "Sources", group: "setup" },
  { id: "targets", title: "Targets", group: "setup" },
  { id: "presets", title: "Presets", group: "setup" },
  { id: "history", title: "History", group: "log" },
  { id: "settings", title: "Settings", group: "log" },
];

const GROUPS: { id: ScreenDef["group"]; label: string }[] = [
  { id: "flow", label: "Sorting" },
  { id: "setup", label: "Setup" },
  { id: "log", label: "Log" },
];

interface Props {
  current: ScreenId;
  onSelect: (id: ScreenId) => void;
}

/// Left navigation. Sort is the main screen: one flow over the current plan.
export function Nav({ current, onSelect }: Props) {
  return (
    <nav className="nav" aria-label="Sections">
      <div className="brand">
        <img src="/icon.svg" alt="" width={20} height={20} />
        <span>Gallery</span>
      </div>
      {GROUPS.map((g) => (
        <div key={g.id} className={`nav-group nav-${g.id}`}>
          <div className="nav-label">{g.label}</div>
          <ul>
            {SCREENS.filter((s) => s.group === g.id).map((s) => (
              <li key={s.id}>
                <button
                  type="button"
                  className="nav-item"
                  aria-current={current === s.id ? "page" : undefined}
                  onClick={() => onSelect(s.id)}
                >
                  {s.title}
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
    </nav>
  );
}
