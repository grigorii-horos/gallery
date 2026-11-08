import { Fragment, useCallback, useEffect, useRef, useState } from "react";
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
        if (my === seq.current) setActive({ gid, group, action, preview: null, error: asFailure(e).message, saving: false });
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
        setActive({ ...active, saving: false, error: out.error?.message ?? "The config did not change" });
        return;
      }
      setActive(null);
      setSaved(`Saved to the config: ${active.action.label}. The previous version is next to it as .bak.`);
      onConfigSaved();
      setGen((g) => g + 1);
    } catch (e) {
      setActive({ ...active, saving: false, error: asFailure(e).message });
    }
  };

  if (error) {
    return (
      <div className="notice bad">
        <p className="notice-title">The triage did not open</p>
        <pre className="notice-text">{error}</pre>
      </div>
    );
  }
  if (!view) return <p className="empty">Triaging unmatched files…</p>;
  if (view.groups.length === 0) return <p className="empty">No unmatched files.</p>;

  return (
    <div className="triage">
      {saved && <p className="notice ok triage-saved">{saved}</p>}
      <table className="triage-table">
        <thead>
          <tr>
            <th>Folder</th>
            <th>Extension</th>
            <th className="num">Files</th>
            <th className="num">Size</th>
            <th>Why unmatched</th>
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
                  <td className="num">{g.count.toLocaleString("en-US")}</td>
                  <td className="num">{formatSize(g.size)}</td>
                  <td className="triage-reason">{g.reason}</td>
                </tr>
                {isOpen && (
                  <tr className="triage-detail">
                    <td colSpan={5}>
                      <ul className="triage-samples" aria-label="Example files">
                        {g.samples.map((p) => (
                          <li key={p} title={p}>
                            <span className="triage-thumb">
                              <Thumb path={p} queue={queue} />
                            </span>
                            <span className="triage-name">{baseName(p)}</span>
                          </li>
                        ))}
                        {g.count > g.samples.length && (
                          <li className="muted small">…and {(g.count - g.samples.length).toLocaleString("en-US")} more</li>
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
  const adds = group.actions.filter((a) => a.kind === "add_to_target");
  const others = group.actions.filter((a) => a.kind !== "add_to_target" && a.kind !== "create_target");
  const create = group.actions.find((a) => a.kind === "create_target");
  const [addIdx, setAddIdx] = useState(0);
  if (group.actions.length === 0) {
    return <p className="muted small">No changes are suggested for files without an extension: edit the config by hand.</p>;
  }
  return (
    <div className="triage-actions">
      {adds.length > 0 && (
        <div className="triage-add">
          <select aria-label="Target" value={addIdx} onChange={(e) => setAddIdx(Number(e.target.value))}>
            {adds.map((a, i) => (
              <option key={a.label} value={i}>
                {a.target}
              </option>
            ))}
          </select>
          <button type="button" className="btn" onClick={() => onPick(adds[addIdx])}>
            Add to target…
          </button>
        </div>
      )}
      {others.map((a) => (
        <button key={a.kind} type="button" className="btn" onClick={() => onPick(a)}>
          {a.kind === "skip_folder" ? "Always ignore in this folder" : "Always ignore in the whole source"}
        </button>
      ))}
      {create && (
        <button type="button" className="btn" onClick={() => onPick(create)}>
          Create target…
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
  const stale = error?.includes("changed");
  return (
    <section className="triage-change" aria-label="Config change">
      {creating && (
        <div className="triage-form">
          <Field label="Target name">
            <TextInput label="Target name" value={name} onChange={(v) => edit(v, dir)} />
          </Field>
          <Field label="Folder" hint="By default files stay in place and are only renamed.">
            <FolderInput label="Target folder" value={dir} onChange={(v) => edit(name, v)} />
          </Field>
        </div>
      )}
      <p className="small muted">The config will change like this:</p>
      {preview && (
        <pre className="diff" aria-label="TOML change">
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
          Save to config
        </button>
        <button type="button" className="btn quiet" onClick={onCancel}>
          Cancel
        </button>
        {stale && (
          <button type="button" className="btn" onClick={onRefresh}>
            Refresh triage
          </button>
        )}
      </div>
    </section>
  );
}
