import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { num } from "../i18n/format";
import { errorText, historyApi, type ItemView, type ReportDetail, type ReportEntry, type UndoPreview } from "../api";
import {
  entryChips,
  entryTally,
  FILTERS,
  filterCounts,
  filterItems,
  partialUndoConfirmLines,
  retryBlock,
  retryConfirmLines,
  runTime,
  STATUS_LABEL,
  undoBlock,
  undoConfirmLines,
  undoResultText,
  undoTargets,
  type ItemFilter,
} from "../history/model";
import type { HistoryState } from "../history/useHistory";
import { filesWord } from "../run/model";

/** Rows shown at once; "Show more" adds as many. */
const PAGE = 300;

interface Props {
  history: HistoryState;
  exportDir: string | null;
  /** Another operation runs (its text), or null. */
  busy: string | null;
  /** Open the retry plan on the Sort screen. */
  onRunPlan: (planPath: string) => void;
}

/// "History": past runs, newest first, and the files of the chosen one.
export function HistoryScreen({ history, exportDir, busy, onRunPlan }: Props) {
  const { t } = useTranslation();
  const [entries, setEntries] = useState<ReportEntry[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    historyApi.listReports(exportDir).then(
      (l) => {
        if (!live) return;
        setEntries(l);
        setListError(null);
      },
      (e) => live && setListError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [exportDir, history.tick]);

  const current = entries?.find((e) => e.path === selected) ?? entries?.[0] ?? null;

  return (
    <div className="history">
      <section className="history-list" aria-label={t("history_runs")}>
        {listError && (
          <div className="notice bad">
            <p className="notice-title">{t("history_unreadable")}</p>
            <pre className="notice-text">{listError}</pre>
          </div>
        )}
        {entries === null && !listError ? (
          <p className="empty small">{t("history_reading")}</p>
        ) : entries && entries.length === 0 ? (
          <div className="empty history-empty">
            <p>{t("history_none")}</p>
            <p className="small">
              {t("history_none_hint")} {exportDir ? t("history_none_server_dir") : t("history_none_server_pick")}
            </p>
          </div>
        ) : (
          <ul>
            {entries?.map((e) => (
              <li key={e.path}>
                <EntryRow entry={e} active={e.path === current?.path} onSelect={() => setSelected(e.path)} />
              </li>
            ))}
          </ul>
        )}
        {exportDir && (
          <p className="history-foot muted small" title={exportDir}>
            {t("history_server_reports")}
          </p>
        )}
      </section>
      {current ? (
        <Detail key={current.path} entry={current} history={history} exportDir={exportDir} busy={busy} onRunPlan={onRunPlan} />
      ) : (
        <section className="history-detail empty-detail" aria-label={t("history_details")}>
          <p className="muted">{t("history_choose")}</p>
        </section>
      )}
    </div>
  );
}

function Tally({ entry }: { entry: ReportEntry }) {
  const segs = entryTally(entry);
  const total = Math.max(entry.summary.total, 1);
  return (
    <span className="tally" aria-hidden>
      {segs.map((s) => (s.count > 0 ? <span key={s.id} className={`tally-${s.id}`} style={{ flexGrow: s.count / total }} /> : null))}
    </span>
  );
}

function Chips({ entry }: { entry: ReportEntry }) {
  useTranslation();
  return (
    <span className="chips">
      {entryChips(entry).map((c) => (
        <span key={c.label} className={`chip ${c.tone}`}>
          {c.label}
        </span>
      ))}
    </span>
  );
}

function EntryRow({ entry, active, onSelect }: { entry: ReportEntry; active: boolean; onSelect: () => void }) {
  useTranslation();
  const when = runTime(entry.started);
  return (
    <button
      type="button"
      className={`history-row${active ? " active" : ""}${entry.undone ? " undone" : ""}`}
      aria-current={active ? "true" : undefined}
      onClick={onSelect}
    >
      <span className="history-row-head">
        <span className="history-when">{when ?? entry.file_name}</span>
        <span className="muted">{filesWord(entry.summary.total)}</span>
      </span>
      {!entry.error && <Tally entry={entry} />}
      <Chips entry={entry} />
    </button>
  );
}

function Copy({ text, label }: { text: string; label?: string }) {
  const { t } = useTranslation();
  const shown = label ?? t("copy");
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };
  return (
    <button type="button" className="btn quiet small-btn" onClick={copy} aria-label={`${shown}: ${text}`}>
      {copied ? t("copied") : shown}
    </button>
  );
}

const folderOf = (p: string) => p.slice(0, Math.max(p.lastIndexOf("/"), 1));

