import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { localeApi, pickConfig } from "./api";
import { applyLanguageSetting } from "./i18n";
import { useAppData } from "./store";
import { Nav, SCREENS, type ScreenId } from "./components/Nav";
import { SyncBanner } from "./components/SyncBanner";
import { useSyncPause, type SyncPauseState } from "./sync/useSyncPause";
import { PendingBanner } from "./components/PendingBanner";
import { usePending, type PendingState } from "./tray/usePending";
import { SettingsScreen } from "./screens/SettingsScreen";
import { StatusBar } from "./components/StatusBar";
import { SourcesScreen } from "./screens/SourcesScreen";
import { TargetsScreen } from "./screens/TargetsScreen";
import { PresetsScreen } from "./screens/PresetsScreen";
import { Placeholder } from "./screens/Placeholder";
import { FlowScreen } from "./flow/FlowScreen";
import { usePlan, type PlanState } from "./plan/usePlan";
import { useRun, type RunState } from "./run/useRun";
import { HistoryScreen } from "./screens/HistoryScreen";
import { useHistory, type HistoryState } from "./history/useHistory";
import { useConfigEditor, type ConfigEditor } from "./editor/useConfigEditor";

export default function App() {
  const { t } = useTranslation();
  // The stored language is the backend's: follow it (the picker changes both).
  useEffect(() => {
    localeApi.get().then(
      (v) => void applyLanguageSetting(v.setting),
      () => {},
    );
  }, []);
  const data = useAppData();
  const plan = usePlan(data.configPath);
  const run = useRun(plan.view?.path ?? null, data.configPath, plan.view?.built_at ?? null);
  const hist = useHistory();
  const sync = useSyncPause();
  // Shared by Sources, Targets and Presets: unsaved edits survive switching screens.
  const editor = useConfigEditor(data.configPath, data.reload);
  const [screen, setScreenState] = useState<ScreenId>(screenFromHash);
  const setScreen = useCallback((id: ScreenId) => {
    history.replaceState(null, "", `#${id}`);
    setScreenState(id);
  }, []);
  // A retry plan History made, waiting on the main screen to be run.
  const [retryPath, setRetryPath] = useState<string | null>(null);
  const openPlan = useCallback(() => setScreen("flow"), [setScreen]);
  const pending = usePending(data.configPath, openPlan);
  // The config was (re)read, e.g. after an edit was saved: the plan follows it.
  const { refresh } = plan;
  useEffect(() => {
    if (data.summary) refresh();
  }, [data.summary, refresh]);
  const title = SCREENS.find((s) => s.id === screen)!.title;
  const shownPath = data.summary?.path ?? data.status?.config_path ?? data.configPath ?? "";

  const openConfig = async () => {
    const picked = await pickConfig(shownPath || null);
    if (picked) data.setConfigPath(picked);
  };

  return (
    <div className="shell">
      <Nav current={screen} onSelect={setScreen} />
      <main className="main" aria-busy={data.loading}>
        <header className="topbar">
          <h1>{t(title)}</h1>
          <div className="topbar-config">
            <span className="path" title={shownPath}>
              <bdi>{shownPath}</bdi>
            </span>
            <button
              type="button"
              className="btn quiet"
              title={editor.dirty ? t("app_reload_dirty_hint") : undefined}
              onClick={() => {
                data.reload();
                if (!editor.dirty) editor.reload();
              }}
            >
              {t("app_reload")}
            </button>
            <button type="button" className="btn" onClick={openConfig}>
              {t("app_open_config")}
            </button>
            {data.configPath && (
              <button
                type="button"
                className="btn quiet"
                title={t("app_default_hint")}
                onClick={() => data.setConfigPath(null)}
              >
                {t("app_default")}
              </button>
            )}
          </div>
        </header>
        {screen !== "flow" && <SyncBanner sync={sync} />}
        {screen !== "flow" && <PendingBanner pending={pending} onPlan={openPlan} />}
        <div className="content">
          <ScreenBody
            screen={screen}
            data={data}
            editor={editor}
            plan={plan}
            run={run}
            history={hist}
            pending={pending}
            sync={sync}
            retryPath={retryPath}
            setRetryPath={setRetryPath}
            onScreen={setScreen}
          />
        </div>
      </main>
      <StatusBar status={data.status} loading={data.loading} />
    </div>
  );
}

/// The screen named in the URL hash (`#targets`), else Sort. The old
/// sorting screens (`#plan`, `#review`, `#run`) are that one screen now.
function screenFromHash(): ScreenId {
  const id = location.hash.slice(1);
  return SCREENS.find((s) => s.id === id)?.id ?? "flow";
}

function ScreenBody({
  screen,
  data,
  editor,
  plan,
  run,
  history,
  pending,
  sync,
  retryPath,
  setRetryPath,
  onScreen,
}: {
  screen: ScreenId;
  data: ReturnType<typeof useAppData>;
  editor: ConfigEditor;
  plan: PlanState;
  run: RunState;
  history: HistoryState;
  pending: PendingState;
  sync: SyncPauseState;
  retryPath: string | null;
  setRetryPath: (path: string | null) => void;
  onScreen: (id: ScreenId) => void;
}) {
  const { t } = useTranslation();
  const busyText = plan.scan
    ? t("busy_scan")
    : run.run
      ? t("busy_run")
      : run.exporting
        ? t("busy_export")
        : history.undo
          ? t("busy_undo")
          : history.retrying
            ? t("busy_retry")
            : null;
  switch (screen) {
    case "sources":
      return <SourcesScreen data={data} editor={editor} />;
    case "targets":
      return <TargetsScreen data={data} editor={editor} />;
    case "presets":
      return <PresetsScreen data={data} editor={editor} />;
    case "flow":
      return (
        <FlowScreen
          data={data}
          plan={plan}
          run={run}
          sync={sync}
          otherBusy={busyText && !run.run && !plan.scan ? busyText : null}
          retryPath={retryPath}
          onRetryDone={() => setRetryPath(null)}
          onOpenHistory={() => onScreen("history")}
          onUndoPilot={(reportPath) => void history.startUndo(reportPath)}
        />
      );
    case "history":
      return (
        <HistoryScreen
          history={history}
          exportDir={run.exportDir}
          busy={busyText}
          onRunPlan={(path) => {
            setRetryPath(path);
            onScreen("flow");
          }}
        />
      );
    case "settings":
      return <SettingsScreen pending={pending} editor={editor} />;
    default:
      return <Placeholder />;
  }
}
