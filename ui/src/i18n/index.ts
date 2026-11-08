// Interface language: i18next with typed flat keys (`locales/en.json` is the
// source of the key type, so a key that does not exist fails `tsc`), the ten
// supported languages, locale detection and `Intl` formatting.
import i18next, { type TFunction } from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./locales/en.json";
import ru from "./locales/ru.json";
import uk from "./locales/uk.json";
import de from "./locales/de.json";
import fr from "./locales/fr.json";
import es from "./locales/es.json";
import ptBR from "./locales/pt-BR.json";
import it from "./locales/it.json";
import pl from "./locales/pl.json";
import ro from "./locales/ro.json";
import zhCN from "./locales/zh-CN.json";

declare module "i18next" {
  interface CustomTypeOptions {
    defaultNS: "translation";
    resources: { translation: typeof en };
    keySeparator: false;
    nsSeparator: false;
  }
}

export interface Language {
  code: string;
  /** The language's name in itself, for the picker. */
  native: string;
  /** English name (what the AI is asked to write in). */
  english: string;
}

/** The eleven languages, English first (the source and the fallback). */
export const LANGUAGES = [
  { code: "en", native: "English", english: "English" },
  { code: "ru", native: "Русский", english: "Russian" },
  { code: "uk", native: "Українська", english: "Ukrainian" },
  { code: "de", native: "Deutsch", english: "German" },
  { code: "fr", native: "Français", english: "French" },
  { code: "es", native: "Español", english: "Spanish" },
  { code: "pt-BR", native: "Português (Brasil)", english: "Brazilian Portuguese" },
  { code: "it", native: "Italiano", english: "Italian" },
  { code: "pl", native: "Polski", english: "Polish" },
  { code: "ro", native: "Română", english: "Romanian" },
  { code: "zh-CN", native: "简体中文", english: "Simplified Chinese" },
] as const satisfies readonly Language[];

export type LangCode = (typeof LANGUAGES)[number]["code"];
export const DEFAULT_LANGUAGE: LangCode = "en";
/** What the Language setting holds when it follows the operating system. */
export const SYSTEM = "system";
/** The stored choice, so the first paint is already in the right language. */
export const SETTING_KEY = "gallery.language";

export const resources = {
  en: { translation: en },
  ru: { translation: ru },
  uk: { translation: uk },
  de: { translation: de },
  fr: { translation: fr },
  es: { translation: es },
  "pt-BR": { translation: ptBR },
  it: { translation: it },
  pl: { translation: pl },
  ro: { translation: ro },
  "zh-CN": { translation: zhCN },
} as const;

/**
 * The supported language of a locale tag (`ru_UA.UTF-8`, `pt-PT`, `zh_TW`):
 * the exact code first, then the language's only regional variant, else null.
 * Same rules as the Rust side.
 */
export function resolveLocale(tag: string): LangCode | null {
  const bare = tag.split(/[.@]/)[0].trim();
  const lang = bare.split(/[_-]/)[0].toLowerCase();
  if (!lang) return null;
  const norm = bare.replace(/_/g, "-").toLowerCase();
  const exact = LANGUAGES.find((l) => l.code.toLowerCase() === norm);
  if (exact) return exact.code;
  return LANGUAGES.find((l) => l.code.split("-")[0] === lang)?.code ?? null;
}

/** The first of `tags` that resolves, else English. */
export function pickLocale(tags: readonly string[]): LangCode {
  for (const t of tags) {
    const code = resolveLocale(t);
    if (code) return code;
  }
  return DEFAULT_LANGUAGE;
}

/** The language of the browser (the OS's), else English. */
export function systemLocale(): LangCode {
  const tags = typeof navigator === "undefined" ? [] : navigator.languages?.length ? navigator.languages : [navigator.language];
  return pickLocale(tags.filter(Boolean));
}

/** A setting (`"system"` or a code) as the language it means. */
export function localeFor(setting: string | null | undefined): LangCode {
  if (!setting || setting === SYSTEM) return systemLocale();
  return resolveLocale(setting) ?? DEFAULT_LANGUAGE;
}

/** `"system"` or a supported code; anything else is "system". */
export function normalizeSetting(value: string | null | undefined): string {
  return LANGUAGES.find((l) => l.code === value)?.code ?? SYSTEM;
}

function storedSetting(): string {
  try {
    return normalizeSetting(localStorage.getItem(SETTING_KEY));
  } catch {
    return SYSTEM;
  }
}

void i18next.use(initReactI18next).init({
  resources,
  lng: localeFor(storedSetting()),
  fallbackLng: DEFAULT_LANGUAGE,
  keySeparator: false,
  nsSeparator: false,
  returnNull: false,
  interpolation: { escapeValue: false },
  react: { useSuspense: false },
});

export const i18n = i18next;

/** The typed translate function of the language in force (for code outside React). */
export const t: TFunction = i18next.t.bind(i18next);

/** The language in force. */
export function currentLanguage(): LangCode {
  return resolveLocale(i18next.language) ?? DEFAULT_LANGUAGE;
}

/** Switches the UI to `setting` (`"system"` or a code) and remembers it. */
export async function applyLanguageSetting(setting: string): Promise<LangCode> {
  const norm = normalizeSetting(setting);
  try {
    if (norm === SYSTEM) localStorage.removeItem(SETTING_KEY);
    else localStorage.setItem(SETTING_KEY, norm);
  } catch {
    // Storage is only a cache for the first paint.
  }
  const code = localeFor(norm);
  await i18next.changeLanguage(code);
  if (typeof document !== "undefined") document.documentElement.lang = code;
  return code;
}

/** The stored setting (what the picker shows before the backend answers). */
export function languageSetting(): string {
  return storedSetting();
}

if (typeof document !== "undefined") document.documentElement.lang = currentLanguage();

/** A key of the catalog (English is the source of the type). */
export type MessageKey = keyof typeof en;
