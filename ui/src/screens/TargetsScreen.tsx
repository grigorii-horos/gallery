import { useState, type DragEvent } from "react";
import type { Stage } from "../api";
import type { AppData } from "../store";
import { EditorBar, EditorFailure, Field, FolderInput, Issues, MaskChips, MaskTester, PipeEditor, SourceToggles, TextInput, type PresetChoice } from "../editor/fields";
import {
  dropMove,
  emptyTarget,
  freeName,
  issuesUnder,
  pathOf,
  type FieldIssue,
  type FilterDraft,
  type OutputDraft,
  type PrefilterDraft,
  type TargetDraft,
} from "../editor/model";
import type { ConfigEditor } from "../editor/useConfigEditor";
import { EffectPanel } from "../editor/EffectPanel";
import { useEffectPreview } from "../editor/useEffectPreview";

const STAGES: { id: Stage; title: string; hint: string }[] = [
  { id: "before_ai", title: "Masks before AI", hint: "in order, first match wins" },
  { id: "ai", title: "AI", hint: "order does not matter" },
  { id: "after_ai", title: "Masks after AI", hint: "in order, for the remaining files" },
];

const OTHER: Partial<Record<Stage, Stage>> = { before_ai: "after_ai", after_ai: "before_ai" };

interface Sel {
  stage: Stage;
  index: number;
}

