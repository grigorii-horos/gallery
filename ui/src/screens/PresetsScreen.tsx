import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { t, type MessageKey } from "../i18n";
import { num } from "../i18n/format";
import { errorText, type MaskInfo, type PresetInfo } from "../api";
import type { AppData } from "../store";
import { Code } from "../components/ConfigGate";
import { editorApi } from "../editor/api";
import { EditorBar, EditorFailure, Field, Issues, MaskChips, PipeEditor, TextInput, type PresetChoice } from "../editor/fields";
import type {
  ContentKind,
  ContentPresetDraft,
  EditOp,
  MaskDraft,
  MaskTestResult,
  NamePresetDraft,
  PresetKind,
} from "../editor/model";
import {
  KINDS,
  KIND_MASK,
  coreDefault,
  defaultBundle,
  defaultContent,
  fieldsOf,
  isBundle,
  freeKey,
  limitFor,
  newNameError,
  parseSamples,
  switchKind,
  validateBundle,
  validateContent,
  validateMask,
  validateName,
  type FieldSpec,
  type Limit,
  type NameResult,
} from "../editor/presets";
import type { ConfigEditor } from "../editor/useConfigEditor";

type Tab = "content" | "bundles" | "name" | "masks";

const TABS: { id: Tab; title: MessageKey; kind: PresetKind; path: string }[] = [
  { id: "content", title: "presets_tab_content", kind: "content", path: "content_presets" },
  { id: "bundles", title: "presets_tab_sets", kind: "content", path: "content_presets" },
  { id: "name", title: "presets_tab_name", kind: "name", path: "name_presets" },
  { id: "masks", title: "presets_tab_masks", kind: "mask", path: "masks" },
];

const kindTitle = (id: string): string | undefined => {
  const k = KINDS.find((x) => x.id === id);
  return k ? t(k.title) : undefined;
};

interface Sel {
  own: boolean;
  name: string;
}

const NAME_SAMPLES = "IMG_20261105_101500.JPEG\nPXL_20261105_101500123.jpg\n20260102_030405.jpg\nScreenshot_20261105-101500.png\nVID_20261105_101500.mp4";
const MASK_SAMPLES = "IMG_20261105_101500.jpg\nScreenshot_20261105-101500.png\nVID_20261105_101500.mp4\nIMG-20261105-WA0001.jpg\nscan_passport.pdf";

