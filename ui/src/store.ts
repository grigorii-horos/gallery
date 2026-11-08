// App-wide data: the chosen config, its summary, the status bar and the built-ins.
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type AppStatus, type ConfigSummary, type PresetSet } from "./api";

const PATH_KEY = "gallery.configPath";

export interface AppData {
  /// null: the default ~/.config/gallery/gallery.toml.
  configPath: string | null;
  status: AppStatus | null;
  summary: ConfigSummary | null;
  /// Why the config could not be loaded at all.
  loadError: string | null;
  builtins: PresetSet | null;
  loading: boolean;
  setConfigPath: (path: string | null) => void;
  reload: () => void;
}

export function useAppData(): AppData {
  const [configPath, setPath] = useState<string | null>(() => localStorage.getItem(PATH_KEY));
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [summary, setSummary] = useState<ConfigSummary | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [builtins, setBuiltins] = useState<PresetSet | null>(null);
  const [loading, setLoading] = useState(true);
  const [generation, setGeneration] = useState(0);

  useEffect(() => {
    api.builtinPresets().then(setBuiltins, () => setBuiltins(null));
  }, []);

  useEffect(() => {
    let live = true;
    // Show the spinner state while the config and tools are re-read.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLoading(true);
    Promise.allSettled([api.loadConfig(configPath), api.appStatus(configPath)]).then(
      ([cfg, st]) => {
        if (!live) return;
        if (cfg.status === "fulfilled") {
          setSummary(cfg.value);
          setLoadError(null);
        } else {
          setSummary(null);
          setLoadError(errorText(cfg.reason));
        }
        setStatus(st.status === "fulfilled" ? st.value : null);
        setLoading(false);
      },
    );
    return () => {
      live = false;
    };
  }, [configPath, generation]);

  const setConfigPath = useCallback((path: string | null) => {
    if (path) localStorage.setItem(PATH_KEY, path);
    else localStorage.removeItem(PATH_KEY);
    setPath(path);
  }, []);

  const reload = useCallback(() => setGeneration((g) => g + 1), []);

  return { configPath, status, summary, loadError, builtins, loading, setConfigPath, reload };
}