function PathLine({ label, path }: { label: string; path: string }) {
  const { t } = useTranslation();
  return (
    <div className="path-line">
      <span className="muted">{label}</span>
      <span className="code" title={path}>
        <bdi>{path}</bdi>
      </span>
      <Copy text={path} />
      <Copy text={folderOf(path)} label={t("folder")} />
    </div>
  );
}

function Detail({
  entry,
  history,
  exportDir,
  busy,
  onRunPlan,
}: {
  entry: ReportEntry;
  history: HistoryState;
  exportDir: string | null;
  busy: string | null;
  onRunPlan: (planPath: string) => void;
}) {
  const { t } = useTranslation();
  const [detail, setDetail] = useState<ReportDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<ItemFilter>("all");
  const [shown, setShown] = useState(PAGE);
  const [confirm, setConfirm] = useState<
    "undo" | "retry" | { target: string; preview: UndoPreview } | { file: ItemView } | null
  >(null);

  useEffect(() => {
    let live = true;
    historyApi.loadReport(entry.path, exportDir).then(
      (d) => {
        if (!live) return;
        setDetail(d);
        setError(null);
      },
      (e) => live && setError(errorText(e)),
    );
    return () => {
      live = false;
    };
  }, [entry.path, exportDir, history.tick]);

  const items = useMemo(() => (detail ? filterItems(detail.items, filter) : []), [detail, filter]);
  const counts = useMemo(() => (detail ? filterCounts(detail.items) : null), [detail]);
  const targets = useMemo(() => (detail ? undoTargets(detail.items) : []), [detail]);
  const when = runTime(entry.started);
  const mineUndo = history.undo?.reportPath === entry.path ? history.undo : null;
  const opBusy = busy ?? (history.undo ? t("busy_undo_short") : history.retrying ? t("busy_retry_short") : null);
  const uBlock = undoBlock(entry, opBusy);
  const rBlock = retryBlock(entry, opBusy);
  const undoResult = history.undoResult?.report_path === entry.path ? history.undoResult : null;
  const undoError = history.undoError?.reportPath === entry.path ? history.undoError.message : null;
  const retryResult = history.retryResult?.report_path === entry.path ? history.retryResult : null;
  const retryError = history.retryError?.reportPath === entry.path ? history.retryError.message : null;
  const s = entry.summary;

  return (
    <section className="history-detail" aria-label={t("history_details")}>
      <header className="history-detail-head">
        <h2>{when ? t("history_run_from", { when }) : entry.file_name}</h2>
        <Chips entry={entry} />
      </header>

      {entry.error ? (
        <div className="notice bad">
          <p className="notice-title">{t("history_report_unreadable")}</p>
          <pre className="notice-text">{entry.error}</pre>
        </div>
      ) : (
        <ul className="history-counts" aria-label={t("history_summary")}>
          {[
            [s.done, t("row_processed"), "ok"],
            [s.renamed, t("row_renamed"), ""],
            [s.failed, t("row_errors"), "bad"],
            [s.stale, t("history_stale"), "warn"],
            [s.missing, t("row_missing"), "warn"],
            [s.not_started, t("history_not_started"), ""],
          ]
            .filter(([n], i) => i === 0 || (n as number) > 0)
            .map(([n, label, tone]) => (
              <li key={label as string} className={tone as string}>
                <span className="fate-count">{num(n as number)}</span> {label}
              </li>
            ))}
        </ul>
      )}

      <div className="history-paths">
        <PathLine label={t("history_report")} path={entry.path} />
        <PathLine label={t("history_plan")} path={entry.plan_path} />
      </div>

      {entry.side === "server" ? (
        <div className="run-exported">
          <p>{t("history_server_note")}</p>
          {detail?.server_command && (
            <div className="command">
              <code>{detail.server_command}</code>
              <Copy text={detail.server_command} />
            </div>
          )}
        </div>
      ) : mineUndo ? (
        <UndoProgressView history={history} />
      ) : (
        <>
          <div className="run-actions">
            <button
              type="button"
              className="btn"
              disabled={!!uBlock || !detail}
              title={uBlock?.text}
              onClick={() => setConfirm("undo")}
            >
              {t("history_undo")}
            </button>
            <button
              type="button"
              className="btn"
              disabled={!!rBlock || history.retrying !== null}
              title={rBlock?.text}
              onClick={() => setConfirm("retry")}
            >
              {history.retrying === entry.path ? t("history_preparing") : t("history_retry")}
            </button>
          </div>
          {(uBlock && rBlock && !entry.error) && <p className="run-block">{uBlock.text}</p>}
        </>
      )}

      {undoError && (
        <div className="notice bad">
          <p className="notice-title">{t("history_undo_failed")}</p>
          <pre className="notice-text">{undoError}</pre>
        </div>
      )}
      {undoResult && !mineUndo && (
        <div className={`run-result ${undoResult.problems.length > 0 ? "bad" : undoResult.cancelled ? "warn" : ""}`} role="status">
          <p className="run-result-title">
            {undoResult.cancelled
              ? t("history_undo_stopped")
              : undoResult.problems.length > 0
                ? t("history_undo_problems")
                : t("history_undone")}
          </p>
          <p>{undoResultText(undoResult.restored, undoResult.removed_outputs)}</p>
          {undoResult.problems.length > 0 && (
            <ul className="history-problems">
              {undoResult.problems.map((p) => (
                <li key={p} className="code">
                  {p}
                </li>
              ))}
            </ul>
          )}
          {undoResult.undone && <p className="muted small">{t("history_back_to_plan")}</p>}
        </div>
      )}
      {retryError && (
        <div className="notice bad">
          <p className="notice-title">{t("history_retry_failed")}</p>
          <pre className="notice-text">{retryError}</pre>
        </div>
      )}
      {retryResult && (
        <div className="run-exported" role="status">
          {retryResult.plan_path ? (
            <>
              <p>
                {retryResult.reused ? t("history_retry_reused") : t("history_retry_ready")}:{" "}
                {t("history_retry_files", { files: filesWord(retryResult.files), failed: retryResult.failed })}
                {retryResult.left_out > 0 && ` ${t("history_retry_left_out", { count: retryResult.left_out })}`}
              </p>
              <p className="code">{retryResult.plan_path}</p>
              <div className="run-actions">
                <button type="button" className="btn primary" onClick={() => onRunPlan(retryResult.plan_path!)}>
                  {t("history_open_in_sort")}
                </button>
              </div>
            </>
          ) : (
            <p>
              {t("history_nothing_to_retry", { count: retryResult.failed })}
            </p>
          )}
        </div>
      )}

      {detail?.warnings.map((w) => (
        <p key={w} className="muted small">
          {w}
        </p>
      ))}

      {error && (
        <div className="notice bad">
          <p className="notice-title">{t("history_files_unreadable")}</p>
          <pre className="notice-text">{error}</pre>
        </div>
      )}
      {detail && counts && (
        <>
          <div className="tabs history-tabs" role="tablist" aria-label={t("check_files")}>
            {FILTERS.map((f) => (
              <button
                key={f.id}
                type="button"
                role="tab"
                className="tab"
                aria-selected={filter === f.id}
                disabled={f.id !== "all" && counts[f.id] === 0}
                onClick={() => {
                  setFilter(f.id);
                  setShown(PAGE);
                }}
              >
                {t(f.label)}
                <span className="count">{num(counts[f.id])}</span>
              </button>
            ))}
          </div>
          {targets.length > 0 && !uBlock && !mineUndo && entry.side === "local" && (
            <ul className="history-targets" aria-label={t("history_targets")}>
              {targets.map((g) => (
                <li key={g.target}>
                  <span className="item-source" title={g.target}>
                    <bdi>{g.target}</bdi>
                  </span>
                  <span className="count">{num(g.sources.length)}</span>
                  <button
                    type="button"
                    className="btn quiet"
                    aria-label={t("history_undo_target", { target: g.target })}
                    onClick={() => setConfirm({ target: g.target, preview: g.preview })}
                  >
                    {t("history_undo")}
                  </button>
                </li>
              ))}
            </ul>
          )}
          {items.length === 0 ? (
            <p className="empty small">{t("history_no_files")}</p>
          ) : (
            <ul className="history-items" aria-label={t("history_run_files")}>
              {items.slice(0, shown).map((it) => (
                <ItemRow
                  key={it.source}
                  item={it}
                  onUndo={!uBlock && !mineUndo && entry.side === "local" ? () => setConfirm({ file: it }) : undefined}
                />
              ))}
            </ul>
          )}
          {items.length > shown && (
            <button type="button" className="btn quiet" onClick={() => setShown((n) => n + PAGE)}>
              {t("show_more", { n: items.length - shown })}
            </button>
          )}
        </>
      )}

      {confirm === "undo" && detail && (
        <Confirm
          {...undoConfirmLines(entry, detail.undo)}
          action={t("undo")}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setConfirm(null);
            void history.startUndo(entry.path);
          }}
        />
      )}
      {confirm !== null && typeof confirm === "object" && "target" in confirm && (
        <Confirm
          {...partialUndoConfirmLines(confirm.target, confirm.preview)}
          action={t("undo")}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setConfirm(null);
            void history.startUndoTarget(entry.path, confirm.target);
          }}
        />
      )}
      {confirm !== null && typeof confirm === "object" && "file" in confirm && (
        <Confirm
          {...partialUndoConfirmLines(null, confirm.file.undo)}
          action={t("undo")}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setConfirm(null);
            void history.startUndoFiles(entry.path, [confirm.file.source]);
          }}
        />
      )}
      {confirm === "retry" && (
        <Confirm
          title={t("history_retry_title")}
          lines={retryConfirmLines(entry)}
          action={t("history_prepare_retry")}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setConfirm(null);
            void history.startRetry(entry.path);
          }}
        />
      )}
    </section>
  );
}