export function PresetsScreen({ data, editor }: { data: AppData; editor: ConfigEditor }) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("content");
  // The preset whose removal was tried last: a refusal notice belongs to it.
  const [removing, setRemoving] = useState<string | null>(null);
  const [sel, setSel] = useState<Record<Tab, Sel | null>>({ content: null, bundles: null, name: null, masks: null });
  const [limits, setLimits] = useState<Limit[]>([]);
  const [nameSamples, setNameSamples] = useState(NAME_SAMPLES);
  const [maskSamples, setMaskSamples] = useState(MASK_SAMPLES);
  const [date, setDate] = useState<string | null>("2026-11-05T10:15:00");

  useEffect(() => {
    editorApi.presetLimits().then(setLimits, () => setLimits([]));
  }, []);

  const v = editor.view;
  const builtin = data.builtins;
  if (!v) {
    if (editor.failure) return <EditorFailure editor={editor} />;
    return <p className="empty">{t("gate_reading")}</p>;
  }
  const d = v.draft;
  const ownNames: Record<Tab, string[]> = {
    content: (d.content_presets ?? []).filter((p) => !isBundle(p)).map((p) => p.name),
    bundles: (d.content_presets ?? []).filter(isBundle).map((p) => p.name),
    name: (d.name_presets ?? []).map((p) => p.name),
    masks: (d.mask_defs ?? []).map((m) => m.name),
  };
  const builtinNames: Record<Tab, string[]> = {
    // In the order the list shows them.
    content: groupByKind(builtinOf(builtin, "content"), true).flatMap(([, l]) => l.map((p) => p.name)),
    bundles: builtinOf(builtin, "bundles").map((p) => p.name),
    name: (builtin?.name ?? []).map((p) => p.name),
    masks: (builtin?.masks ?? []).map((m) => m.name),
  };
  const meta = TABS.find((t) => t.id === tab)!;
  const picked = sel[tab];
  const current: Sel | null =
    picked && (picked.own ? ownNames[tab] : builtinNames[tab]).includes(picked.name)
      ? picked
      : ownNames[tab].length
        ? { own: true, name: ownNames[tab][0] }
        : builtinNames[tab].length
          ? { own: false, name: builtinNames[tab][0] }
          : null;
  // Bundles and presets share one namespace.
  const shared = tab === "content" || tab === "bundles";
  const takenNames = shared ? [...ownNames.content, ...ownNames.bundles] : ownNames[tab];
  const systemNames = shared ? [...builtinNames.content, ...builtinNames.bundles] : builtinNames[tab];
  const select = (s: Sel) => setSel((x) => ({ ...x, [tab]: s }));
  const usage = v.usage?.[tab === "masks" ? "masks" : tab === "bundles" ? "content" : tab] ?? {};
  const maskSuggestions = [...builtinNames.masks, ...ownNames.masks].map((m) => `@${m}`);
  const errPath = (name: string) => `${meta.path}.${name}`;
  const hasError = (name: string) => {
    const p = v.error?.path ?? "";
    return p === errPath(name) || p.startsWith(`${errPath(name)}.`);
  };
  const general = v.error && !TABS.some((t) => v.error?.path?.startsWith(`${t.path}.`)) ? v.error : null;

  const add = async () => {
    const taken = [...ownNames[tab], ...builtinNames[tab], ...ownNames.content, ...ownNames.bundles, ...builtinNames.content, ...builtinNames.bundles];
    let op: EditOp;
    let name: string;
    if (tab === "content") {
      name = freeKey("my-avif", taken);
      op = { op: "add_content_preset", preset: defaultContent("photo", name) };
    } else if (tab === "bundles") {
      name = freeKey("my-pack", [...taken, ...(d.content_presets ?? []).map((p) => p.name)]);
      op = { op: "add_content_preset", preset: defaultBundle(name, ["avif-high", "png-avif", "webp-avif"]) };
    } else if (tab === "name") {
      name = freeKey("my-name", taken);
      op = {
        op: "add_name_preset",
        preset: { name, template: "{date:YYYYMMDD_HHMMSS}.{ext}", date: "capture", mask: null, skip_if: null, normalize_ext: true },
      };
    } else {
      name = freeKey("my-mask", taken);
      op = { op: "add_mask", mask: { name, items: ["*.jpg"] } };
    }
    const o = await editor.editNow(op);
    if (o) setSel((x) => ({ ...x, [tab]: { own: true, name } }));
  };

  const ownList = (): { name: string; meta: string }[] => {
    if (tab === "content" || tab === "bundles")
      return (d.content_presets ?? []).filter((p) => isBundle(p) === (tab === "bundles")).map((p) => ({
        name: p.name,
        meta: v.own?.content.find((i) => i.name === p.name)?.description ?? kindTitle(p.kind) ?? p.kind,
      }));
    if (tab === "name") return (d.name_presets ?? []).map((p) => ({ name: p.name, meta: p.template }));
    return (d.mask_defs ?? []).map((m) => ({ name: m.name, meta: m.items.join(" | ") }));
  };

  return (
    <div className="editor-screen presets">
      <div className="tabs" role="tablist" aria-label={t("presets_kinds")}>
        {TABS.map((x) => (
          <button key={x.id} type="button" role="tab" aria-selected={tab === x.id} className="tab" onClick={() => setTab(x.id)}>
            {t(x.title)}
            <span className="count">{num(builtinNames[x.id].length + ownNames[x.id].length)}</span>
          </button>
        ))}
      </div>
      <EditorFailure editor={editor} />
      {general && (
        <div className="notice bad">
          <p className="notice-title">{t("editor_invalid")}</p>
          <pre className="notice-text">{general.message}</pre>
        </div>
      )}
      <div className="preset-edit">
        <div className="preset-lists">
          <section aria-label={t("pipe_group_own")}>
            <header className="stage-head">
              <h2>{t("pipe_group_own")}</h2>
              <span className="count">{ownNames[tab].length}</span>
              <button type="button" className="btn quiet add" onClick={() => void add()}>
                {tab === "masks" ? t("presets_add_mask") : tab === "bundles" ? t("presets_add_set") : t("presets_add_preset")}
              </button>
            </header>
            <ul className="target-list">
              {ownList().length === 0 && (
                <li className="empty small">
                  {tab === "masks" ? t("presets_no_masks") : tab === "bundles" ? t("presets_no_sets") : t("presets_no_presets")} — {t("presets_add_or_copy")}
                </li>
              )}
              {ownList().map((it) => (
                <li key={it.name}>
                  <ItemButton
                    name={tab === "masks" ? `@${it.name}` : it.name}
                    meta={it.meta}
                    pressed={!!current?.own && current.name === it.name}
                    bad={hasError(it.name)}
                    used={(usage[it.name] ?? []).length}
                    onClick={() => select({ own: true, name: it.name })}
                  />
                </li>
              ))}
            </ul>
          </section>
          <section aria-label={t("pipe_group_builtin")}>
            <header className="stage-head">
              <h2>{t("pipe_group_builtin")}</h2>
              <span className="count">{builtinNames[tab].length}</span>
            </header>
            {!builtin ? (
              <p className="empty small">{t("presets_loading_builtin")}</p>
            ) : tab === "masks" ? (
              <ul className="target-list">
                {builtin.masks.map((m) => (
                  <li key={m.name}>
                    <ItemButton
                      name={`@${m.name}`}
                      meta={m.source}
                      pressed={!!current && !current.own && current.name === m.name}
                      onClick={() => select({ own: false, name: m.name })}
                    />
                  </li>
                ))}
              </ul>
            ) : (
              groupByKind(builtinOf(builtin, tab), tab === "content").map(([kind, list]) => (
                <div key={kind} className="preset-group">
                  {kind && <h3 className="preset-group-title">{kindTitle(kind) ?? kind}</h3>}
                  <ul className="target-list">
                    {list.map((p) => (
                      <li key={p.name}>
                        <ItemButton
                          name={p.name}
                          meta={p.description}
                          pressed={!!current && !current.own && current.name === p.name}
                          onClick={() => select({ own: false, name: p.name })}
                        />
                      </li>
                    ))}
                  </ul>
                </div>
              ))
            )}
          </section>
        </div>
        <aside className="inspector" aria-label={t("presets_selected")}>
          {!current ? (
            <p className="empty">{t("presets_choose")}</p>
          ) : current.own ? (
            <OwnForm
              key={`${tab}-${current.name}-${editor.formGen}`}
              tab={tab}
              name={current.name}
              data={data}
              editor={editor}
              limits={limits}
              usage={usage[current.name] ?? []}
              taken={takenNames}
              builtins={systemNames}
              maskSuggestions={maskSuggestions}
              samples={{ nameSamples, setNameSamples, maskSamples, setMaskSamples, date, setDate }}
              onRenamed={(to) => select({ own: true, name: to })}
              removing={removing}
              onRemoveTry={setRemoving}
              onRemoved={() => setSel((x) => ({ ...x, [tab]: null }))}
            />
          ) : (
            <BuiltinView
              key={`${tab}-${current.name}`}
              tab={tab}
              name={current.name}
              data={data}
              editor={editor}
              taken={takenNames}
              builtins={systemNames}
              samples={{ nameSamples, setNameSamples, maskSamples, setMaskSamples, date, setDate }}
              onCopied={(to) => select({ own: true, name: to })}
            />
          )}
        </aside>
      </div>
      <EditorBar editor={editor} />
    </div>
  );
}

