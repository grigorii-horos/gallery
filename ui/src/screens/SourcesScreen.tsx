import { useState } from "react";
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
  const [selected, setSelected] = useState<string | null>(null);
  const v = editor.view;
  if (!v) {
    if (editor.failure) return <EditorFailure editor={editor} />;
    return <p className="empty">Reading config…</p>;
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
          <p className="notice-title">The config with these changes will not pass validation</p>
          <pre className="notice-text">{general.message}</pre>
        </div>
      )}
      <div className="split">
        <section className="split-main" aria-label="Source list">
          <div className="list-head">
            <h2>
              Sources<span className="count">{sources.length}</span>
            </h2>
            <button type="button" className="btn" onClick={add}>
              Add source
            </button>
          </div>
          {sources.length === 0 ? (
            <p className="empty">No sources. Add a folder Gallery takes files from.</p>
          ) : (
            <table className="grid pick">
              <thead>
                <tr>
                  <th>id</th>
                  <th>Path</th>
                  <th className="num">Subfolders</th>
                  <th>AI prompt</th>
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
                          {s.id || <span className="muted">no id</span>}
                        </button>
                        {bad && <span className="flag bad" title="error" aria-label="error" />}
                      </th>
                      <td>
                        <Code>{s.path}</Code>
                      </td>
                      <td className="num">{s.recursive ? "yes" : "no"}</td>
                      <td>{promptLabel(s)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          )}
        </section>
        <aside className="split-side inspector" aria-label="Selected source">
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
            <p className="empty">Choose a source or add a new one.</p>
          )}
        </aside>
      </div>
      <EditorBar editor={editor} />
    </div>
  );
}

function promptLabel(s: SourceDraft) {
  const parts: string[] = [];
  if (s.ai_prefix_file || s.ai_prefix) parts.push("before");
  if (s.ai_suffix_file || s.ai_suffix) parts.push("after");
  return parts.length ? `own: ${parts.join(" and ")} the list` : <span className="muted">default</span>;
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
        <h2>{form.id || "New source"}</h2>
        <button
          type="button"
          className="btn quiet danger"
          onClick={async () => {
            await editor.editNow({ op: "remove_source", id: emitted });
            onRemoved();
          }}
        >
          Delete
        </button>
      </div>
      <Issues items={[]} error={sourceErr} />
      <Field label="id" hint="A short name; targets refer to the source by it. Renaming updates the targets.">
        <TextInput label="id" value={form.id} invalid={!!errAt("id")} onChange={(id) => update({ ...form, id })} />
        <Issues items={[]} error={errAt("id")} />
      </Field>
      <Field label="Folder">
        <FolderInput
          label="Source folder"
          value={form.path}
          invalid={!!errAt("path")}
          onChange={(p) => update({ ...form, path: p })}
        />
        <Issues items={[]} error={errAt("path")} />
      </Field>
      <Field label="On the server" hint="Empty — the same path as here.">
        <TextInput
          label="Path on the server"
          mono
          value={form.server_path ?? ""}
          placeholder="same as here"
          onChange={(s) => update({ ...form, server_path: opt(s) })}
        />
      </Field>
      <Field label="Subfolders">
        <label className="check">
          <input
            type="checkbox"
            checked={form.recursive}
            onChange={(e) => update({ ...form, recursive: e.target.checked })}
          />
          Scan subfolders too
        </label>
      </Field>
      <Field label="Skip">
        {form.skip === null ? (
          <p className="field-inline">
            <span className="muted">default list: </span>
            {DEFAULT_SKIP.map((s) => (
              <Code key={s}>{s} </Code>
            ))}
            <button type="button" className="link small" onClick={() => update({ ...form, skip: [...DEFAULT_SKIP] })}>
              Change
            </button>
          </p>
        ) : (
          <>
            <MaskChips
              label="What to skip"
              value={form.skip}
              suggestions={[]}
              placeholder="glob, e.g. *.tmp"
              onChange={(skip) => update({ ...form, skip })}
            />
            <button type="button" className="link small" onClick={() => update({ ...form, skip: null })}>
              Restore the default list
            </button>
          </>
        )}
      </Field>
      <h3>AI prompt for files of this source</h3>
      <PromptSlot slot="prefix" form={form} update={update} configFile={configFile} />
      <PromptSlot slot="suffix" form={form} update={update} configFile={configFile} />
      <div className="form-actions">
        <button type="button" className="btn" onClick={showPrompt} disabled={!form.id}>
          Show the final prompt
        </button>
      </div>
      {preview !== null &&
        (typeof preview === "string" ? (
          <p className="bad-text">{preview}</p>
        ) : (
          <section className="prompt-preview" aria-label="Final prompt">
            <p className="muted small">
              {preview.categories.length
                ? `Categories: ${preview.categories.length}. A target's prefilter may remove some of them for a particular file.`
                : "No AI target takes files of this source: the AI will not see them."}
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
  const text = slot === "prefix" ? form.ai_prefix : form.ai_suffix;
  const file = slot === "prefix" ? form.ai_prefix_file : form.ai_suffix_file;
  const mode: "none" | "file" | "text" = file !== null ? "file" : text !== null ? "text" : "none";
  const set = (t: string | null, f: string | null) =>
    update(
      slot === "prefix"
        ? { ...form, ai_prefix: t, ai_prefix_file: f }
        : { ...form, ai_suffix: t, ai_suffix_file: f },
    );
  const title = slot === "prefix" ? "Before the category list" : "After the category list";
  return (
    <Field label={slot === "prefix" ? "Prefix" : "Suffix"} hint={title}>
      <div className="seg" role="radiogroup" aria-label={title}>
        {(
          [
            ["none", "none"],
            ["file", "file"],
            ["text", "text"],
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
            label={`File: ${title.toLowerCase()}`}
            mono
            value={file ?? ""}
            placeholder="prompts/camera.md — relative to the config folder"
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
            Choose…
          </button>
        </div>
      )}
      {mode === "text" && (
        <textarea
          className="input"
          aria-label={`Text: ${title.toLowerCase()}`}
          rows={4}
          value={text ?? ""}
          onChange={(e) => set(e.target.value, null)}
        />
      )}
    </Field>
  );
}
