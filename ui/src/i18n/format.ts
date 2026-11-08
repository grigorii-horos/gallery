// Numbers, sizes, money and dates in the language in force, through `Intl`.
import { currentLanguage, t } from "./index";

const cache = new Map<string, Intl.NumberFormat | Intl.DateTimeFormat>();

function nf(options: Intl.NumberFormatOptions = {}): Intl.NumberFormat {
  const key = `n|${currentLanguage()}|${JSON.stringify(options)}`;
  let f = cache.get(key) as Intl.NumberFormat | undefined;
  if (!f) {
    f = new Intl.NumberFormat(currentLanguage(), options);
    cache.set(key, f);
  }
  return f;
}

function df(options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const key = `d|${currentLanguage()}|${JSON.stringify(options)}`;
  let f = cache.get(key) as Intl.DateTimeFormat | undefined;
  if (!f) {
    f = new Intl.DateTimeFormat(currentLanguage(), options);
    cache.set(key, f);
  }
  return f;
}

/** A whole number with the language's grouping: 12,345 / 12 345 / 12.345. */
export function num(n: number): string {
  return nf().format(n);
}

/** A number with exactly `digits` decimals. */
export function fixed(n: number, digits: number): string {
  return nf({ minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);
}

/** `1.5 MB`: 1024-based, the unit words from the catalog, the number by `Intl`. */
export function bytes(n: number): string {
  const units = ["unit_b", "unit_kb", "unit_mb", "unit_gb"] as const;
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return t(units[i], { n: i === 0 ? num(n) : fixed(v, 1) });
}

/** A price in US dollars: `$1.23`, with the language's separators. */
export function usd(v: number, digits = 2): string {
  return nf({ style: "currency", currency: "USD", minimumFractionDigits: digits, maximumFractionDigits: digits }).format(v);
}

/** A date and time: 5 Nov 2026, 10:15. Strings are ISO local times (`YYYY-MM-DDTHH:MM:SS`). */
export function dateTime(value: Date | string | number): string {
  const d = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(d.getTime())) return String(value);
  return df({ dateStyle: "medium", timeStyle: "short" }).format(d);
}

/** A date without the time. */
export function date(value: Date | string | number): string {
  const d = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(d.getTime())) return String(value);
  return df({ dateStyle: "medium" }).format(d);
}

/** A list with the language's conjunction: "a, b and c". */
export function list(items: readonly string[]): string {
  const key = `l|${currentLanguage()}`;
  let f = cache.get(key) as unknown as Intl.ListFormat | undefined;
  if (!f) {
    f = new Intl.ListFormat(currentLanguage(), { style: "long", type: "conjunction" });
    cache.set(key, f as unknown as Intl.NumberFormat);
  }
  return f.format(items);
}

/** "November 5, 2026, 10:15": the long date, then the 24-hour time (a run's wall clock). */
export function dateTimeLong(value: Date): string {
  const day = df({ dateStyle: "long" }).format(value);
  const time = df({ hour: "2-digit", minute: "2-digit", hourCycle: "h23" }).format(value);
  return `${day}, ${time}`;
}