interface Samples {
  nameSamples: string;
  setNameSamples: (s: string) => void;
  maskSamples: string;
  setMaskSamples: (s: string) => void;
  date: string | null;
  setDate: (d: string | null) => void;
}

function ItemButton({
  name,
  meta,
  pressed,
  bad,
  used,
  onClick,
}: {
  name: string;
  meta: string;
  pressed: boolean;
  bad?: boolean;
  used?: number;
  onClick: () => void;
}) {
  const { t } = useTranslation();
  return (
    <button type="button" className="target preset-item" aria-pressed={pressed} onClick={onClick}>
      <span className="preset-item-head">
        <span className="preset-name">{name}</span>
        {bad && <span className="flag bad" aria-label={t("flag_error")} />}
        {used !== undefined && used > 0 && (
          <span className="used" title={t("presets_used_title")}>
            ×{used}
          </span>
        )}
      </span>
      <span className="preset-meta">{meta}</span>
    </button>
  );
}

/** The built-in presets of a tab: bundles are their own tab, not part of "Content". */
function builtinOf(builtin: AppData["builtins"], tab: "content" | "bundles" | "name"): PresetInfo[] {
  if (!builtin) return [];
  if (tab === "name") return builtin.name;
  return builtin.content.filter((p) => (p.kind === "bundle") === (tab === "bundles"));
}

function groupByKind(presets: PresetInfo[], grouped: boolean): (readonly [string, PresetInfo[]])[] {
  if (!grouped) return [["", presets]];
  const order = KINDS.map((k) => k.id as string);
  const map = new Map<string, PresetInfo[]>();
  for (const p of presets) map.set(p.kind, [...(map.get(p.kind) ?? []), p]);
  const rank = (k: string) => (order.indexOf(k) < 0 ? order.length : order.indexOf(k));
  return [...map.entries()].sort(([a], [b]) => rank(a) - rank(b));
}

// ---------- built-in: read only, "Copy as own" ----------

