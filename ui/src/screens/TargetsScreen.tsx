import { useState, type DragEvent } from "react";
import { useTranslation } from "react-i18next";
import { t as tr } from "../i18n";
import { num } from "../i18n/format";
import type { AppData } from "../store";
import { EditorBar, EditorFailure, Field, FolderInput, Issues, MaskChips, MaskTester, PipeEditor, SourceToggles, TextInput, type PresetChoice } from "../editor/fields";
import {
  dropMove,
  emptyFilter,
  emptyTarget,
  freeName,
  issuesUnder,
  pathOf,
  type FieldIssue,
  type FilterDraft,
  type OutputDraft,
  type TargetDraft,
} from "../editor/model";
import type { ConfigEditor } from "../editor/useConfigEditor";
import { EffectPanel } from "../editor/EffectPanel";
import { useEffectPreview } from "../editor/useEffectPreview";

export function TargetsScreen({ data, editor }: { data: AppData; editor: ConfigEditor }) {
  const { t } = useTranslation();
  const v = editor.view;
  const [picked, setSelected] = useState<number | null>(null);
  const [drag, setDrag] = useState<number | null>(null);
  const [drop, setDrop] = useState<number | null>(null);
  const effect = useEffectPreview(data.configPath, editor.ops, editor.dirty && !editor.view?.error);
  if (!v) {
    if (editor.failure) return <EditorFailure editor={editor} />;
    return <p className="empty">{t("gate_reading")}</p>;
  }
  const list = v.draft.targets;
  const selected = picked !== null && list[picked] ? picked : list.length > 0 ? 0 : null;
  const current = selected !== null ? list[selected] : undefined;
  const general = v.error && !v.error.path?.startsWith("targets") ? v.error : null;
  const allNames = list.map((x) => x.name);

  const add = async () => {
    const fresh = { ...emptyTarget(), name: freeName(t("targets_new_name"), allNames) };
    const o = await editor.editNow({ op: "add_target", kind: "folders", target: fresh });
    if (o) setSelected(o.draft.targets.length - 1);
  };

  const onDragOver = (e: DragEvent, i: number) => {
    if (drag === null) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const before = e.clientY < r.top + r.height / 2 ? i : i + 1;
    if (drop !== before) setDrop(before);
  };
  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    const from = drag;
    const at = drop;
    setDrag(null);
    setDrop(null);
    if (from === null || at === null) return;
    const op = dropMove(from, at);
    if (!op) return;
    const o = await editor.editNow(op);
    if (o) setSelected(op.to);
  };

  return (
    <div className="editor-screen">
      <EditorFailure editor={editor} />
      {general && (
        <div className="notice bad">
          <p className="notice-title">{t("editor_invalid")}</p>
          <pre className="notice-text">{general.message}</pre>
        </div>
      )}
      <div className="targets">
        <section className="stage" aria-label={t("nav_targets")}>
          <header className="stage-head">
            <h2>{t("nav_targets")}</h2>
            <span className="count">{num(list.length)}</span>
            <button type="button" className="btn quiet add" onClick={add} aria-label={t("targets_add_aria", { stage: t("nav_targets") })}>
              {t("targets_add")}
            </button>
          </header>
          <p className="stage-hint">{t("targets_hint_before")}</p>
          <ol className={`target-list${drag !== null ? " droppable" : ""}`} onDrop={onDrop}>
            {list.length === 0 && <li className="empty small">{t("targets_none")}</li>}
            {list.map((tg, i) => {
              const path = pathOf(i);
              const bad = v.error ? issuesUnder([v.error], path).length > 0 : false;
              const warn = issuesUnder(v.warnings, path).length > 0;
              const lineBefore = drop === i;
              const lineAfter = drop === list.length && i === list.length - 1;
              return (
                <li
                  key={`${tg.name}-${i}`}
                  className={`${lineBefore ? "drop-before" : ""}${lineAfter ? " drop-after" : ""}`}
                  draggable
                  onDragStart={(e) => {
                    setDrag(i);
                    e.dataTransfer.effectAllowed = "move";
                    e.dataTransfer.setData("text/plain", tg.name);
                  }}
                  onDragEnd={() => {
                    setDrag(null);
                    setDrop(null);
                  }}
                  onDragOver={(e) => onDragOver(e, i)}
                >
                  <button type="button" className="target" aria-pressed={selected === i} onClick={() => setSelected(i)}>
                    <span className="grip" aria-hidden>
                      ⠿
                    </span>
                    <span className="target-text">
                      <span className="target-name">
                        {tg.name || <span className="muted">{t("targets_no_name")}</span>}
                        {tg.filters.some((f) => f.ai != null) && <span className="muted small"> {t("stage_ai")}</span>}
                        {bad ? <span className="flag bad" aria-label={t("flag_error")} /> : warn && <span className="flag warn" aria-label={t("flag_warnings")} />}
                      </span>
                      <span className="target-meta">{targetMeta(tg)}</span>
                    </span>
                  </button>
                </li>
              );
            })}
          </ol>
        </section>
        <aside className="inspector" aria-label={t("targets_selected")}>
          {current && selected !== null ? (
            <TargetForm
              key={`${selected}-${editor.formGen}`}
              target={current}
              index={selected}
              count={list.length}
              data={data}
              editor={editor}
              onSelect={setSelected}
            />
          ) : (
            <p className="empty">{t("targets_choose")}</p>
          )}
        </aside>
      </div>
      <EditorBar editor={editor} />
      {editor.dirty && !editor.view?.error && <EffectPanel state={effect} />}
    </div>
  );
}

