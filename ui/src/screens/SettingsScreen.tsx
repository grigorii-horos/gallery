import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { errorText, localeApi, trayApi, type TraySettings } from "../api";
import { LANGUAGES, SYSTEM, applyLanguageSetting, languageSetting, normalizeSetting, systemLocale } from "../i18n";
import type { PendingState } from "../tray/usePending";
import { PlacesPanel } from "../editor/PlacesPanel";
import type { ConfigEditor } from "../editor/useConfigEditor";
import { AiExamples } from "./AiExamples";

const MIN_INTERVAL = 5;
const MAX_INTERVAL = 24 * 60;

/// Tray and notifications. Gallery only counts and tells; it never applies by itself.
export function SettingsScreen({ pending, editor }: { pending: PendingState; editor?: ConfigEditor }) {
  const { t } = useTranslation();
  const [language, setLanguage] = useState(languageSetting);
  const [languageError, setLanguageError] = useState<string | null>(null);
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

  if (!settings) return <p className="muted">{error ?? t("settings_reading")}</p>;

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

  // Applied at once, with no Save: the UI, the backend's messages, the tray and notifications.
  const chooseLanguage = async (value: string) => {
    const next = normalizeSetting(value);
    setLanguage(next);
    setLanguageError(null);
    await applyLanguageSetting(next);
    try {
      await localeApi.set(next);
    } catch (e) {
      setLanguageError(errorText(e));
    }
  };

  const s = pending.snapshot;
  const systemName = LANGUAGES.find((l) => l.code === systemLocale())?.native ?? "";
  return (
    <section className="settings">
      <p className="muted">{t("settings_intro")}</p>
      <div className="field">
        <label className="field-label" htmlFor="language">
          {t("settings_language")}
        </label>
        <div className="field-body">
          <select id="language" className="select" value={language} onChange={(e) => void chooseLanguage(e.target.value)}>
            <option value={SYSTEM}>{t("settings_language_system", { language: systemName })}</option>
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code} lang={l.code}>
                {l.native}
              </option>
            ))}
          </select>
          <p className="field-hint">{t("settings_language_hint")}</p>
          {languageError && <p className="bad-text">{languageError}</p>}
        </div>
      </div>
      <div className="field">
        <label className="field-label" htmlFor="interval">
          {t("settings_check")}
        </label>
        <div className="field-body">
          <span>
            {t("settings_every")}{" "}
            <input
              id="interval"
              type="number"
              min={MIN_INTERVAL}
              max={MAX_INTERVAL}
              value={settings.check_interval_min}
              onChange={(e) => set({ check_interval_min: num(e.target.value) })}
            />{" "}
            {t("settings_min")}
          </span>
          <p className="field-hint">{t("settings_interval_hint", { min: MIN_INTERVAL })}</p>
        </div>
      </div>
      <div className="field">
        <label className="field-label" htmlFor="threshold">
          {t("settings_notify")}
        </label>
        <div className="field-body">
          <span>
            {t("settings_when_at_least")}{" "}
            <input
              id="threshold"
              type="number"
              min={1}
              value={settings.notify_threshold}
              onChange={(e) => set({ notify_threshold: num(e.target.value) })}
            />
          </span>
          <p className="field-hint">{t("settings_once_hour")}</p>
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t("settings_window")}</span>
        <div className="field-body">
          <label>
            <input
              type="checkbox"
              checked={settings.hide_to_tray}
              onChange={(e) => set({ hide_to_tray: e.target.checked })}
            />{" "}
            {t("settings_hide_tray")}
          </label>
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t("settings_startup")}</span>
        <div className="field-body">
          <label>
            <input type="checkbox" checked={autostart} onChange={(e) => setAutostart(e.target.checked)} />{" "}
            {t("settings_autostart")}
          </label>
          <p className="field-hint">{t("settings_autostart_hint")}</p>
        </div>
      </div>
      <div className="form-actions">
        <button type="button" className="btn primary" disabled={!dirty || saving} onClick={() => void save()}>
          {t("save")}
        </button>{" "}
        <button type="button" className="btn" disabled={pending.checking} onClick={() => void pending.checkNow()}>
          {t("settings_check_now")}
        </button>
      </div>
      {error && <p className="bad-text">{error}</p>}
      {pending.error && <p className="bad-text">{pending.error}</p>}
      {s && (
        <p className="muted" role="status">
          {s.error
            ? t("settings_last_failed", { error: s.error })
            : s.total === 0
              ? t("settings_last_sorted")
              : t("settings_last", { total: s.total, ready: s.to_process, ai: s.need_ai })}
        </p>
      )}
      {editor && <PlacesPanel editor={editor} />}
      <AiExamples />
    </section>
  );
}