function BuiltinView({
  tab,
  name,
  data,
  editor,
  taken,
  builtins,
  samples,
  onCopied,
}: {
  tab: Tab;
  name: string;
  data: AppData;
  editor: ConfigEditor;
  taken: string[];
  builtins: string[];
  samples: Samples;
  onCopied: (to: string) => void;
}) {
  const { t } = useTranslation();
  const kind = TABS.find((x) => x.id === tab)!.kind;
  const [to, setTo] = useState(freeKey(`my-${name}`, [...taken, ...builtins]));
  const [busy, setBusy] = useState(false);
  const err = newNameError(kind, to, taken, builtins);
  const info: PresetInfo | MaskInfo | undefined =
    tab === "masks" ? data.builtins?.masks.find((m) => m.name === name) : builtinOf(data.builtins, tab).find((p) => p.name === name);
  if (!info) return null;
  const copy = async () => {
    setBusy(true);
    const o = await editor.editNow({ op: "copy_builtin", kind, from: name, to });
    setBusy(false);
    if (o) onCopied(to);
  };
  return (
    <div className="detail form">
      <div className="form-head">
        <h2>
          <Code>{tab === "masks" ? `@${name}` : name}</Code>
        </h2>
        <span className="badge">{t("presets_badge_builtin")}</span>
      </div>
      {"description" in info ? (
        <>
          <p className="preset-desc">{info.description}</p>
          {tab === "bundles" ? (
            <BundleSteps steps={info.params.steps} builtins={data.builtins?.content ?? []} />
          ) : (
            <Field label={t("presets_files")}>
              <Code title={info.mask_expanded}>{info.mask}</Code>
              {info.mask !== info.mask_expanded && <span className="field-hint wrap-code">{info.mask_expanded}</span>}
            </Field>
          )}
          <h3>{t("presets_parameters")}</h3>
          <dl className="params">
            {Object.entries(info.params)
              .filter(([k]) => k !== "kind" && k !== "mask" && k !== "steps")
              .map(([k, val]) => (
                <div key={k} className="param">
                  <dt>{k}</dt>
                  <dd>
                    <code>{fmtParam(val)}</code>
                  </dd>
                </div>
              ))}
          </dl>
        </>
      ) : (
        <>
          <Field label={t("presets_as_written")}>
            <Code>{info.source}</Code>
          </Field>
          {info.expanded !== info.source && (
            <Field label={t("presets_expanded")}>
              <span className="wrap-code">
                <Code>{info.expanded}</Code>
              </span>
            </Field>
          )}
        </>
      )}
      <div className="copy-box">
        <p className="copy-title">{t("presets_copy_as_own")}</p>
        <p className="field-hint">
          {tab === "masks" ? t("presets_copy_note_mask") : t("presets_copy_note_preset")}
        </p>
        <div className="input-row">
          <TextInput label={t("presets_copy_name")} value={to} mono invalid={!!err} onChange={setTo} />
          <button type="button" className="btn primary" disabled={!!err || busy} onClick={() => void copy()}>
            {t("copy")}
          </button>
        </div>
        <Issues items={[]} error={err} />
      </div>
      {tab === "name" && <NamePreview editor={editor} configPath={data.configPath} preset={name} samples={samples} />}
      {tab === "masks" && <MaskPreview editor={editor} configPath={data.configPath} items={[`@${name}`]} samples={samples} />}
    </div>
  );
}

/** The expansion of a built-in bundle: each step with the mask it runs on. */
function BundleSteps({ steps, builtins }: { steps: unknown; builtins: PresetInfo[] }) {
  const { t } = useTranslation();
  const list = (Array.isArray(steps) ? steps : []).map((s) =>
    typeof s === "string" ? { preset: s, mask: null as string | null } : { preset: String(s.preset), mask: Array.isArray(s.mask) ? s.mask.join(" | ") : String(s.mask) },
  );
  return (
    <Field label={t("presets_steps_in_order")}>
      <ol className="bundle-steps">
        {list.map((s, i) => (
          <li key={i}>
            <Code>{s.preset}</Code> <code>{s.mask ?? builtins.find((b) => b.name === s.preset)?.mask ?? "*"}</code>
          </li>
        ))}
      </ol>
    </Field>
  );
}

function fmtParam(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (Array.isArray(v)) return v.join(" | ");
  return String(v);
}

// ---------- own: typed forms ----------

