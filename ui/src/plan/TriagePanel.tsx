import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import { errorText } from "../api";
import { asFailure, editorApi } from "../editor/api";
import { Field, FolderInput, TextInput } from "../editor/fields";
import { baseName } from "./model";
import { Thumb } from "./Thumb";
import type { ThumbQueue } from "./thumbQueue";
import {
  blocker,
  extLabel,
  formatSize,
  groupId,
  placeLabel,
  triageApi,
  type ChangePreview,
  type SuggestedAction,
  type TriageGroup,
  type TriageView,
} from "./triage";

interface Active {
  gid: string;
  group: TriageGroup;
  action: SuggestedAction;
  preview: ChangePreview | null;
  error: string | null;
  /** The config file changed after the triage read it: refresh, then try again. */
  stale?: boolean;
  saving: boolean;
}

/// "Unmatched" grouped by folder and extension, with the reason for each
/// group and the config edits that deal with it. An edit is previewed as the
/// exact TOML change and saved only on the click; a config changed meanwhile
/// is refused.
export function TriagePanel({
  configPath,
  version,
  queue,
  onConfigSaved,
}: {
  configPath: string | null;
  /** Changes when the current plan is rebuilt: the groups are read again. */
  version: string;
  queue: ThumbQueue;
  /** The config was saved: the plan is stale now. */
  onConfigSaved: () => void;
}) {
  const { t } = useTranslation();
  const [view, setView] = useState<TriageView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [active, setActive] = useState<Active | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [gen, setGen] = useState(0);
  const seq = useRef(0);

  useEffect(() => {
    let live = true;
    triageApi.view(configPath).then(
      (v) => {
        if (!live) return;
        setView(v);
        setError(null);
      },
      (e) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [configPath, version, gen]);

  const preview = useCallback(
    async (gid: string, group: TriageGroup, action: SuggestedAction, version: string) => {
      const my = ++seq.current;
      setActive({ gid, group, action, preview: null, error: null, saving: false });
      try {
        const p = await triageApi.preview(configPath, version, action.ops);
        if (my === seq.current) setActive({ gid, group, action, preview: p, error: null, saving: false });
      } catch (e) {
        if (my === seq.current) {
          const f = asFailure(e);
          setActive({ gid, group, action, preview: null, error: f.message, stale: f.kind === "changed", saving: false });
        }
      }
    },
    [configPath],
  );

  const save = async () => {
    if (!active || !view || blocker(active.preview)) return;
    setActive({ ...active, saving: true });
    try {
      const out = await editorApi.save(configPath, view.version, active.action.ops);
      if (!out.saved) {
        setActive({ ...active, saving: false, error: out.error?.message ?? t("triage_unchanged") });
        return;
      }
      setActive(null);
      setSaved(t("triage_saved", { label: active.action.label }));
      onConfigSaved();
      setGen((g) => g + 1);
    } catch (e) {
      const f = asFailure(e);
      setActive({ ...active, saving: false, error: f.message, stale: f.kind === "changed" });
    }
  };

  if (error) {
    return (
      <div className="notice bad">
        <p className="notice-title">{t("triage_open_failed")}</p>
        <pre className="notice-text">{error}</pre>
      </div>
    );
  }
  if (!view) return <p className="empty">{t("triage_loading")}</p>;
  if (view.groups.length === 0) return <p className="empty">{t("triage_none")}</p>;

  return (
    <div className="triage">
      {saved && <p className="notice ok triage-saved">{saved}</p>}
      <table className="triage-table">
        <thead>
          <tr>
            <th>{t("triage_col_folder")}</th>
            <th>{t("triage_col_ext")}</th>
            <th className="num">{t("triage_col_files")}</th>
            <th className="num">{t("triage_col_size")}</th>
            <th>{t("triage_col_why")}</th>
          </tr>
        </thead>
        <tbody>
          {view.groups.map((g) => {
            const gid = groupId(g);
            const isOpen = open === gid;
            return (
              <Fragment key={gid}>
                <tr className={isOpen ? "open" : undefined}>
                  <td>
                    <button
                      type="button"
                      className="triage-toggle"
                      aria-expanded={isOpen}
                      aria-label={`${placeLabel(g)} ${extLabel(g.ext)}`}
                      onClick={() => {
                        setOpen(isOpen ? null : gid);
                        setActive(null);
                      }}
                    >
                      <span aria-hidden>{isOpen ? "▾" : "▸"}</span> {placeLabel(g)}
                    </button>
                  </td>
                  <td className="mono">{extLabel(g.ext)}</td>
                  <td className="num">{num(g.count)}</td>
                  <td className="num">{formatSize(g.size)}</td>
                  <td className="triage-reason">{g.reason}</td>
                </tr>
                {isOpen && (
                  <tr className="triage-detail">
                    <td colSpan={5}>
                      <ul className="triage-samples" aria-label={t("triage_examples")}>
                        {g.samples.map((p) => (
                          <li key={p} title={p}>
                            <span className="triage-thumb">
                              <Thumb path={p} queue={queue} />
                            </span>
                            <span className="triage-name">{baseName(p)}</span>
                          </li>
                        ))}
                        {g.count > g.samples.length && (
                          <li className="muted small">{t("triage_and_more", { n: g.count - g.samples.length })}</li>
                        )}
                      </ul>
                      <Actions
                        group={g}
                        view={view}
                        configPath={configPath}
                        active={active?.gid === gid ? active : null}
                        onPick={(a) => void preview(gid, g, a, view.version)}
                        onCancel={() => setActive(null)}
                        onSave={() => void save()}
                        onRefresh={() => {
                          setActive(null);
                          setGen((n) => n + 1);
                        }}
                      />
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function Actions({
  group,
  view,
  configPath,
  active,
  onPick,
  onCancel,
  onSave,
  onRefresh,
}: {
  group: TriageGroup;
  view: TriageView;
  configPath: string | null;
  active: Active | null;
  onPick: (a: SuggestedAction) => void;
  onCancel: () => void;
  onSave: () => void;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  const adds = group.actions.filter((a) => a.kind === "add_to_target");
  const others = group.actions.filter((a) => a.kind !== "add_to_target" && a.kind !== "create_target");
  const create = group.actions.find((a) => a.kind === "create_target");
  const [addIdx, setAddIdx] = useState(0);
  if (group.actions.length === 0) {
    return <p className="muted small">{t("triage_no_suggestions")}</p>;
  }
  return (
    <div className="triage-actions">
      {adds.length > 0 && (
        <div className="triage-add">
          <select aria-label={t("triage_target")} value={addIdx} onChange={(e) => setAddIdx(Number(e.target.value))}>
            {adds.map((a, i) => (
              <option key={a.label} value={i}>
                {a.target}
              </option>
            ))}
          </select>
          <button type="button" className="btn" onClick={() => onPick(adds[addIdx])}>
            {t("triage_add_to_target")}
          </button>
        </div>
      )}
      {others.map((a) => (
        <button key={a.kind} type="button" className="btn" onClick={() => onPick(a)}>
          {a.kind === "skip_folder" ? t("triage_ignore_folder") : t("triage_ignore_source")}
        </button>
      ))}
      {create && (
        <button type="button" className="btn" onClick={() => onPick(create)}>
          {t("triage_create_target")}
        </button>
      )}
      {active && (
        <ChangeView
          key={active.action.kind === "create_target" ? "create" : active.action.kind + (active.action.target ?? "")}
          active={active}
          view={view}
          configPath={configPath}
          onPick={onPick}
          onCancel={onCancel}
          onSave={onSave}
          onRefresh={onRefresh}
        />
      )}
    </div>
  );
}

function ChangeView({
  active,
  view,
  configPath,
  onPick,
  onCancel,
  onSave,
  onRefresh,
}: {
  active: Active;
  view: TriageView;
  configPath: string | null;
  onPick: (a: SuggestedAction) => void;
  onCancel: () => void;
  onSave: () => void;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  const { action, preview, error } = active;
  const creating = action.kind === "create_target";
  const [name, setName] = useState(action.target ?? "");
  const [dir, setDir] = useState(action.dir ?? "");
  const retry = useRef(0);
  // Typing in the form asks Rust for the operation again, then previews it.
  const edit = (n: string, d: string) => {
    setName(n);
    setDir(d);
    const my = ++retry.current;
    triageApi.createOps(configPath, view.version, active.group, n, d).then(
      (a) => my === retry.current && onPick(a),
      () => {},
    );
  };
  const block = blocker(preview);
  const stale = !!active.stale;
  return (
    <section className="triage-change" aria-label={t("triage_change_aria")}>
      {creating && (
        <div className="triage-form">
          <Field label={t("triage_target_name")}>
            <TextInput label={t("triage_target_name")} value={name} onChange={(v) => edit(v, dir)} />
          </Field>
          <Field label={t("triage_col_folder")} hint={t("triage_folder_hint")}>
            <FolderInput label={t("triage_target_folder")} value={dir} onChange={(v) => edit(name, v)} />
          </Field>
        </div>
      )}
      <p className="small muted">{t("triage_will_change")}</p>
      {preview && (
        <pre className="diff" aria-label={t("triage_toml_aria")}>
          {preview.diff.map((l, i) => (
            <span key={i} className={`diff-${l.kind}`}>
              {l.kind === "add" ? "+ " : l.kind === "del" ? "- " : "  "}
              {l.text}
              {"\n"}
            </span>
          ))}
        </pre>
      )}
      {preview?.warnings.map((w) => (
        <p key={w.path + w.message} className="small muted">
          {w.message}
        </p>
      ))}
      {error && <p className="bad-text">{error}</p>}
      {preview && block && !error && <p className="bad-text">{block}</p>}
      <div className="triage-buttons">
        <button type="button" className="btn primary" disabled={!!block || active.saving} onClick={onSave}>
          {t("triage_save")}
        </button>
        <button type="button" className="btn quiet" onClick={onCancel}>
          {t("cancel")}
        </button>
        {stale && (
          <button type="button" className="btn" onClick={onRefresh}>
            {t("triage_refresh")}
          </button>
        )}
      </div>
    </section>
  );
}
