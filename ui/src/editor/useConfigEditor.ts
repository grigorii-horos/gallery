// The config editor's state, shared by the Sources and Targets screens:
// the saved file, the pending operations and the checked draft after them.
import { t } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { asFailure, editorApi } from "./api";
import { isDirty, pushOp, type EditFailure, type EditOp, type EditOutcome, type TargetRename } from "./model";

export interface ConfigEditor {
  /** The draft after the pending operations (the saved file when none). */
  view: EditOutcome | null;
  ops: EditOp[];
  dirty: boolean;
  checking: boolean;
  saving: boolean;
  /** The file cannot be edited, changed outside, or could not be written. */
  failure: EditFailure | null;
  /** A one-line note: a rejected operation, a successful save. */
  notice: { tone: "ok" | "bad"; text: string } | null;
  /** Bumped when forms must start again from the draft (structural edit, save, discard). */
  formGen: number;
  /** A field edit: checked after a short pause. */
  edit: (op: EditOp) => void;
  /** An add/remove/move: checked now; resolves with the new draft. */
  editNow: (op: EditOp) => Promise<EditOutcome | null>;
  save: () => Promise<void>;
  discard: () => void;
  /** Read the file again, dropping pending operations. */
  reload: () => void;
  /** Saved renames that manual decisions (overrides) still point at the old name of. */
  retargetable: TargetRename[];
  /** Move those decisions to the new names. */
  retarget: () => Promise<void>;
  dismissRetarget: () => void;
  /** The pending operations as of now (for previews and the mask tester). */
  currentOps: () => EditOp[];
}

const PAUSE_MS = 200;

export function useConfigEditor(configPath: string | null, onSaved: () => void): ConfigEditor {
  const [base, setBase] = useState<EditOutcome | null>(null);
  const [view, setViewState] = useState<EditOutcome | null>(null);
  // The operations `view` was checked with.
  const [viewOps, setViewOps] = useState<EditOp[]>([]);
  const [ops, setOpsState] = useState<EditOp[]>([]);
  const [checking, setChecking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<EditFailure | null>(null);
  const [notice, setNotice] = useState<ConfigEditor["notice"]>(null);
  const [formGen, setFormGen] = useState(0);
  const [loadGen, setLoadGen] = useState(0);
  const [retargetable, setRetargetable] = useState<TargetRename[]>([]);
  const baseRef = useRef<EditOutcome | null>(null);
  const opsRef = useRef<EditOp[]>([]);
  const seq = useRef(0);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  const setView = useCallback((o: EditOutcome | null, with_: EditOp[]) => {
    setViewState(o);
    setViewOps(with_);
  }, []);

  const setOps = useCallback((o: EditOp[]) => {
    opsRef.current = o;
    setOpsState(o);
  }, []);

  useEffect(() => {
    let live = true;
    clearTimeout(timer.current);
    seq.current++;
    editorApi.draft(configPath, null, []).then(
      (o) => {
        if (!live) return;
        setOps([]);
        baseRef.current = o;
        setRetargetable([]);
        setBase(o);
        setView(o, []);
        setFailure(null);
        setFormGen((g) => g + 1);
      },
      (e) => {
        if (!live) return;
        setOps([]);
        baseRef.current = null;
        setBase(null);
        setView(null, []);
        setFailure(asFailure(e));
      },
    );
    return () => {
      live = false;
    };
  }, [configPath, loadGen, setOps, setView]);

  const check = useCallback(async (): Promise<EditOutcome | null> => {
    const my = ++seq.current;
    setChecking(true);
    for (;;) {
      const sent = opsRef.current;
      try {
        const o = await editorApi.draft(configPath, baseRef.current?.version ?? null, sent);
        if (my === seq.current) {
          setView(o, sent);
          setChecking(false);
        }
        return o;
      } catch (e) {
        const f = asFailure(e);
        if (f.kind === "op" && f.op !== null && opsRef.current[f.op] === sent[f.op]) {
          // The operation does not fit the file: drop it, check the rest.
          setOps(opsRef.current.filter((_, i) => i !== f.op));
          setNotice({ tone: "bad", text: t("editor_not_applied", { error: f.message }) });
          continue;
        }
        if (my === seq.current) {
          setFailure(f);
          setChecking(false);
        }
        return null;
      }
    }
  }, [configPath, setOps, setView]);

  const edit = useCallback(
    (op: EditOp) => {
      setOps(pushOp(opsRef.current, op));
      setNotice(null);
      clearTimeout(timer.current);
      timer.current = setTimeout(() => void check(), PAUSE_MS);
    },
    [check, setOps],
  );

  const editNow = useCallback(
    async (op: EditOp) => {
      setOps(pushOp(opsRef.current, op));
      setNotice(null);
      clearTimeout(timer.current);
      const o = await check();
      setFormGen((g) => g + 1);
      return o;
    },
    [check, setOps],
  );

  const save = useCallback(async () => {
    clearTimeout(timer.current);
    if (!base || opsRef.current.length === 0) return;
    setSaving(true);
    try {
      const o = await editorApi.save(configPath, base.version, opsRef.current);
      seq.current++;
      setView(o, opsRef.current);
      setChecking(false);
      if (o.saved || !o.changed) {
        setOps([]);
        setViewOps([]);
        baseRef.current = o;
        setBase(o);
        setFormGen((g) => g + 1);
        if (o.saved) {
          setRetargetable(o.renames.filter((r) => r.overrides > 0));
          setNotice({ tone: "ok", text: o.backup ? t("editor_saved_as", { file: o.backup }) : t("editor_saved") });
          onSaved();
        }
      }
    } catch (e) {
      setFailure(asFailure(e));
    } finally {
      setSaving(false);
    }
  }, [base, configPath, onSaved, setOps, setView]);

  const discard = useCallback(() => {
    clearTimeout(timer.current);
    seq.current++;
    setOps([]);
    setView(base, []);
    setChecking(false);
    setNotice(null);
    setFormGen((g) => g + 1);
  }, [base, setOps, setView]);

  const reload = useCallback(() => {
    setNotice(null);
    setFailure(null);
    setLoadGen((g) => g + 1);
  }, []);

  const retarget = useCallback(async () => {
    try {
      const moved = await editorApi.retargetOverrides(
        configPath,
        baseRef.current?.version ?? "",
        retargetable.map((r): [string, string] => [r.from, r.to]),
      );
      setNotice({ tone: "ok", text: t("editor_moved", { n: moved }) });
    } catch (e) {
      setNotice({ tone: "bad", text: t("editor_not_moved", { error: asFailure(e).message }) });
    }
    setRetargetable([]);
  }, [retargetable, configPath]);

  const dismissRetarget = useCallback(() => setRetargetable([]), []);

  const currentOps = useCallback(() => opsRef.current, []);

  return {
    view,
    ops,
    dirty: isDirty(ops, view && viewOps === ops ? view : null),
    checking,
    saving,
    failure,
    notice,
    formGen,
    edit,
    editNow,
    save,
    discard,
    reload,
    retargetable,
    retarget,
    dismissRetarget,
    currentOps,
  };
}