function OwnForm({
  tab,
  name,
  data,
  editor,
  limits,
  usage,
  taken,
  builtins,
  maskSuggestions,
  samples,
  onRenamed,
  removing,
  onRemoveTry,
  onRemoved,
}: {
  tab: Tab;
  name: string;
  data: AppData;
  editor: ConfigEditor;
  limits: Limit[];
  usage: string[];
  taken: string[];
  builtins: string[];
  maskSuggestions: string[];
  samples: Samples;
  onRenamed: (to: string) => void;
  removing: string | null;
  onRemoveTry: (name: string) => void;
  onRemoved: () => void;
}) {
  const { t } = useTranslation();
  const meta = TABS.find((x) => x.id === tab)!;
  const d = editor.view!.draft;
  const err = editor.view!.error;
  const base = `${meta.path}.${name}`;
  const serverErr = (key?: string) => {
    if (!err?.path) return null;
    if (key ? err.path === `${base}.${key}` : err.path === base) return err.message;
    return null;
  };
  const remove = async () => {
    onRemoveTry(name);
    const op: EditOp =
      tab === "content" || tab === "bundles"
        ? { op: "remove_content_preset", name }
        : tab === "name"
          ? { op: "remove_name_preset", name }
          : { op: "remove_mask", name };
    const o = await editor.editNow(op);
    const still =
      o &&
      (tab === "content" || tab === "bundles"
        ? o.draft.content_presets?.some((p) => p.name === name)
        : tab === "name"
          ? o.draft.name_presets?.some((p) => p.name === name)
          : o.draft.mask_defs?.some((m) => m.name === name));
    if (o && !still) onRemoved();
  };

  // A refused removal names this item: show it here too, not only above the lists.
  const notice = editor.notice?.tone === "bad" ? editor.notice.text : null;
  const refusal = notice && removing === name ? notice : null;
  // The core's warnings about this preset (e.g. folders together with skip_if).
  const own = editor.view!.warnings.filter((w) => w.path === base);

  let body: ReactNode = null;
  if (tab === "bundles") {
    const p = d.content_presets?.find((x) => x.name === name);
    if (p) body = <BundleForm preset={p} editor={editor} data={data} serverErr={serverErr} maskSuggestions={maskSuggestions} />;
  } else if (tab === "content") {
    const p = d.content_presets?.find((x) => x.name === name);
    if (p) body = <ContentForm preset={p} editor={editor} limits={limits} serverErr={serverErr} maskSuggestions={maskSuggestions} />;
  } else if (tab === "name") {
    const p = d.name_presets?.find((x) => x.name === name);
    if (p)
      body = (
        <NameForm preset={p} editor={editor} configPath={data.configPath} serverErr={serverErr} maskSuggestions={maskSuggestions} samples={samples} />
      );
  } else {
    const m = d.mask_defs?.find((x) => x.name === name);
    if (m)
      body = <MaskForm mask={m} editor={editor} configPath={data.configPath} serverErr={serverErr} maskSuggestions={maskSuggestions} samples={samples} />;
  }

  return (
    <div className="detail form">
      <div className="form-head">
        <h2>
          <Code>{tab === "masks" ? `@${name}` : name}</Code>
        </h2>
        <span className="badge own">{t("presets_badge_own")}</span>
      </div>
      <Issues items={own} error={serverErr()} />
      <RenameRow kind={meta.kind} name={name} usage={usage.length} taken={taken} builtins={builtins} editor={editor} onRenamed={onRenamed} />
      <div className="usage">
        {usage.length === 0 ? (
          <span className="muted">{t("presets_unused")}</span>
        ) : (
          <>
            <span className="muted">{t("presets_used_by")}</span>
            <ul>
              {usage.map((u) => (
                <li key={u}>{u}</li>
              ))}
            </ul>
          </>
        )}
      </div>
      {body}
      {refusal && <p className="bad-text small">{refusal}</p>}
      <div className="form-actions">
        <button
          type="button"
          className="btn quiet danger"
          title={usage.length ? t("presets_remove_refs") : undefined}
          onClick={() => void remove()}
        >
          {tab === "masks" ? t("presets_delete_mask") : tab === "bundles" ? t("presets_delete_set") : t("presets_delete_preset")}
        </button>
      </div>
    </div>
  );
}