export function TargetsScreen({ data, editor }: { data: AppData; editor: ConfigEditor }) {
  const v = editor.view;
  const [picked, setSelected] = useState<Sel | null>(null);
  const [drag, setDrag] = useState<Sel | null>(null);
  const [drop, setDrop] = useState<{ stage: Stage; before: number } | null>(null);
  const effect = useEffectPreview(data.configPath, editor.ops, editor.dirty && !editor.view?.error);
  if (!v) {
    if (editor.failure) return <EditorFailure editor={editor} />;
    return <p className="empty">Reading config…</p>;
  }
  const targets = v.draft.targets;
  const first = STAGES.find((st) => targets[st.id].length > 0);
  const selected =
    picked && targets[picked.stage][picked.index] ? picked : first ? { stage: first.id, index: 0 } : null;
  const current = selected ? targets[selected.stage][selected.index] : undefined;
  const general = v.error && !v.error.path?.startsWith("targets") ? v.error : null;
  const allNames = STAGES.flatMap((s) => targets[s.id].map((t) => t.name));

  const add = async (stage: Stage) => {
    const t = { ...emptyTarget(stage), name: freeName("New target", allNames) };
    const o = await editor.editNow({ op: "add_target", stage, target: t });
    if (o) setSelected({ stage, index: o.draft.targets[stage].length - 1 });
  };

  const onDragOver = (e: DragEvent, stage: Stage, i: number) => {
    if (!drag || !dropMoveOk(drag.stage, stage)) return;
    e.preventDefault();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const before = e.clientY < r.top + r.height / 2 ? i : i + 1;
    if (drop?.stage !== stage || drop.before !== before) setDrop({ stage, before });
  };
  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    const from = drag;
    const at = drop;
    setDrag(null);
    setDrop(null);
    if (!from || !at) return;
    const op = dropMove(from, at.stage, at.before);
    if (!op) return;
    const o = await editor.editNow(op);
    if (o) setSelected({ stage: op.to, index: op.to_index });
  };

  return (
    <div className="editor-screen">
      <EditorFailure editor={editor} />
      {general && (
        <div className="notice bad">
          <p className="notice-title">The config with these changes will not pass validation</p>
          <pre className="notice-text">{general.message}</pre>
        </div>
      )}
      <div className="targets">
        {STAGES.map((st, n) => {
          const list = targets[st.id];
          return (
            <section key={st.id} className="stage" aria-label={st.title}>
              <header className="stage-head">
                <span className="stage-no">{n + 1}</span>
                <h2>{st.title}</h2>
                <span className="count">{list.length}</span>
                <button type="button" className="btn quiet add" onClick={() => add(st.id)} aria-label={`Add target: ${st.title}`}>
                  + target
                </button>
              </header>
              <p className="stage-hint">{st.hint}</p>
              <ol
                className={`target-list${drag && dropMoveOk(drag.stage, st.id) ? " droppable" : ""}`}
                onDragOver={(e) => {
                  if (drag && dropMoveOk(drag.stage, st.id) && list.length === 0) {
                    e.preventDefault();
                    setDrop({ stage: st.id, before: 0 });
                  }
                }}
                onDrop={onDrop}
              >
                {list.length === 0 && <li className="empty small">No targets{drag && dropMoveOk(drag.stage, st.id) ? " — drop here" : ""}</li>}
                {list.map((t, i) => {
                  const path = pathOf(st.id, i);
                  const bad = v.error ? issuesUnder([v.error], path).length > 0 : false;
                  const warn = issuesUnder(v.warnings, path).length > 0;
                  const lineBefore = drop?.stage === st.id && drop.before === i;
                  const lineAfter = drop?.stage === st.id && drop.before === list.length && i === list.length - 1;
                  return (
                    <li
                      key={`${t.name}-${i}`}
                      className={`${lineBefore ? "drop-before" : ""}${lineAfter ? " drop-after" : ""}`}
                      draggable
                      onDragStart={(e) => {
                        setDrag({ stage: st.id, index: i });
                        e.dataTransfer.effectAllowed = "move";
                        e.dataTransfer.setData("text/plain", t.name);
                      }}
                      onDragEnd={() => {
                        setDrag(null);
                        setDrop(null);
                      }}
                      onDragOver={(e) => onDragOver(e, st.id, i)}
                    >
                      <button
                        type="button"
                        className="target"
                        aria-pressed={selected?.stage === st.id && selected.index === i}
                        onClick={() => setSelected({ stage: st.id, index: i })}
                      >
                        <span className="grip" aria-hidden>
                          ⠿
                        </span>
                        <span className="target-text">
                          <span className="target-name">
                            {t.name || <span className="muted">no name</span>}
                            {bad ? <span className="flag bad" aria-label="error" /> : warn && <span className="flag warn" aria-label="has warnings" />}
                          </span>
                          <span className="target-meta">{targetMeta(t)}</span>
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ol>
            </section>
          );
        })}
        <aside className="inspector" aria-label="Selected target">
          {current && selected ? (
            <TargetForm
              key={`${selected.stage}-${selected.index}-${editor.formGen}`}
              target={current}
              stage={selected.stage}
              index={selected.index}
              count={targets[selected.stage].length}
              data={data}
              editor={editor}
              onSelect={setSelected}
            />
          ) : (
            <p className="empty">Choose a target or add a new one.</p>
          )}
        </aside>
      </div>
      <EditorBar editor={editor} />
      {editor.dirty && !editor.view?.error && <EffectPanel state={effect} />}
    </div>
  );
}

function dropMoveOk(from: Stage, to: Stage): boolean {
  return (from === "ai") === (to === "ai");
}

function targetMeta(t: TargetDraft): string {
  if (t.outputs.length === 0) return "no outputs: files stay in place";
  const dirs = t.outputs.map((o) => o.dir.split("/").filter(Boolean).pop() ?? o.dir);
  return dirs.join(", ");
}

function TargetForm({
  target,
  stage,
  index,
  count,
  data,
  editor,
  onSelect,
}: {
  target: TargetDraft;
  stage: Stage;
  index: number;
  count: number;
  data: AppData;
  editor: ConfigEditor;
  onSelect: (s: Sel | null) => void;
}) {
  const [form, setForm] = useState(target);
  const v = editor.view!;
  const path = pathOf(stage, index);
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
    editor.edit({ op: "update_target", stage, index, target: next });
  };
  const structural = async (op: Parameters<ConfigEditor["editNow"]>[0], then: Sel | null) => {
    const o = await editor.editNow(op);
    if (o) onSelect(then);
  };

  const ownErr = errors.find((e) => e.path === path)?.message ?? null;
  const other = OTHER[stage];

  return (
    <div className="detail form">
      <div className="form-head">
        <TextInput label="Target name" value={form.name} invalid={!!errAt(".name")} onChange={(name) => update({ ...form, name })} />
      </div>
      <Issues items={[]} error={errAt(".name") ?? ownErr} />
      <div className="form-tools">
        <button type="button" className="btn quiet" disabled={index === 0} onClick={() => structural({ op: "move_target", from: stage, index, to: stage, to_index: index - 1 }, { stage, index: index - 1 })}>
          Up
        </button>
        <button type="button" className="btn quiet" disabled={index >= count - 1} onClick={() => structural({ op: "move_target", from: stage, index, to: stage, to_index: index + 1 }, { stage, index: index + 1 })}>
          Down
        </button>
        {other && (
          <button
            type="button"
            className="btn quiet"
            onClick={() => {
              const len = v.draft.targets[other].length;
              const at = other === "after_ai" ? 0 : len;
              void structural({ op: "move_target", from: stage, index, to: other, to_index: at }, { stage: other, index: at });
            }}
          >
            {other === "after_ai" ? "To “Masks after AI”" : "To “Masks before AI”"}
          </button>
        )}
        <span className="spacer" />
        <button type="button" className="btn quiet danger" onClick={() => structural({ op: "remove_target", stage, index }, null)}>
          Delete target
        </button>
      </div>

      {stage === "ai" ? (
        <>
          <h3>Sources</h3>
          <SourceToggles all={sourceIds} value={form.sources} label="Sources of the AI target" onChange={(sources) => update({ ...form, sources })} />
          <Issues items={warnAt(".sources")} />
          <h3>Description for the AI</h3>
          <textarea
            className="input"
            aria-label="Description for the AI"
            aria-invalid={!!errAt(".description") || undefined}
            rows={4}
            value={form.description ?? ""}
            onChange={(e) => update({ ...form, description: e.target.value })}
          />
          <Issues items={[]} error={errAt(".description")} />
          <h3>Prefilter</h3>
          {form.prefilter ? (
            <PrefilterEditor
              value={form.prefilter}
              path={`${path}.prefilter`}
              errorAt={(p) => (v.error?.path === p ? v.error.message : null)}
              suggestions={maskSuggestions}
              editor={editor}
              configPath={data.configPath}
              onChange={(prefilter) => update({ ...form, prefilter })}
            />
          ) : (
            <p className="field-inline">
              <span className="muted">none: the AI is asked about any file of the sources </span>
              <button type="button" className="link small" onClick={() => update({ ...form, prefilter: { match: ["@photo", "@video"], exclude: [] } })}>
                Add
              </button>
            </p>
          )}
        </>
      ) : (
        <>
          <h3>Filters <span className="muted small">— the target takes a file if any filter matches</span></h3>
          {form.filters.length === 0 && <p className="notice warn">No filters: the target takes no files.</p>}
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
          <button type="button" className="link small" onClick={() => update({ ...form, filters: [...form.filters, { sources: [], match: null, exclude: [] }] })}>
            Add filter
          </button>
        </>
      )}

      <h3>Outputs</h3>
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
        Add output
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
  return (
    <div className="filter-edit">
      <Field label="Sources">
        <SourceToggles all={sourceIds} value={f.sources} label="Sources of the filter" onChange={(sources) => onChange({ ...f, sources })} />
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
      <div className="filter-foot">
        <MaskTester editor={editor} configPath={configPath} match={f.match} exclude={f.exclude} />
        <button type="button" className="link small danger" onClick={onRemove}>
          Remove filter
        </button>
      </div>
    </div>
  );
}

function PrefilterEditor({
  value: p,
  path,
  errorAt,
  suggestions,
  editor,
  configPath,
  onChange,
}: {
  value: PrefilterDraft;
  path: string;
  errorAt: (p: string) => string | null;
  suggestions: string[];
  editor: ConfigEditor;
  configPath: string | null;
  onChange: (p: PrefilterDraft | null) => void;
}) {
  return (
    <div className="filter-edit">
      <MatchFields match={p.match} exclude={p.exclude} path={path} errorAt={errorAt} suggestions={suggestions} onChange={(match, exclude) => onChange({ match, exclude })} />
      <div className="filter-foot">
        <MaskTester editor={editor} configPath={configPath} match={p.match} exclude={p.exclude} />
        <button type="button" className="link small danger" onClick={() => onChange(null)}>
          Remove prefilter
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
  return (
    <>
      <Field label="Matches">
        {match === null ? (
          <p className="field-inline">
            <span className="muted">any file </span>
            <button type="button" className="link small" onClick={() => onChange([], exclude)}>
              Set mask
            </button>
          </p>
        ) : (
          <>
            <MaskChips label="Mask: matches" value={match} invalid={!!errorAt(`${path}.match`)} suggestions={suggestions} onChange={(m) => onChange(m, exclude)} />
            <button type="button" className="link small" onClick={() => onChange(null, exclude)}>
              Any file
            </button>
          </>
        )}
        <Issues items={[]} error={errorAt(`${path}.match`)} />
      </Field>
      <Field label="Except">
        <MaskChips
          label="Mask: except"
          value={exclude}
          invalid={!!errorAt(`${path}.exclude`)}
          suggestions={suggestions}
          placeholder="nothing"
          allowNot={false}
          exceptionField="Matches"
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
  const presets = (kind: "content" | "name"): PresetChoice[] => [
    ...(data.summary?.presets[kind] ?? []).map((p) => ({ name: p.name, mask: p.mask, description: p.description, own: true, bundle: p.kind === "bundle" })),
    ...(data.builtins?.[kind] ?? []).map((p) => ({ name: p.name, mask: p.mask, description: p.description, own: false, bundle: p.kind === "bundle" })),
  ];
  const warnFor = (sub: string) => warnings.filter((w) => w.path === `${path}${sub}`);
  return (
    <div className="output">
      <div className="output-head">
        <span className="output-no">Output {n + 1}</span>
        <button type="button" className="link small danger" onClick={onRemove}>
          Remove output
        </button>
      </div>
      <Field label="Folder" hint={<>Templates allowed: <code>{"{YYYY}"}</code>, <code>{"{MM}"}</code>, <code>{"{source}"}</code></>}>
        <FolderInput label={`Output ${n + 1} folder`} value={o.dir} invalid={!!error && !o.dir.trim()} onChange={(dir) => onChange({ ...o, dir })} />
        <Issues items={warnFor(".dir")} />
      </Field>
      <Field label="On the server">
        <TextInput
          label={`Folder on the server, output ${n + 1}`}
          mono
          value={o.server_dir ?? ""}
          placeholder="same as here"
          onChange={(s) => onChange({ ...o, server_dir: s.trim() ? s : null })}
        />
      </Field>
      <Field label="Content">
        <PipeEditor
          label={`Content, output ${n + 1}`}
          steps={o.content}
          presets={presets("content")}
          maskSuggestions={suggestions}
          empty="copy as is"
          onChange={(content) => onChange({ ...o, content })}
        />
        <Issues items={warnFor(".content")} />
      </Field>
      <Field label="Name">
        <PipeEditor
          label={`Name, output ${n + 1}`}
          steps={o.rename}
          presets={presets("name")}
          maskSuggestions={suggestions}
          empty="keep the name"
          onChange={(rename) => onChange({ ...o, rename })}
        />
      </Field>
      <Issues items={[]} error={error} />
    </div>
  );
}