function targetMeta(tg: TargetDraft): string {
  if (tg.outputs.length === 0) return tr("targets_no_outputs");
  const dirs = tg.outputs.map((o) => o.dir.split("/").filter(Boolean).pop() ?? o.dir);
  return dirs.join(", ");
}

function TargetForm({
  target,
  index,
  count,
  data,
  editor,
  onSelect,
}: {
  target: TargetDraft;
  index: number;
  count: number;
  data: AppData;
  editor: ConfigEditor;
  onSelect: (s: number | null) => void;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState(target);
  const v = editor.view!;
  const path = pathOf(index);
  const sourceIds = v.draft.sources.map((s) => s.id);
  const maskSuggestions = [
    ...(data.builtins?.masks ?? []).map((m) => `@${m.name}`),
    ...v.draft.masks.map((m) => `@${m}`),
  ];
  const errors = v.error ? [v.error] : [];
  const errAt = (sub: string) => {
    const e = v.error;
    return e && e.path === `${path}${sub}` ? e.message : null;
  };
  const warnAt = (sub: string) => v.warnings.filter((w) => w.path === `${path}${sub}`);

  const update = (next: TargetDraft) => {
    setForm(next);
    editor.edit({ op: "update_target", index, target: next });
  };
  const structural = async (op: Parameters<ConfigEditor["editNow"]>[0], then: number | null) => {
    const o = await editor.editNow(op);
    if (o) onSelect(then);
  };

  const ownErr = errors.find((e) => e.path === path)?.message ?? null;

  return (
    <div className="detail form">
      <div className="form-head">
        <TextInput label={t("triage_target_name")} value={form.name} invalid={!!errAt(".name")} onChange={(name) => update({ ...form, name })} />
      </div>
      <Issues items={[]} error={errAt(".name") ?? ownErr} />
      <div className="form-tools">
        <button type="button" className="btn quiet" disabled={index === 0} onClick={() => structural({ op: "move_target", from: index, to: index - 1 }, index - 1)}>
          {t("targets_up")}
        </button>
        <button type="button" className="btn quiet" disabled={index >= count - 1} onClick={() => structural({ op: "move_target", from: index, to: index + 1 }, index + 1)}>
          {t("targets_down")}
        </button>
        <span className="spacer" />
        <button type="button" className="btn quiet danger" onClick={() => structural({ op: "remove_target", index }, null)}>
          {t("targets_delete")}
        </button>
      </div>

      <h3>{t("targets_filters")} <span className="muted small">— {t("targets_filters_hint")}</span></h3>
      {form.filters.length === 0 && <p className="notice warn">{t("targets_no_filters")}</p>}
      <ul className="filters">
        {form.filters.map((f, i) => (
          <li key={i}>
            <FilterEditor
              value={f}
              sourceIds={sourceIds}
              path={`${path}.filters[${i}]`}
              errorAt={(p) => (v.error?.path === p ? v.error.message : null)}
              warnings={issuesUnder(v.warnings, `${path}.filters[${i}]`)}
              suggestions={maskSuggestions}
              editor={editor}
              configPath={data.configPath}
              onChange={(nf) => update({ ...form, filters: form.filters.map((x, j) => (j === i ? nf : x)) })}
              onRemove={() => update({ ...form, filters: form.filters.filter((_, j) => j !== i) })}
            />
          </li>
        ))}
      </ul>
      <button type="button" className="link small" onClick={() => update({ ...form, filters: [...form.filters, emptyFilter()] })}>
        {t("targets_add_filter")}
      </button>

      <h3>{t("trace_outputs")}</h3>
      <Issues items={warnAt(".outputs")} />
      {form.outputs.map((o, i) => (
        <OutputEditor
          key={i}
          value={o}
          n={i}
          path={`${path}.outputs[${i}]`}
          error={v.error && issuesUnder([v.error], `${path}.outputs[${i}]`)[0]?.message}
          warnings={issuesUnder(v.warnings, `${path}.outputs[${i}]`)}
          data={data}
          suggestions={maskSuggestions}
          onChange={(no) => update({ ...form, outputs: form.outputs.map((x, j) => (j === i ? no : x)) })}
          onRemove={() => update({ ...form, outputs: form.outputs.filter((_, j) => j !== i) })}
        />
      ))}
      <button type="button" className="link small" onClick={() => update({ ...form, outputs: [...form.outputs, { dir: "", server_dir: null, content: [], rename: [] }] })}>
        {t("targets_add_output")}
      </button>
    </div>
  );
}

function FilterEditor({
  value: f,
  sourceIds,
  path,
  errorAt,
  warnings,
  suggestions,
  editor,
  configPath,
  onChange,
  onRemove,
}: {
  value: FilterDraft;
  sourceIds: string[];
  path: string;
  errorAt: (p: string) => string | null;
  warnings: FieldIssue[];
  suggestions: string[];
  editor: ConfigEditor;
  configPath: string | null;
  onChange: (f: FilterDraft) => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="filter-edit">
      <Field label={t("nav_sources")}>
        <SourceToggles all={sourceIds} value={f.sources} label={t("targets_filter_sources")} onChange={(sources) => onChange({ ...f, sources })} />
        <Issues items={warnings} />
      </Field>
      <MatchFields
        match={f.match}
        exclude={f.exclude}
        path={path}
        errorAt={errorAt}
        suggestions={suggestions}
        onChange={(match, exclude) => onChange({ ...f, match, exclude })}
      />
      {f.ai != null ? (
        <Field label={t("targets_description")}>
          <textarea
            className="input"
            aria-label={t("targets_description")}
            aria-invalid={!!errorAt(`${path}.ai`) || undefined}
            rows={4}
            value={f.ai}
            onChange={(e) => onChange({ ...f, ai: e.target.value })}
          />
          <Issues items={[]} error={errorAt(`${path}.ai`)} />
        </Field>
      ) : (
        <button type="button" className="link small" onClick={() => onChange({ ...f, ai: "" })}>
          {t("targets_description")}
        </button>
      )}
      <div className="filter-foot">
        <MaskTester editor={editor} configPath={configPath} match={f.match} exclude={f.exclude} />
        <button type="button" className="link small danger" onClick={onRemove}>
          {t("targets_remove_filter")}
        </button>
      </div>
    </div>
  );
}

function MatchFields({
  match,
  exclude,
  path,
  errorAt,
  suggestions,
  onChange,
}: {
  match: string[] | null;
  exclude: string[];
  path: string;
  errorAt: (p: string) => string | null;
  suggestions: string[];
  onChange: (match: string[] | null, exclude: string[]) => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <Field label={t("field_matches")}>
        {match === null ? (
          <p className="field-inline">
            <span className="muted">{t("match_any_file")} </span>
            <button type="button" className="link small" onClick={() => onChange([], exclude)}>
              {t("match_set_mask")}
            </button>
          </p>
        ) : (
          <>
            <MaskChips label={t("match_mask_matches")} value={match} invalid={!!errorAt(`${path}.match`)} suggestions={suggestions} onChange={(m) => onChange(m, exclude)} />
            <button type="button" className="link small" onClick={() => onChange(null, exclude)}>
              {t("match_any")}
            </button>
          </>
        )}
        <Issues items={[]} error={errorAt(`${path}.match`)} />
      </Field>
      <Field label={t("match_except")}>
        <MaskChips
          label={t("match_mask_except")}
          value={exclude}
          invalid={!!errorAt(`${path}.exclude`)}
          suggestions={suggestions}
          placeholder={t("match_nothing")}
          allowNot={false}
          exceptionField={t("field_matches")}
          onChange={(x) => onChange(match, x)}
        />
        <Issues items={[]} error={errorAt(`${path}.exclude`)} />
      </Field>
    </>
  );
}

function OutputEditor({
  value: o,
  n,
  path,
  error,
  warnings,
  data,
  suggestions,
  onChange,
  onRemove,
}: {
  value: OutputDraft;
  n: number;
  path: string;
  error: string | null | undefined;
  warnings: FieldIssue[];
  data: AppData;
  suggestions: string[];
  onChange: (o: OutputDraft) => void;
  onRemove: () => void;
}) {
  const { t } = useTranslation();
  const presets = (kind: "content" | "name"): PresetChoice[] => [
    ...(data.summary?.presets[kind] ?? []).map((p) => ({ name: p.name, mask: p.mask, description: p.description, own: true, bundle: p.kind === "bundle" })),
    ...(data.builtins?.[kind] ?? []).map((p) => ({ name: p.name, mask: p.mask, description: p.description, own: false, bundle: p.kind === "bundle" })),
  ];
  const warnFor = (sub: string) => warnings.filter((w) => w.path === `${path}${sub}`);
  return (
    <div className="output">
      <div className="output-head">
        <span className="output-no">{t("output_n", { n: n + 1 })}</span>
        <button type="button" className="link small danger" onClick={onRemove}>
          {t("output_remove")}
        </button>
      </div>
      <Field label={t("folder")} hint={<>{t("output_templates")} <code>{"{YYYY}"}</code>, <code>{"{MM}"}</code>, <code>{"{source}"}</code></>}>
        <FolderInput label={t("output_folder", { n: n + 1 })} value={o.dir} invalid={!!error && !o.dir.trim()} onChange={(dir) => onChange({ ...o, dir })} />
        <Issues items={warnFor(".dir")} />
      </Field>
      <Field label={t("sources_on_server")}>
        <TextInput
          label={t("output_server_folder", { n: n + 1 })}
          mono
          value={o.server_dir ?? ""}
          placeholder={t("sources_same")}
          onChange={(s) => onChange({ ...o, server_dir: s.trim() ? s : null })}
        />
      </Field>
      <Field label={t("pipe_content")}>
        <PipeEditor
          label={t("output_content", { n: n + 1 })}
          steps={o.content}
          presets={presets("content")}
          maskSuggestions={suggestions}
          empty={t("output_copy")}
          onChange={(content) => onChange({ ...o, content })}
        />
        <Issues items={warnFor(".content")} />
      </Field>
      <Field label={t("pipe_name")}>
        <PipeEditor
          label={t("output_name", { n: n + 1 })}
          steps={o.rename}
          presets={presets("name")}
          maskSuggestions={suggestions}
          empty={t("output_keep_name")}
          onChange={(rename) => onChange({ ...o, rename })}
        />
      </Field>
      <Issues items={[]} error={error} />
    </div>
  );
}
