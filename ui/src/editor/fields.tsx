// Form pieces of the config editor.
import { useId, useState, type KeyboardEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { errorText } from "../api";
import { editorApi, pickFolder } from "./api";
import type { ConfigEditor } from "./useConfigEditor";
import { maskItemKind, stepLabel, type FieldIssue, type MaskTestResult, type StepDraft } from "./model";

/// Inline issues of a field: the error (if it is about this field) and warnings.
export function Issues({ items, error }: { items: FieldIssue[]; error?: string | null }) {
  if (!error && items.length === 0) return null;
  return (
    <ul className="field-issues">
      {error && <li className="is-bad">{error}</li>}
      {items.map((w, i) => (
        <li key={i} className="is-warn">
          {w.message}
        </li>
      ))}
    </ul>
  );
}

export function Field({ label, children, hint }: { label: string; children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="field">
      <span className="field-label">{label}</span>
      <div className="field-body">
        {children}
        {hint && <p className="field-hint">{hint}</p>}
      </div>
    </div>
  );
}

/// A text input that reports every change; `null` for an empty optional value.
export function TextInput({
  value,
  onChange,
  placeholder,
  mono,
  label,
  invalid,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  mono?: boolean;
  label: string;
  invalid?: boolean;
}) {
  return (
    <input
      className={`input${mono ? " mono" : ""}`}
      aria-label={label}
      aria-invalid={invalid || undefined}
      value={value}
      placeholder={placeholder}
      spellCheck={false}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/// A folder path with "Choose…" (native dialog).
export function FolderInput({
  value,
  onChange,
  label,
  placeholder,
  invalid,
}: {
  value: string;
  onChange: (v: string) => void;
  label: string;
  placeholder?: string;
  invalid?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <div className="input-row">
      <TextInput value={value} onChange={onChange} label={label} placeholder={placeholder} mono invalid={invalid} />
      <button
        type="button"
        className="btn"
        onClick={async () => {
          const picked = await pickFolder(value || null, label);
          if (picked) onChange(picked);
        }}
      >
        {t("choose")}
      </button>
    </div>
  );
}

/// Toggles for the sources a filter takes files from.
export function SourceToggles({
  all,
  value,
  onChange,
  label,
}: {
  all: string[];
  value: string[];
  onChange: (v: string[]) => void;
  label: string;
}) {
  const { t } = useTranslation();
  const unknown = value.filter((v) => !all.includes(v));
  return (
    <div className="toggles" role="group" aria-label={label}>
      {[...all, ...unknown].map((id) => {
        const on = value.includes(id);
        return (
          <button
            key={id}
            type="button"
            className={`toggle${unknown.includes(id) ? " unknown" : ""}`}
            aria-pressed={on}
            title={unknown.includes(id) ? t("toggles_no_source") : undefined}
            onClick={() => onChange(on ? value.filter((v) => v !== id) : [...value, id])}
          >
            {id}
          </button>
        );
      })}
      {all.length === 0 && <span className="muted">{t("toggles_none")}</span>}
    </div>
  );
}

/// A mask as chips (alternatives, OR) with `@name` suggestions.
export function MaskChips({
  value,
  onChange,
  suggestions,
  label,
  placeholder,
  invalid,
  allowNot = true,
  exceptionField,
}: {
  value: string[];
  onChange: (v: string[]) => void;
  suggestions: string[];
  label: string;
  placeholder?: string;
  invalid?: boolean;
  /// `false` for lists that only turn files away (exclude, skip_if): a `!`
  /// item there would take more files, so it is refused.
  allowNot?: boolean;
  /// Where the exception belongs when `allowNot` is off.
  exceptionField?: string;
}) {
  const { t } = useTranslation();
  const field = exceptionField ?? t("field_matches");
  const [text, setText] = useState("");
  const [refused, setRefused] = useState<string | null>(null);
  const hint = placeholder ?? (allowNot ? t("chips_hint_not") : t("chips_hint"));
  const listId = useId();
  const add = (raw: string) => {
    let items = raw
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const bad = allowNot ? [] : items.filter((s) => maskItemKind(s) === "not");
    setRefused(
      bad.length
        ? t("chips_refused", { item: bad[0], field })
        : null,
    );
    items = items.filter((s) => !bad.includes(s) && !value.includes(s));
    if (items.length) onChange([...value, ...items]);
    setText("");
  };
  const chipClass = (m: string) =>
    maskItemKind(m) === "not" && !allowNot ? " bad" : maskItemKind(m) === "pattern" ? "" : ` ${maskItemKind(m)}`;
  const chipTitle = (m: string) =>
    maskItemKind(m) !== "not"
      ? undefined
      : allowNot
        ? t("chips_exception")
        : t("chips_not_allowed", { field });
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" || (e.key === "," && text.trim())) {
      e.preventDefault();
      add(text);
    } else if (e.key === "Backspace" && !text && value.length) {
      onChange(value.slice(0, -1));
    }
  };
  return (
    <div className="chips" aria-invalid={invalid || undefined}>
      {value.map((m, i) => (
        <span key={`${m}-${i}`} className={`chip${chipClass(m)}`} title={chipTitle(m)}>
          <code>{m}</code>
          <button
            type="button"
            className="chip-x"
            aria-label={t("remove_item", { item: m })}
            onClick={() => onChange(value.filter((_, j) => j !== i))}
          >
            ×
          </button>
        </span>
      ))}
      <input
        className="chips-input"
        aria-label={label}
        list={listId}
        value={text}
        placeholder={value.length ? "" : hint}
        spellCheck={false}
        onChange={(e) => {
          const v = e.target.value;
          // A suggestion picked from the list adds itself.
          if (suggestions.includes(v)) add(v);
          else setText(v);
        }}
        onKeyDown={onKey}
        onBlur={() => text.trim() && add(text)}
      />
      <datalist id={listId}>
        {suggestions
          .filter((s) => !value.includes(s))
          .map((s) => (
            <option key={s} value={s} />
          ))}
      </datalist>
      {refused && (
        <p role="alert" className="chips-refused is-bad small">
          {refused}
        </p>
      )}
    </div>
  );
}

/// "Test on a name": which sample names pass `match` / `exclude`.
export function MaskTester({
  editor,
  configPath,
  match,
  exclude,
}: {
  editor: ConfigEditor;
  configPath: string | null;
  match: string[] | null;
  exclude: string[];
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [names, setNames] = useState("IMG_20261105_101500.jpg\nScreenshot_20261105-101500.png\nVID_20261105_101500.mp4");
  const [result, setResult] = useState<MaskTestResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const run = async () => {
    setError(null);
    try {
      setResult(
        await editorApi.maskTest(configPath, editor.currentOps(), { match, exclude, names: names.split("\n") }),
      );
    } catch (e) {
      setResult(null);
      setError(errorText(e));
    }
  };
  if (!open)
    return (
      <button type="button" className="link small" onClick={() => setOpen(true)}>
        {t("tester_open")}
      </button>
    );
  return (
    <div className="tester">
      <textarea
        className="input mono"
        aria-label={t("tester_names")}
        rows={3}
        value={names}
        spellCheck={false}
        onChange={(e) => setNames(e.target.value)}
      />
      <div className="tester-actions">
        <button type="button" className="btn" onClick={run}>
          {t("tester_test")}
        </button>
        <button type="button" className="btn quiet" onClick={() => setOpen(false)}>
          {t("tester_hide")}
        </button>
      </div>
      {error && <p className="bad-text small">{error}</p>}
      {result && (
        <ul className="tester-result">
          {result.map((r) => (
            <li key={r.name} className={r.taken ? "yes" : "no"}>
              <span className="tester-mark" aria-hidden>
                {r.taken ? "✓" : "–"}
              </span>
              <code>{r.name}</code>
              <span className="muted">
                {r.taken ? t("tester_matches") : r.excluded ? t("tester_excluded") : t("tester_no_match")}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export interface PresetChoice {
  name: string;
  /** The preset's own mask, as written. */
  mask: string;
  description: string;
  own: boolean;
  /** A bundle: expands to several presets, has no mask of its own. */
  bundle?: boolean;
}

/// A pipe of preset steps: ordered chips (drag or ‹ › to reorder), each
/// showing the mask it runs on; a click opens its mask override.
export function PipeEditor({
  steps,
  onChange,
  presets,
  maskSuggestions,
  label,
  empty,
}: {
  steps: StepDraft[];
  onChange: (s: StepDraft[]) => void;
  presets: PresetChoice[];
  maskSuggestions: string[];
  label: string;
  empty: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState<number | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const choiceOf = (name: string) => presets.find((p) => p.name === name);
  const maskOf = (name: string) => {
    const c = choiceOf(name);
    return c && !c.bundle ? c.mask : null;
  };
  const move = (from: number, to: number) => {
    if (to < 0 || to >= steps.length || from === to) return;
    const next = [...steps];
    const [s] = next.splice(from, 1);
    next.splice(to, 0, s);
    onChange(next);
    setOpen(null);
  };
  const set = (i: number, s: StepDraft) => onChange(steps.map((x, j) => (j === i ? s : x)));
  return (
    <div className="pipe-edit" aria-label={label} role="group">
      {steps.length === 0 && <span className="muted">{empty}</span>}
      <ol className="pipe-chips">
        {steps.map((s, i) => {
          const l = stepLabel(s, maskOf(s.preset));
          const bundle = choiceOf(s.preset)?.bundle;
          const known = presets.some((p) => p.name === s.preset);
          return (
            <li
              key={i}
              className={`pchip${drag === i ? " dragging" : ""}${known ? "" : " unknown"}`}
              draggable
              onDragStart={(e) => {
                setDrag(i);
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", String(i));
              }}
              onDragEnd={() => setDrag(null)}
              onDragOver={(e) => drag !== null && e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                if (drag !== null) move(drag, i);
                setDrag(null);
              }}
            >
              <button
                type="button"
                className="pchip-main"
                aria-expanded={open === i}
                title={known ? undefined : t("pipe_no_preset")}
                onClick={() => setOpen(open === i ? null : i)}
              >
                <span className="pchip-name">{l.preset}</span>
                {bundle ? (
                  <code className="pchip-mask">{choiceOf(s.preset)?.description}</code>
                ) : (
                  l.mask && <code className={`pchip-mask${l.own ? " own" : ""}`}>{l.mask}</code>
                )}
              </button>
            </li>
          );
        })}
      </ol>
      {open !== null && steps[open] && (
        <div className="pchip-panel">
          <div className="pchip-panel-head">
            <strong>{steps[open].preset}</strong>
            <span className="muted">{t("pipe_step_of", { n: open + 1, total: steps.length })}</span>
            <span className="spacer" />
            <button type="button" className="btn quiet" disabled={open === 0} onClick={() => move(open, open - 1)}>
              ‹ {t("pipe_earlier")}
            </button>
            <button
              type="button"
              className="btn quiet"
              disabled={open === steps.length - 1}
              onClick={() => move(open, open + 1)}
            >
              {t("pipe_later")} ›
            </button>
            <button
              type="button"
              className="btn quiet danger"
              onClick={() => {
                onChange(steps.filter((_, j) => j !== open));
                setOpen(null);
              }}
            >
              {t("pipe_remove_step")}
            </button>
          </div>
          {choiceOf(steps[open].preset)?.bundle ? (
            <p className="field-hint">{t("pipe_set_hint")}</p>
          ) : (
          <label className="check">
            <input
              type="checkbox"
              checked={steps[open].mask !== null}
              onChange={(e) =>
                set(open, {
                  ...steps[open],
                  mask: e.target.checked ? [maskOf(steps[open].preset) ?? "*"] : null,
                })
              }
            />
            <span>
              {t("pipe_own_mask")}
              {maskOf(steps[open].preset) && (
                <>
                  {" "}
                  (<code>{maskOf(steps[open].preset)}</code>)
                </>
              )}
            </span>
          </label>
          )}
          {steps[open].mask !== null && !choiceOf(steps[open].preset)?.bundle && (
            <MaskChips
              value={steps[open].mask ?? []}
              onChange={(m) => set(open, { ...steps[open], mask: m })}
              suggestions={maskSuggestions}
              label={t("pipe_mask_of", { preset: steps[open].preset })}
            />
          )}
        </div>
      )}
      <select
        className="select"
        aria-label={t("pipe_add_aria", { label })}
        value=""
        onChange={(e) => {
          if (e.target.value) onChange([...steps, { preset: e.target.value, mask: null }]);
        }}
      >
        <option value="">{t("pipe_add")}</option>
        {([
          ["bundle", t("pipe_group_sets"), (p: PresetChoice) => !!p.bundle],
          ["own", t("pipe_group_own"), (p: PresetChoice) => !p.bundle && p.own],
          ["system", t("pipe_group_builtin"), (p: PresetChoice) => !p.bundle && !p.own],
        ] as const).map(([key, title, pick]) => {
          const list = presets.filter(pick);
          if (!list.length) return null;
          return (
            <optgroup key={key} label={title}>
              {list.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name} — {p.bundle ? p.description : p.mask}
                </option>
              ))}
            </optgroup>
          );
        })}
      </select>
    </div>
  );
}

/// The bottom bar of the editor screens: what is unsaved, save, discard.
export function EditorBar({ editor }: { editor: ConfigEditor }) {
  const { t } = useTranslation();
  const v = editor.view;
  const err = v?.error;
  const n = editor.ops.length;
  let state: ReactNode;
  let tone = "";
  if (editor.saving) state = t("saving");
  else if (err) {
    tone = "bad";
    state = (
      <>
        <strong>{t("bar_cannot_save")}</strong> {err.message}
      </>
    );
  } else if (editor.dirty) {
    tone = "dirty";
    state = t(editor.checking ? "bar_unsaved_checking" : "bar_unsaved", { count: n });
  } else if (editor.retargetable.length > 0) {
    tone = "ok";
    const total = editor.retargetable.reduce((n, r) => n + r.overrides, 0);
    state = (
      <>
        {t("bar_saved_retarget", { total })}{" "}
        <button type="button" className="btn" onClick={() => void editor.retarget()}>
          {t("bar_move")}
        </button>{" "}
        <button type="button" className="btn quiet" onClick={editor.dismissRetarget}>
          {t("bar_keep")}
        </button>
      </>
    );
  } else if (editor.notice?.tone === "ok") {
    tone = "ok";
    state = editor.notice.text;
  } else state = <span className="muted">{t("bar_clean")}</span>;
  return (
    <div className={`editor-bar ${tone}`} role="status">
      <span className="editor-bar-state">{state}</span>
      <button type="button" className="btn quiet" disabled={!n || editor.saving} onClick={editor.discard}>
        {t("bar_discard")}
      </button>
      <button
        type="button"
        className="btn primary"
        disabled={!editor.dirty || !!err || editor.saving || editor.checking}
        onClick={() => void editor.save()}
      >
        {t("save")}
      </button>
    </div>
  );
}

/// The editor cannot work on the file, or it changed outside.
export function EditorFailure({ editor }: { editor: ConfigEditor }) {
  const { t } = useTranslation();
  const f = editor.failure;
  const notice = editor.notice?.tone === "bad" ? editor.notice.text : null;
  if (!f && !notice) return null;
  if (!f) return <div className="notice bad">{notice}</div>;
  const title =
    f.kind === "changed"
      ? t("failure_changed")
      : f.kind === "read"
        ? t("failure_read")
        : t("failure_write");
  return (
    <div className="notice bad">
      <p className="notice-title">{title}</p>
      <p>{f.message}</p>
      {f.kind !== "read" && (
        <p>
          <button type="button" className="btn" onClick={editor.reload}>
            {t("failure_reload")}
          </button>{" "}
          <span className="muted">{t("failure_lost")}</span>
        </p>
      )}
    </div>
  );
}

