// Pieces shared by the condition forms. A condition's value is the raw
// TOML value as JSON; every form edits a copy and hands the new value up.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Field, TextInput } from "../fields";

export type Raw = Record<string, unknown>;

/** What a form needs to know about the config around the filter. */
export interface CondContext {
  /** All source ids. */
  sources: string[];
  /** Names defined in `[places]`. */
  places: string[];
  /** `@mask` suggestions. */
  suggestions: string[];
}

export interface CondFormProps {
  value: unknown;
  onChange: (v: unknown) => void;
  ctx: CondContext;
}

export const asObj = (v: unknown): Raw => (v && typeof v === "object" && !Array.isArray(v) ? (v as Raw) : {});

/** `o` with `key` set; an empty value (text, list, false, null) removes the key. */
export function withKey(o: Raw, key: string, v: unknown): Raw {
  const next = { ...o };
  const empty = v === undefined || v === null || v === "" || v === false || (Array.isArray(v) && v.length === 0);
  if (empty) delete next[key];
  else next[key] = v;
  return next;
}

export const str = (v: unknown): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : "");

/** A string or a list of strings as a list. */
export const strList = (v: unknown): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : typeof v === "string" && v ? [v] : [];

/** One item as a string, more as a list: how the core writes mask lists. */
export const oneOrList = (l: string[]): string | string[] => (l.length === 1 ? l[0] : l);

/** Numbers from text; null when it is not a number (the field is then dropped). */
export function numberOf(text: string): number | null {
  const n = Number(text.trim());
  return text.trim() !== "" && Number.isFinite(n) ? n : null;
}

/** A labelled optional text option; an empty field drops the option. */
export function OptText({
  o,
  k,
  label,
  onChange,
  placeholder,
  hint,
}: {
  o: Raw;
  k: string;
  label: string;
  onChange: (o: Raw) => void;
  placeholder?: string;
  hint?: string;
}) {
  return (
    <Field label={label} hint={hint}>
      <TextInput mono label={label} value={str(o[k])} placeholder={placeholder} onChange={(s) => onChange(withKey(o, k, s.trim() === "" ? null : s))} />
    </Field>
  );
}

/** A labelled optional number option. */
export function OptNumber({ o, k, label, onChange, placeholder }: { o: Raw; k: string; label: string; onChange: (o: Raw) => void; placeholder?: string }) {
  return (
    <Field label={label}>
      <input
        className="input mono"
        type="number"
        min={0}
        step="any"
        aria-label={label}
        value={str(o[k])}
        placeholder={placeholder}
        onChange={(e) => onChange(withKey(o, k, numberOf(e.target.value)))}
      />
    </Field>
  );
}

/** A labelled optional comma-separated list; the text is kept as typed, the list goes up. */
export function OptList({ o, k, label, onChange, placeholder }: { o: Raw; k: string; label: string; onChange: (o: Raw) => void; placeholder?: string }) {
  const [text, setText] = useState(strList(o[k]).join(", "));
  return (
    <Field label={label}>
      <TextInput
        mono
        label={label}
        value={text}
        placeholder={placeholder}
        onChange={(s) => {
          setText(s);
          onChange(
            withKey(
              o,
              k,
              s
                .split(",")
                .map((x) => x.trim())
                .filter(Boolean),
            ),
          );
        }}
      />
    </Field>
  );
}

/** The `not` toggle of a condition table. */
export function NotToggle({ o, onChange, disabled, title }: { o: Raw; onChange: (o: Raw) => void; disabled?: boolean; title?: string }) {
  const { t } = useTranslation();
  return (
    <label className="cond-not" title={title}>
      <input type="checkbox" checked={o.not === true} disabled={disabled} onChange={(e) => onChange(withKey(o, "not", e.target.checked))} /> {t("cond_not")}
    </label>
  );
}

/** A labelled select with an empty "any" choice first. */
export function OptSelect({
  o,
  k,
  label,
  options,
  onChange,
  anyLabel,
}: {
  o: Raw;
  k: string;
  label: string;
  options: [string, string][];
  onChange: (o: Raw) => void;
  anyLabel?: string;
}) {
  return (
    <Field label={label}>
      <select className="select" aria-label={label} value={str(o[k])} onChange={(e) => onChange(withKey(o, k, e.target.value))}>
        {anyLabel !== undefined && <option value="">{anyLabel}</option>}
        {options.map(([v, text]) => (
          <option key={v} value={v}>
            {text}
          </option>
        ))}
      </select>
    </Field>
  );
}

/** Toggle buttons for a set of values (months, weekdays, sources). */
export function ValueToggles<T extends string | number>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: [T, string][];
  value: T[];
  onChange: (v: T[]) => void;
}) {
  return (
    <div className="toggles" role="group" aria-label={label}>
      {options.map(([v, text]) => {
        const on = value.includes(v);
        return (
          <button key={String(v)} type="button" className="toggle" aria-pressed={on} onClick={() => onChange(on ? value.filter((x) => x !== v) : [...value, v])}>
            {text}
          </button>
        );
      })}
    </div>
  );
}
