import { describe, expect, it } from "vitest";
import { LANGUAGES, applyLanguageSetting, i18n, localeFor, normalizeSetting, pickLocale, resolveLocale, resources, t } from "./index";
import { bytes, usd } from "./format";

type Catalog = Record<string, string>;
const catalogs = (): Record<string, Catalog> =>
  Object.fromEntries(Object.entries(resources).map(([lang, r]) => [lang, r.translation as unknown as Catalog]));

const PLURAL = /_(zero|one|two|few|many|other)$/;
const SUFFIXES = ["one", "two", "few", "many", "other"];
const placeholders = (s: string) => [...s.matchAll(/\{\{\s*([A-Za-z_]+)[^}]*\}\}/g)].map((m) => m[1]).sort();

/** The plural categories a catalog of `lang` carries (what Intl says the language has). */
function categories(lang: string): string[] {
  return new Intl.PluralRules(lang).resolvedOptions().pluralCategories as string[];
}

describe("catalog parity", () => {
  const all = catalogs();
  const en = all.en;
  const bases = (c: Catalog) => [...new Set(Object.keys(c).map((k) => k.replace(PLURAL, "")))].sort();

  it("has the ten languages and an English catalog", () => {
    expect(LANGUAGES.map((l) => l.code)).toEqual(["en", "ru", "uk", "de", "fr", "es", "pt-BR", "it", "pl", "zh-CN"]);
    expect(Object.keys(all).sort()).toEqual(LANGUAGES.map((l) => l.code).sort());
  });

  for (const lang of LANGUAGES.map((l) => l.code).filter((c) => c !== "en")) {
    it(`${lang} has every key of English and none of its own`, () => {
      const c = all[lang];
      expect(bases(c)).toEqual(bases(en));
    });

    it(`${lang} has exactly its plural forms for every plural message`, () => {
      const c = all[lang];
      const need = categories(lang);
      const pluralBases = new Set(Object.keys(en).filter((k) => k.endsWith("_one")).map((k) => k.replace(PLURAL, "")));
      for (const base of pluralBases) {
        const have = SUFFIXES.concat("zero").filter((s) => `${base}_${s}` in c);
        expect(have.sort(), `${lang} ${base}`).toEqual([...need].sort());
      }
    });

    it(`${lang} uses the same placeholders as English`, () => {
      const c = all[lang];
      for (const [key, text] of Object.entries(c)) {
        const base = key.replace(PLURAL, "");
        const ref = en[key] ?? en[`${base}_other`] ?? en[base];
        expect(ref, `${lang} ${key} has no English counterpart`).toBeDefined();
        expect(placeholders(text), `${lang} ${key}`).toEqual(placeholders(ref));
      }
    });

    it(`${lang} leaves nothing empty`, () => {
      for (const [key, text] of Object.entries(all[lang])) expect(text.trim(), `${lang} ${key}`).not.toBe("");
    });
  }

  it("keeps plain keys apart from plural suffixes", () => {
    const pluralBases = new Set(Object.keys(en).filter((k) => k.endsWith("_one")).map((k) => k.replace(PLURAL, "")));
    for (const key of Object.keys(en)) {
      if (!PLURAL.test(key)) expect(pluralBases.has(key), key).toBe(false);
    }
  });
});

describe("plural forms", () => {
  it("picks one / few / many for Russian", async () => {
    await applyLanguageSetting("ru");
    const f = (n: number) => t("files", { count: n });
    expect(f(1)).toBe("1 файл");
    expect(f(2)).toBe("2 файла");
    expect(f(5)).toBe("5 файлов");
    expect(f(11)).toBe("11 файлов");
    expect(f(21)).toBe("21 файл");
    expect(f(22)).toBe("22 файла");
    expect(f(100)).toBe("100 файлов");
  });

  it("picks one / few / many for Polish", async () => {
    await applyLanguageSetting("pl");
    const f = (n: number) => t("files", { count: n });
    expect(f(1)).toBe("1 plik");
    expect(f(3)).toBe("3 pliki");
    expect(f(5)).toBe("5 plików");
    expect(f(12)).toBe("12 plików");
    expect(f(22)).toBe("22 pliki");
    expect(f(25)).toBe("25 plików");
  });

  it("has no plural for Chinese", async () => {
    await applyLanguageSetting("zh-CN");
    for (const n of [0, 1, 2, 5, 21]) expect(t("files", { count: n })).toBe(`${n} 个文件`);
  });

  it("has one and other for English and German", async () => {
    expect(t("files", { count: 1 })).toBe("1 file");
    expect(t("files", { count: 2 })).toBe("2 files");
    await applyLanguageSetting("de");
    expect(t("files", { count: 1 })).toBe("1 Datei");
    expect(t("files", { count: 3 })).toBe("3 Dateien");
  });

  it("formats the count with the language's grouping", async () => {
    expect(t("files", { count: 12345 })).toBe("12,345 files");
    await applyLanguageSetting("de");
    expect(t("files", { count: 12345 })).toBe("12.345 Dateien");
    await applyLanguageSetting("fr");
    expect(t("files", { count: 12345 }).replace(/\s/g, " ")).toBe("12 345 fichiers");
  });
});

describe("locale detection", () => {
  it("matches the exact language first", () => {
    expect(resolveLocale("uk_UA")).toBe("uk");
    expect(resolveLocale("ru_UA.UTF-8")).toBe("ru");
    expect(resolveLocale("pt-BR")).toBe("pt-BR");
    expect(resolveLocale("de-AT")).toBe("de");
  });

  it("falls back to the one regional variant we have", () => {
    expect(resolveLocale("pt_PT")).toBe("pt-BR");
    expect(resolveLocale("pt")).toBe("pt-BR");
    expect(resolveLocale("zh_TW")).toBe("zh-CN");
    expect(resolveLocale("zh-Hant-HK")).toBe("zh-CN");
  });

  it("ends in English for the unknown", () => {
    expect(resolveLocale("ja-JP")).toBeNull();
    expect(resolveLocale("")).toBeNull();
    expect(pickLocale(["ja-JP", "xx"])).toBe("en");
    expect(pickLocale(["ja-JP", "uk-UA", "ru"])).toBe("uk");
    expect(localeFor("klingon")).toBe("en");
    expect(localeFor("zh_TW")).toBe("zh-CN");
  });

  it("reads a setting as 'system' or a supported code", () => {
    expect(normalizeSetting("ru")).toBe("ru");
    expect(normalizeSetting("ru-RU")).toBe("system");
    expect(normalizeSetting(null)).toBe("system");
    expect(normalizeSetting("system")).toBe("system");
  });
});

describe("formatting", () => {
  it("formats numbers, sizes and prices for the language", async () => {
    expect(bytes(1536)).toBe("1.5 KB");
    expect(usd(1.5)).toBe("$1.50");
    await applyLanguageSetting("ru");
    expect(bytes(1536)).toBe("1,5 КБ");
    await applyLanguageSetting("de");
    expect(bytes(1536 * 1024)).toBe("1,5 MB");
    expect(usd(1234.5)).toMatch(/^1\.234,50\s\$$/);
  });

  it("switches the language of the instance", async () => {
    expect(i18n.language).toBe("en");
    await applyLanguageSetting("zh-CN");
    expect(i18n.language).toBe("zh-CN");
    expect(t("nav_settings")).toBe("设置");
    expect(document.documentElement.lang).toBe("zh-CN");
  });
});