function UndoProgressView({ history }: { history: HistoryState }) {
  const { t } = useTranslation();
  const u = history.undo!;
  const p = u.progress;
  const frac = p.total > 0 ? Math.min(1, p.done / p.total) : 0;
  return (
    <div className="run-progress" role="status" aria-label={t("history_undo_progress")}>
      <div className="run-progress-head">
        <span>
          {t("history_undo_count", { done: p.done, total: p.total })}
        </span>
      </div>
      <div className="run-bar" aria-hidden>
        <span style={{ width: `${frac * 100}%` }} />
      </div>
      <div className="scan-current">
        <span className="path" title={p.current ?? ""}>
          <bdi>{p.current ?? t("getting_ready")}</bdi>
        </span>
        <button type="button" className="btn" disabled={u.cancelling} onClick={history.cancelUndo}>
          {u.cancelling ? t("stopping") : t("cancel")}
        </button>
      </div>
      {u.cancelling && <p className="muted small">{t("history_cancelling")}</p>}
    </div>
  );
}

function ItemRow({ item, onUndo }: { item: ItemView; onUndo?: () => void }) {
  const { t } = useTranslation();
  const problem = item.status === "failed" || item.status === "stale" || item.status === "missing";
  const errors = item.outputs.filter((o) => o.error);
  const warnings = item.outputs.flatMap((o) => o.warnings);
  const why = item.error ?? (errors[0] ? errors[0].error : null);
  return (
    <li className={`history-item ${item.status}${item.undone ? " undone" : ""}`}>
      <details>
        <summary>
          <span className={`item-dot ${item.status}`} aria-hidden />
          <span className="code item-source" title={item.source}>
            <bdi>{item.source}</bdi>
          </span>
          <span className={problem && !item.undone ? `item-status ${item.status}` : "item-status muted"}>
            {item.undone ? t("chip_undone") : item.renamed ? t("item_renamed") : t(STATUS_LABEL[item.status])}
          </span>
          {why && <span className="item-why bad-text">{why}</span>}
        </summary>
        <div className="item-body">
          {item.error && <p className="bad-text">{item.error}</p>}
          {errors.map((o) => (
            <p key={o.planned} className="bad-text">
              <span className="code">{o.planned}</span>: {o.error}
            </p>
          ))}
          {item.outputs
            .filter((o) => o.actual)
            .map((o) => (
              <Line key={o.actual!} label={o.renamed_from ? t("item_renamed_to") : t("item_copy")}>
                <span className="code">{o.actual}</span>
                <Copy text={folderOf(o.actual!)} label={t("folder")} />
              </Line>
            ))}
          {item.trashed && (
            <Line label={t("item_in_trash")}>
              <span className="code">{item.trashed}</span>
            </Line>
          )}
          {warnings.map((w) => (
            <p key={w} className="warn-text small">
              {w}
            </p>
          ))}
          <Line label={t("item_source")}>
            <Copy text={item.source} />
            <Copy text={folderOf(item.source)} label={t("folder")} />
          </Line>
          {onUndo && item.undo.items > 0 && (
            <p className="item-line">
              <button type="button" className="btn quiet" onClick={onUndo}>
                {t("history_undo_file")}
              </button>
            </p>
          )}
        </div>
      </details>
    </li>
  );
}

function Line({ label, children }: { label: string; children: ReactNode }) {
  return (
    <p className="item-line">
      <span className="muted">{label}</span> {children}
    </p>
  );
}

function Confirm({
  title,
  lines,
  action,
  onCancel,
  onConfirm,
}: {
  title: string;
  lines: string[];
  action: string;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);
  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="history-confirm-title">
        <h2 id="history-confirm-title">{title}</h2>
        <ul className="modal-lines">
          {lines.map((l) => (
            <li key={l}>{l}</li>
          ))}
        </ul>
        <div className="modal-actions">
          <button ref={cancelRef} type="button" className="btn" onClick={onCancel}>
            {t("cancel")}
          </button>
          <button type="button" className="btn primary" onClick={onConfirm}>
            {action}
          </button>
        </div>
      </div>
    </div>
  );
}