function RenameRow({
  kind,
  name,
  usage,
  taken,
  builtins,
  editor,
  onRenamed,
}: {
  kind: PresetKind;
  name: string;
  usage: number;
  taken: string[];
  builtins: string[];
  editor: ConfigEditor;
  onRenamed: (to: string) => void;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [to, setTo] = useState(name);
  const [refs, setRefs] = useState(true);
  const err = to === name ? null : newNameError(kind, to, taken.filter((x) => x !== name), builtins);
  if (!open)
    return (
      <button type="button" className="link small" onClick={() => setOpen(true)}>
        {t("rename_open")}
      </button>
    );
  const go = async () => {
    const o = await editor.editNow({ op: "rename_preset", kind, from: name, to, update_refs: refs });
    if (o) {
      setOpen(false);
      onRenamed(to);
    }
  };
  return (
    <div className="rename-box">
      <div className="input-row">
        <TextInput label={t("rename_new_name")} value={to} mono invalid={!!err} onChange={setTo} />
        <button type="button" className="btn" disabled={!!err || to === name} onClick={() => void go()}>
          {t("rename_do")}
        </button>
        <button type="button" className="btn quiet" onClick={() => setOpen(false)}>
          {t("cancel")}
        </button>
      </div>
      <Issues items={[]} error={err} />
      {usage > 0 && (
        <label className="check">
          <input type="checkbox" checked={refs} onChange={(e) => setRefs(e.target.checked)} />
          <span>{t("rename_update_refs", { n: usage })}</span>
        </label>
      )}
    </div>
  );
}

/** An optional mask: "own" or the default shown. */
function OptionalMask({
  label,
  value,
  fallback,
  onChange,
  suggestions,
  error,
  offText,
  allowNot = true,
}: {
  label: string;
  value: string[] | null;
  fallback: string | null;
  onChange: (v: string[] | null) => void;
  suggestions: string[];
  error: string | null;
  offText: string;
  allowNot?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <Field label={label}>
      <label className="check">
        <input type="checkbox" checked={value !== null} onChange={(e) => onChange(e.target.checked ? [fallback ?? "*"] : null)} />
        <span>
          {value === null ? (
            <>
              {offText}
              {fallback && (
                <>
                  {" "}
                  <code>{fallback}</code>
                </>
              )}
            </>
          ) : (
            t("optmask_own")
          )}
        </span>
      </label>
      {value !== null && <MaskChips value={value} onChange={onChange} suggestions={suggestions} label={label} invalid={!!error} allowNot={allowNot} exceptionField={t("field_mask")} />}
      <Issues items={[]} error={error} />
    </Field>
  );
}

/** Every content preset a bundle step may name: the user's own and the built-in ones. */
function stepChoices(data: AppData, self: string): PresetChoice[] {
  const pick = (list: PresetInfo[], own: boolean): PresetChoice[] =>
    list
      .filter((p) => p.name !== self)
      .map((p) => ({ name: p.name, mask: p.mask, description: p.description, own, bundle: p.kind === "bundle" }));
  return [...pick(data.summary?.presets.content ?? [], true), ...pick(data.builtins?.content ?? [], false)];
}

function BundleForm({
  preset,
  editor,
  data,
  serverErr,
  maskSuggestions,
}: {
  preset: ContentPresetDraft;
  editor: ConfigEditor;
  data: AppData;
  serverErr: (key?: string) => string | null;
  maskSuggestions: string[];
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState(preset);
  const choices = stepChoices(data, preset.name);
  const local = validateBundle(form, choices.map((c) => c.name));
  const update = (next: ContentPresetDraft) => {
    setForm(next);
    editor.edit({ op: "update_content_preset", name: preset.name, preset: next });
  };
  return (
    <>
      <h3>{t("bundle_steps")}</h3>
      <p className="field-hint">{t("bundle_hint")}</p>
      <PipeEditor
        label={t("bundle_steps_of", { name: preset.name })}
        steps={form.steps ?? []}
        presets={choices}
        maskSuggestions={maskSuggestions}
        empty={t("bundle_empty")}
        onChange={(steps) => update({ ...form, steps })}
      />
      <Issues items={[]} error={local.steps ?? local.mask ?? serverErr("steps") ?? serverErr("mask")} />
    </>
  );
}

function ContentForm({
  preset,
  editor,
  limits,
  serverErr,
  maskSuggestions,
}: {
  preset: ContentPresetDraft;
  editor: ConfigEditor;
  limits: Limit[];
  serverErr: (key?: string) => string | null;
  maskSuggestions: string[];
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState(preset);
  const local = validateContent(form, limits);
  const update = (next: ContentPresetDraft) => {
    setForm(next);
    editor.edit({ op: "update_content_preset", name: preset.name, preset: next });
  };
  const errOf = (k: string) => local[k] ?? serverErr(k);
  return (
    <>
      <h3>{t("content_kind")}</h3>
      <div className="seg" role="radiogroup" aria-label={t("content_kind_aria")}>
        {KINDS.map((k) => (
          <button
            key={k.id}
            type="button"
            role="radio"
            className="seg-item"
            aria-checked={form.kind === k.id}
            onClick={() => form.kind !== k.id && update(switchKind(form, k.id))}
          >
            {t(k.title)}
          </button>
        ))}
      </div>
      <Issues items={[]} error={serverErr("kind")} />
      <h3>{t("presets_parameters")}</h3>
      {fieldsOf(form.kind).map((f) => (
        <ContentField key={f.key} spec={f} preset={form} limit={limitFor(limits, form, f.key)} error={errOf(f.key)} onChange={update} />
      ))}
      <h3>{t("presets_files")}</h3>
      <OptionalMask
        label={t("field_mask")}
        value={form.mask}
        fallback={KIND_MASK[form.kind as ContentKind] ?? null}
        onChange={(mask) => update({ ...form, mask })}
        suggestions={maskSuggestions}
        error={errOf("mask")}
        offText={t("content_default_mask")}
      />
    </>
  );
}

function ContentField({
  spec,
  preset,
  limit,
  error,
  onChange,
}: {
  spec: FieldSpec;
  preset: ContentPresetDraft;
  limit: Limit | undefined;
  error: string | null;
  onChange: (p: ContentPresetDraft) => void;
}) {
  const { t } = useTranslation();
  const v = preset[spec.key];
  const dflt = coreDefault(preset.kind, spec.key);
  const set = (val: unknown) => onChange({ ...preset, [spec.key]: val });
  let input: ReactNode;
  if (spec.type === "bool") {
    const on = (v ?? dflt) === true;
    input = (
      <label className="check">
        <input type="checkbox" checked={on} aria-label={t(spec.label)} onChange={(e) => set(e.target.checked)} />
        <span>{spec.hint ? t(spec.hint) : on ? t("yes") : t("no")}</span>
      </label>
    );
  } else if (spec.type === "choice") {
    input = (
      <select className="select field-select" aria-label={t(spec.label)} value={(v as string | null) ?? ""} onChange={(e) => set(e.target.value || null)}>
        {!spec.choices!.some((c) => c.value === "") && v == null && <option value="">{dflt ? t("field_default_value", { value: String(dflt) }) : t("field_default")}</option>}
        {spec.choices!.map((c) => (
          <option key={c.value} value={c.value}>
            {t(c.label)}
          </option>
        ))}
      </select>
    );
  } else {
    input = (
      <div className="num-row">
        <input
          className="input num"
          type="number"
          step={1}
          min={limit?.min}
          max={limit?.max}
          aria-label={t(spec.label)}
          aria-invalid={!!error || undefined}
          value={v === null || v === undefined ? "" : String(v)}
          placeholder={dflt === null ? "—" : String(dflt)}
          onChange={(e) => set(e.target.value === "" ? null : Number(e.target.value))}
        />
        {limit && (
          <span className="range" aria-hidden>
            {limit.min}–{limit.max}
          </span>
        )}
      </div>
    );
  }
  return (
    <Field label={t(spec.label)} hint={spec.type !== "bool" && spec.hint ? t(spec.hint) : undefined}>
      {input}
      <Issues items={[]} error={error} />
    </Field>
  );
}

const DATE_SOURCES: { value: string; label: MessageKey }[] = [
  { value: "capture", label: "fact_capture" },
  { value: "file_created", label: "date_file" },
  { value: "none", label: "date_none" },
];

function NameForm({
  preset,
  editor,
  configPath,
  serverErr,
  maskSuggestions,
  samples,
}: {
  preset: NamePresetDraft;
  editor: ConfigEditor;
  configPath: string | null;
  serverErr: (key?: string) => string | null;
  maskSuggestions: string[];
  samples: Samples;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState(preset);
  const local = validateName(form);
  const errOf = (k: string) => local[k] ?? serverErr(k);
  const update = (next: NamePresetDraft) => {
    setForm(next);
    editor.edit({ op: "update_name_preset", name: preset.name, preset: next });
  };
  const date = form.date ?? "capture";
  return (
    <>
      <h3>{t("name_template")}</h3>
      <TextInput label={t("name_template_aria")} value={form.template} mono invalid={!!errOf("template")} onChange={(template) => update({ ...form, template })} />
      <p className="field-hint">
        <code>{"{date:YYYYMMDD_HHMMSS}"}</code> <code>{"{stem}"}</code> <code>{"{ext}"}</code> <code>{"{source}"}</code>; {t("name_template_folders")}
      </p>
      <Issues items={[]} error={errOf("template")} />
      <Field label={t("name_date")}>
        <div className="seg" role="radiogroup" aria-label={t("name_date_source")}>
          {DATE_SOURCES.map((s) => (
            <button key={s.value} type="button" role="radio" className="seg-item" aria-checked={date === s.value} onClick={() => update({ ...form, date: s.value })}>
              {t(s.label)}
            </button>
          ))}
        </div>
        <Issues items={[]} error={errOf("date")} />
      </Field>
      <Field label={t("name_extension")}>
        <label className="check">
          <input type="checkbox" checked={form.normalize_ext ?? false} onChange={(e) => update({ ...form, normalize_ext: e.target.checked })} />
          <span>{t("name_lower")}</span>
        </label>
      </Field>
      <OptionalMask
        label={t("name_leave_alone")}
        value={form.skip_if}
        fallback="@dated"
        onChange={(skip_if) => update({ ...form, skip_if })}
        suggestions={maskSuggestions}
        error={errOf("skip_if")}
        offText={t("name_apply_all")}
        allowNot={false}
      />
      <OptionalMask
        label={t("field_mask")}
        value={form.mask}
        fallback="*"
        onChange={(mask) => update({ ...form, mask })}
        suggestions={maskSuggestions}
        error={errOf("mask")}
        offText={t("name_all_files")}
      />
      <NamePreview editor={editor} configPath={configPath} preset={form} samples={samples} invalid={Object.keys(local).length > 0} />
    </>
  );
}

function MaskForm({
  mask,
  editor,
  configPath,
  serverErr,
  maskSuggestions,
  samples,
}: {
  mask: MaskDraft;
  editor: ConfigEditor;
  configPath: string | null;
  serverErr: (key?: string) => string | null;
  maskSuggestions: string[];
  samples: Samples;
}) {
  const { t } = useTranslation();
  const [items, setItems] = useState(mask.items);
  const err = validateMask(items) ?? serverErr();
  const update = (next: string[]) => {
    setItems(next);
    editor.edit({ op: "update_mask", name: mask.name, mask: { name: mask.name, items: next } });
  };
  return (
    <>
      <h3>{t("mask_variants")} <span className="muted small">— {t("mask_variants_hint")}</span></h3>
      <MaskChips
        value={items}
        onChange={update}
        suggestions={maskSuggestions.filter((s) => s !== `@${mask.name}`)}
        label={t("mask_label", { name: mask.name })}
        invalid={!!err}
      />
      <Issues items={[]} error={err} />
      {!validateMask(items) && <MaskPreview editor={editor} configPath={configPath} items={items} samples={samples} />}
    </>
  );
}

// ---------- live previews ----------

/** Re-run `run` 250 ms after `deps` last changed; the newest answer wins. */
function useLive<T>(run: () => Promise<T>, deps: unknown[]): { result: T | null; error: string | null } {
  const [state, setState] = useState<{ result: T | null; error: string | null }>({ result: null, error: null });
  useEffect(() => {
    let live = true;
    const t = setTimeout(() => {
      run().then(
        (r) => live && setState({ result: r, error: null }),
        (e) => live && setState({ result: null, error: errorText(e) }),
      );
    }, 250);
    return () => {
      live = false;
      clearTimeout(t);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return state;
}

const STATUS: Record<NameResult["status"], { mark: string; text: MessageKey | null; tone: string }> = {
  renamed: { mark: "→", text: null, tone: "yes" },
  same: { mark: "=", text: "preview_same", tone: "no" },
  not_matched: { mark: "–", text: "preview_not_matched", tone: "no" },
  skipped: { mark: "–", text: "preview_skipped", tone: "no" },
  no_date: { mark: "–", text: "preview_no_date", tone: "no" },
};

function NamePreview({
  editor,
  configPath,
  preset,
  samples,
  invalid,
}: {
  editor: ConfigEditor;
  configPath: string | null;
  preset: string | NamePresetDraft;
  samples: Samples;
  invalid?: boolean;
}) {
  const { t } = useTranslation();
  const names = parseSamples(samples.nameSamples);
  const key = JSON.stringify(preset);
  const { result, error } = useLive(
    () =>
      invalid
        ? Promise.resolve(null)
        : editorApi.previewName(configPath, editor.currentOps(), {
            preset,
            names,
            date: samples.date ? samples.date.replace("T", " ") : null,
            source: null,
          }),
    [key, samples.nameSamples, samples.date, configPath, invalid],
  );
  return (
    <section className="preview" aria-label={t("preview_title")}>
      <h3>{t("preview_title")}</h3>
      <div className="preview-date">
        <label className="check">
          <input type="checkbox" checked={samples.date !== null} onChange={(e) => samples.setDate(e.target.checked ? "2026-11-05T10:15:00" : null)} />
          <span>{t("preview_has_date")}</span>
        </label>
        {samples.date !== null && (
          <input
            className="input date"
            type="datetime-local"
            step={1}
            aria-label={t("preview_date_aria")}
            value={samples.date}
            onChange={(e) => e.target.value && samples.setDate(e.target.value)}
          />
        )}
      </div>
      <textarea
        className="input mono"
        aria-label={t("tester_names")}
        rows={Math.min(8, Math.max(3, names.length + 1))}
        value={samples.nameSamples}
        spellCheck={false}
        onChange={(e) => samples.setNameSamples(e.target.value)}
      />
      {error && <p className="bad-text small">{error}</p>}
      {result && (
        <ol className="preview-rows">
          {result.map((r, i) => {
            const s = STATUS[r.status];
            return (
              <li key={`${r.name}-${i}`} className={s.tone}>
                <code className="from">{r.name}</code>
                <span className="mark" aria-hidden>
                  {s.mark}
                </span>
                {r.status === "renamed" ? <code className="to">{r.result}</code> : <span className="muted">{s.text ? t(s.text) : ""}</span>}
              </li>
            );
          })}
        </ol>
      )}
    </section>
  );
}

function MaskPreview({
  editor,
  configPath,
  items,
  samples,
}: {
  editor: ConfigEditor;
  configPath: string | null;
  items: string[];
  samples: Samples;
}) {
  const { t } = useTranslation();
  const names = parseSamples(samples.maskSamples);
  const { result, error } = useLive<MaskTestResult[]>(
    () => editorApi.maskTest(configPath, editor.currentOps(), { match: items, exclude: [], names }),
    [items.join("\n"), samples.maskSamples, configPath],
  );
  const hits = result?.filter((r) => r.taken).length ?? 0;
  return (
    <section className="preview" aria-label={t("preview_title")}>
      <h3>
        {t("preview_title")} {result && <span className="muted small">— {t("preview_hits", { hits, total: result.length })}</span>}
      </h3>
      <textarea
        className="input mono"
        aria-label={t("preview_mask_names")}
        rows={Math.min(8, Math.max(3, names.length + 1))}
        value={samples.maskSamples}
        spellCheck={false}
        onChange={(e) => samples.setMaskSamples(e.target.value)}
      />
      {error && <p className="bad-text small">{error}</p>}
      {result && (
        <ul className="preview-rows">
          {result.map((r, i) => (
            <li key={`${r.name}-${i}`} className={r.taken ? "yes" : "no"}>
              <span className="mark" aria-label={r.taken ? t("tester_matches") : t("tester_no_match")}>
                {r.taken ? "✓" : "✗"}
              </span>
              <code className="from">{r.name}</code>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
