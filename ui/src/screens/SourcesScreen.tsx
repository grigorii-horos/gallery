import { useState } from "react";
import { useTranslation } from "react-i18next";
import { t } from "../i18n";
import { num } from "../i18n/format";
import { errorText } from "../api";
import type { AppData } from "../store";
import { Code } from "../components/ConfigGate";
import { editorApi, pickTextFile } from "../editor/api";
import { EditorBar, EditorFailure, Field, FolderInput, Issues, MaskChips, TextInput } from "../editor/fields";
import {
  DEFAULT_SKIP,
  emptySource,
  freeName,
  issuesUnder,
  relativeTo,
  type PromptPreview,
  type SourceDraft,
} from "../editor/model";
import type { ConfigEditor } from "../editor/useConfigEditor";

export function SourcesScreen({ data, editor }: { data: AppData; editor: ConfigEditor }) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string | null>(null);
  const v = editor.view;
  if (!v) {
    if (editor.failure) return <EditorFailure editor={editor} />;
    return <p className="empty">{t("gate_reading")}</p>;
  }
  const sources = v.draft.sources;
  const current = sources.find((s) => s.id === selected) ?? sources[0];
  const index = current ? sources.indexOf(current) : -1;
  const configFile = data.summary?.path ?? data.status?.config_path ?? "";
  const general = v.error && !v.error.path?.startsWith("sources") ? v.error : null;

  const add = async () => {
    const id = freeName("new", sources.map((s) => s.id));
    const o = await editor.editNow({ op: "add_source", source: { ...emptySource(), id } });
    if (o) setSelected(id);
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
      <div className="split">
        <section className="split-main" aria-label={t("sources_list")}>
          <div className="list-head">
            <h2>
              {t("nav_sources")}<span className="count">{num(sources.length)}</span>
            </h2>
            <button type="button" className="btn" onClick={add}>
              {t("sources_add")}
            </button>
          </div>
          {sources.length === 0 ? (
            <p className="empty">{t("sources_none")}</p>
          ) : (
            <table className="grid pick">
              <thead>
                <tr>
                  <th>id</th>
                  <th>{t("sources_col_path")}</th>
                  <th className="num">{t("sources_col_subfolders")}</th>
                  <th>{t("sources_col_prompt")}</th>
                </tr>
              </thead>
              <tbody>
                {sources.map((s, i) => {
                  const bad = issuesUnder(v.error ? [v.error] : [], `sources[${i}]`).length > 0;
                  return (
                    <tr
                      key={`${s.id}-${i}`}
                      className={s === current ? "selected" : undefined}
                      onClick={() => setSelected(s.id)}
                    >
                      <th scope="row">
                        <button type="button" className="row-pick" aria-pressed={s === current}>
                          {s.id || <span className="muted">{t("sources_no_id")}</span>}
                        </button>
                        {bad && <span className="flag bad" title={t("flag_error")} aria-label={t("flag_error")} />}
                      </th>
                      <td>
                        <Code>{s.path}</Code>
                      </td>
                      <td className="num">{s.recursive ? t("yes") : t("no")}</td>
                      <td>{promptLabel(s)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </section>
        <aside className="split-side inspector" aria-label={t("sources_selected")}>
          {current ? (
            <SourceForm
              key={`${index}-${editor.formGen}`}
              source={current}
              index={index}
              editor={editor}
              configPath={data.configPath}
              configFile={configFile}
              onIdChange={setSelected}
              onRemoved={() => setSelected(null)}
            />
          ) : (
            <p className="empty">{t("sources_choose")}</p>
          )}
        </aside>
      </div>
      <EditorBar editor={editor} />
    </div>
  );
}

function promptLabel(s: SourceDraft) {
  const before = !!(s.ai_prefix_file || s.ai_prefix);
  const after = !!(s.ai_suffix_file || s.ai_suffix);
  if (before && after) return t("sources_prompt_both");
  if (before) return t("sources_prompt_before");
  if (after) return t("sources_prompt_after");
  return <span className="muted">{t("sources_prompt_default")}</span>;
}

type Slot = "prefix" | "suffix";

function SourceForm({
  source,
  index,
  editor,
  configPath,
  configFile,
  onIdChange,
  onRemoved,
}: {
  source: SourceDraft;
  index: number;
  editor: ConfigEditor;
  configPath: string | null;
  configFile: string;
  onIdChange: (id: string) => void;
  onRemoved: () => void;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState(source);
  // The id the last update left the source with: the next one starts there.
  const [emitted, setEmitted] = useState(source.id);
  const [preview, setPreview] = useState<PromptPreview | string | null>(null);
  const path = `sources[${index}]`;
  const v = editor.view!;
  const errAt = (field: string) => (v.error?.path === `${path}.${field}` ? v.error.message : null);
  const sourceErr = v.error?.path === path ? v.error.message : null;

  const update = (next: SourceDraft) => {
    setForm(next);
    editor.edit({ op: "update_source", id: emitted, source: next });
    setEmitted(next.id);
    if (next.id !== form.id) onIdChange(next.id);
    setPreview(null);
  };
  const opt = (s: string) => (s.trim() ? s : null);

  const showPrompt = async () => {
    try {
      setPreview(await editorApi.previewPrompt(configPath, editor.currentOps(), form.id));
    } catch (e) {
      setPreview(errorText(e));
    }
  };

  return (
    <div className="detail form">
      <div className="form-head">
        <h2>{form.id || t("sources_new")}</h2>
        <button
          type="button"
          className="btn quiet danger"
          onClick={async () => {
            await editor.editNow({ op: "remove_source", id: emitted });
            onRemoved();
          }}
        >
          {t("delete")}
        </button>
      </div>
      <Issues items={[]} error={sourceErr} />
      <Field label="id" hint={t("sources_id_hint")}>
        <TextInput label="id" value={form.id} invalid={!!errAt("id")} onChange={(id) => update({ ...form, id })} />
        <Issues items={[]} error={errAt("id")} />
      </Field>
      <Field label={t("folder")}>
        <FolderInput
          label={t("sources_folder")}
          value={form.path}
          invalid={!!errAt("path")}
          onChange={(p) => update({ ...form, path: p })}
        />
        <Issues items={[]} error={errAt("path")} />
      </Field>
      <Field label={t("sources_on_server")} hint={t("sources_server_hint")}>
        <TextInput
          label={t("sources_server_path")}
          mono
          value={form.server_path ?? ""}
          placeholder={t("sources_same")}
          onChange={(s) => update({ ...form, server_path: opt(s) })}
        />
      </Field>
      <Field label={t("sources_col_subfolders")}>
        <label className="check">
          <input
            type="checkbox"
            checked={form.recursive}
            onChange={(e) => update({ ...form, recursive: e.target.checked })}
          />
          {t("sources_scan_sub")}
        </label>
      </Field>
      <Field label={t("sources_skip")}>
        {form.skip === null ? (
          <p className="field-inline">
            <span className="muted">{t("sources_default_list")} </span>
            {DEFAULT_SKIP.map((s) => (
              <Code key={s}>{s} </Code>
            ))}
            <button type="button" className="link small" onClick={() => update({ ...form, skip: [...DEFAULT_SKIP] })}>
              {t("sources_change")}
            </button>
          </p>
        ) : (
          <>
            <MaskChips
              label={t("sources_what_skip")}
              value={form.skip}
              suggestions={[]}
              placeholder={t("sources_skip_placeholder")}
              onChange={(skip) => update({ ...form, skip })}
            />
            <button type="button" className="link small" onClick={() => update({ ...form, skip: null })}>
              {t("sources_restore")}
            </button>
          </>
        )}
      </Field>
      <h3>{t("sources_prompt_title")}</h3>
      <PromptSlot slot="prefix" form={form} update={update} configFile={configFile} />
      <PromptSlot slot="suffix" form={form} update={update} configFile={configFile} />
      <div className="form-actions">
        <button type="button" className="btn" onClick={showPrompt} disabled={!form.id}>
          {t("sources_show_prompt")}
        </button>
      </div>
      {preview !== null &&
        (typeof preview === "string" ? (
          <p className="bad-text">{preview}</p>
        ) : (
          <section className="prompt-preview" aria-label={t("sources_final_prompt")}>
            <p className="muted small">
              {preview.categories.length
                ? t("sources_categories", { n: preview.categories.length })
                : t("sources_no_categories")}
            </p>
            <pre>{preview.prompt}</pre>
          </section>
        ))}
    </div>
  );
}

function PromptSlot({
  slot,
  form,
  update,
  configFile,
}: {
  slot: Slot;
  form: SourceDraft;
  update: (s: SourceDraft) => void;
  configFile: string;
}) {
  const { t } = useTranslation();
  const text = slot === "prefix" ? form.ai_prefix : form.ai_suffix;
  const file = slot === "prefix" ? form.ai_prefix_file : form.ai_suffix_file;
  const mode: "none" | "file" | "text" = file !== null ? "file" : text !== null ? "text" : "none";
  const set = (txt: string | null, f: string | null) =>
    update(
      slot === "prefix"
        ? { ...form, ai_prefix: txt, ai_prefix_file: f }
        : { ...form, ai_suffix: txt, ai_suffix_file: f },
    );
  const title = slot === "prefix" ? t("slot_before") : t("slot_after");
  return (
    <Field label={slot === "prefix" ? t("slot_prefix") : t("slot_suffix")} hint={title}>
      <div className="seg" role="radiogroup" aria-label={title}>
        {(
          [
            ["none", t("slot_none")],
            ["file", t("slot_file")],
            ["text", t("slot_text")],
          ] as const
        ).map(([m, label]) => (
          <button
            key={m}
            type="button"
            role="radio"
            aria-checked={mode === m}
            className="seg-item"
            onClick={() => {
              if (m === mode) return;
              if (m === "none") set(null, null);
              if (m === "file") set(null, "");
              if (m === "text") set(text ?? "", null);
            }}
          >
            {label}
          </button>
        ))}
      </div>
      {mode === "file" && (
        <div className="input-row">
          <TextInput
            label={t("slot_file_aria", { title: title.toLowerCase() })}
            mono
            value={file ?? ""}
            placeholder={t("slot_file_placeholder")}
            onChange={(f) => set(null, f)}
          />
          <button
            type="button"
            className="btn"
            onClick={async () => {
              const picked = await pickTextFile(null, title);
              if (picked) set(null, relativeTo(picked, configFile));
            }}
          >
            {t("choose")}
          </button>
        </div>
      )}
      {mode === "text" && (
        <textarea
          className="input"
          aria-label={t("slot_text_aria", { title: title.toLowerCase() })}
          rows={4}
          value={text ?? ""}
          onChange={(e) => set(e.target.value, null)}
        />
      )}
    </Field>
  );
}
