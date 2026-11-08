import { useEffect, useState } from "react";
import { errorText, trayApi, type TraySettings } from "../api";
import type { PendingState } from "../tray/usePending";
import { AiExamples } from "./AiExamples";

const MIN_INTERVAL = 5;
const MAX_INTERVAL = 24 * 60;

/// Tray and notifications. Gallery only counts and tells; it never applies by itself.
export function SettingsScreen({ pending }: { pending: PendingState }) {
  const [settings, setSettings] = useState<TraySettings | null>(null);
  const [autostart, setAutostart] = useState(false);
  const [saved, setSaved] = useState<{ settings: TraySettings; autostart: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    trayApi.settings().then(
      (v) => {
        setSettings(v.settings);
        setAutostart(v.autostart);
        setSaved(v);
      },
      (e) => setError(errorText(e)),
    );
  }, []);

  if (!settings) return <p className="muted">{error ?? "Reading settings…"}</p>;

  const dirty =
    !saved ||
    saved.autostart !== autostart ||
    saved.settings.check_interval_min !== settings.check_interval_min ||
    saved.settings.notify_threshold !== settings.notify_threshold ||
    saved.settings.hide_to_tray !== settings.hide_to_tray;
  const set = (patch: Partial<TraySettings>) => setSettings({ ...settings, ...patch });
  // An empty field is 0 while typing; the core brings it into bounds on save.
  const num = (v: string) => (Number.isFinite(parseInt(v, 10)) ? parseInt(v, 10) : 0);

  const save = async () => {
    setSaving(true);
    setError(null);
    try {
      const v = await trayApi.setSettings(settings, autostart);
      setSettings(v.settings);
      setAutostart(v.autostart);
      setSaved(v);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  const s = pending.snapshot;
  return (
    <section className="settings">
      <p className="muted">
        Gallery quietly counts new files and tells you about them. It never applies anything by itself: only you run a plan.
      </p>
      <div className="field">
        <label className="field-label" htmlFor="interval">
          Check
        </label>
        <div className="field-body">
          <span>
            every{" "}
            <input
              id="interval"
              type="number"
              min={MIN_INTERVAL}
              max={MAX_INTERVAL}
              value={settings.check_interval_min}
              onChange={(e) => set({ check_interval_min: num(e.target.value) })}
            />{" "}
            min
          </span>
          <p className="field-hint">From {MIN_INTERVAL} minutes to a day; one more check runs at startup.</p>
        </div>
      </div>
      <div className="field">
        <label className="field-label" htmlFor="threshold">
          Notify
        </label>
        <div className="field-body">
          <span>
            when there are at least{" "}
            <input
              id="threshold"
              type="number"
              min={1}
              value={settings.notify_threshold}
              onChange={(e) => set({ notify_threshold: num(e.target.value) })}
            />
          </span>
          <p className="field-hint">At most once an hour.</p>
        </div>
      </div>
      <div className="field">
        <span className="field-label">Window</span>
        <div className="field-body">
          <label>
            <input
              type="checkbox"
              checked={settings.hide_to_tray}
              onChange={(e) => set({ hide_to_tray: e.target.checked })}
            />{" "}
            Closing the window hides it to the tray
          </label>
        </div>
      </div>
      <div className="field">
        <span className="field-label">Startup</span>
        <div className="field-body">
          <label>
            <input type="checkbox" checked={autostart} onChange={(e) => setAutostart(e.target.checked)} />{" "}
            Start at login (minimized to the tray)
          </label>
          <p className="field-hint">Writes ~/.config/autostart/gallery.desktop only when this is checked.</p>
        </div>
      </div>
      <div className="form-actions">
        <button type="button" className="btn primary" disabled={!dirty || saving} onClick={() => void save()}>
          Save
        </button>{" "}
        <button type="button" className="btn" disabled={pending.checking} onClick={() => void pending.checkNow()}>
          Check now
        </button>
      </div>
      {error && <p className="bad-text">{error}</p>}
      {pending.error && <p className="bad-text">{pending.error}</p>}
      {s && (
        <p className="muted" role="status">
          {s.error
            ? `Last check failed: ${s.error}`
            : s.total === 0
              ? "Last check: all sorted."
              : `Last check: ${s.total} to sort (ready to run ${s.to_process}, waiting for AI ${s.need_ai}).`}
        </p>
      )}
      <AiExamples />
    </section>
  );
}
