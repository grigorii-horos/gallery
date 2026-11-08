import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { errorText, historyApi, type ItemView, type ReportDetail, type ReportEntry } from "../api";
import {
  entryChips,
  entryTally,
  FILTERS,
  filterCounts,
  filterItems,
  retryBlock,
  retryConfirmLines,
  runTime,
  STATUS_LABEL,
  undoBlock,
  undoConfirmLines,
  undoResultText,
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
      <section className="history-list" aria-label="Runs">
        {listError && (
          <div className="notice bad">
            <p className="notice-title">Reports could not be read</p>
            <pre className="notice-text">{listError}</pre>
          </div>
        )}
        {entries === null && !listError ? (
          <p className="empty small">Reading reports…</p>
        ) : entries && entries.length === 0 ? (
          <div className="empty history-empty">
            <p>No runs yet.</p>
            <p className="small">
              A report appears after a plan is run on the Sort screen.
              {exportDir ? " Server reports arrive in the export folder." : " To see server reports, choose an export folder there too."}
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
            Server reports — from the export folder
          </p>
        )}
      </section>
      {current ? (
        <Detail key={current.path} entry={current} history={history} exportDir={exportDir} busy={busy} onRunPlan={onRunPlan} />
      ) : (
        <section className="history-detail empty-detail" aria-label="Details">
          <p className="muted">Choose a run on the left.</p>
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

function Copy({ text, label = "Copy" }: { text: string; label?: string }) {
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
    <button type="button" className="btn quiet small-btn" onClick={copy} aria-label={`${label}: ${text}`}>
      {copied ? "Copied" : label}
    </button>
  );
}

const folderOf = (p: string) => p.slice(0, Math.max(p.lastIndexOf("/"), 1));

function PathLine({ label, path }: { label: string; path: string }) {
  return (
    <div className="path-line">
      <span className="muted">{label}</span>
      <span className="code" title={path}>
        <bdi>{path}</bdi>
      </span>
      <Copy text={path} />
      <Copy text={folderOf(path)} label="Folder" />
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
  const [detail, setDetail] = useState<ReportDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<ItemFilter>("all");
  const [shown, setShown] = useState(PAGE);
  const [confirm, setConfirm] = useState<"undo" | "retry" | null>(null);

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
  const when = runTime(entry.started);
  const mineUndo = history.undo?.reportPath === entry.path ? history.undo : null;
  const opBusy = busy ?? (history.undo ? "An undo is running — wait for it to finish." : history.retrying ? "A retry is being prepared — wait for it to finish." : null);
  const uBlock = undoBlock(entry, opBusy);
  const rBlock = retryBlock(entry, opBusy);
  const undoResult = history.undoResult?.report_path === entry.path ? history.undoResult : null;
  const undoError = history.undoError?.reportPath === entry.path ? history.undoError.message : null;
  const retryResult = history.retryResult?.report_path === entry.path ? history.retryResult : null;
  const retryError = history.retryError?.reportPath === entry.path ? history.retryError.message : null;
  const s = entry.summary;

  return (
    <section className="history-detail" aria-label="Details">
      <header className="history-detail-head">
        <h2>{when ? `Run from ${when}` : entry.file_name}</h2>
        <Chips entry={entry} />
      </header>

      {entry.error ? (
        <div className="notice bad">
          <p className="notice-title">The report could not be read</p>
          <pre className="notice-text">{entry.error}</pre>
        </div>
      ) : (
        <ul className="history-counts" aria-label="Summary">
          {[
            [s.done, "processed", "ok"],
            [s.renamed, "of them renamed in place", ""],
            [s.failed, "errors", "bad"],
            [s.stale, "changed after the plan", "warn"],
            [s.missing, "file missing", "warn"],
            [s.not_started, "not started (cancelled)", ""],
          ]
            .filter(([n], i) => i === 0 || (n as number) > 0)
            .map(([n, label, tone]) => (
              <li key={label as string} className={tone as string}>
                <span className="fate-count">{(n as number).toLocaleString("en-US")}</span> {label}
              </li>
            ))}
        </ul>
      )}

      <div className="history-paths">
        <PathLine label="Report" path={entry.path} />
        <PathLine label="Plan" path={entry.plan_path} />
      </div>

      {entry.side === "server" ? (
        <div className="run-exported">
          <p>The run was on a server. The GUI does not undo it and does not touch the server — undo it there:</p>
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
              Undo…
            </button>
            <button
              type="button"
              className="btn"
              disabled={!!rBlock || history.retrying !== null}
              title={rBlock?.text}
              onClick={() => setConfirm("retry")}
            >
              {history.retrying === entry.path ? "Preparing retry…" : "Retry failed…"}
            </button>
          </div>
          {(uBlock && rBlock && !entry.error) && <p className="run-block">{uBlock.text}</p>}
        </>
      )}

      {undoError && (
        <div className="notice bad">
          <p className="notice-title">Undo failed</p>
          <pre className="notice-text">{undoError}</pre>
        </div>
      )}
      {undoResult && !mineUndo && (
        <div className={`run-result ${undoResult.problems.length > 0 ? "bad" : undoResult.cancelled ? "warn" : ""}`} role="status">
          <p className="run-result-title">
            {undoResult.cancelled
              ? "Undo stopped: you can undo the rest again"
              : undoResult.problems.length > 0
                ? "Undo with problems: these files were left as they are"
                : "The run was undone"}
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
          {undoResult.undone && <p className="muted small">These files will go back into the current plan on the Sort screen.</p>}
        </div>
      )}
      {retryError && (
        <div className="notice bad">
          <p className="notice-title">The retry was not prepared</p>
          <pre className="notice-text">{retryError}</pre>
        </div>
      )}
      {retryResult && (
        <div className="run-exported" role="status">
          {retryResult.plan_path ? (
            <>
              <p>
                {retryResult.reused ? "The retry plan was already prepared" : "The retry plan is ready"}: {filesWord(retryResult.files)} of{" "}
                {retryResult.failed} failed.
                {retryResult.left_out > 0 && ` ${retryResult.left_out} left out: they changed, went to the trash or their copies could not be removed.`}
              </p>
              <p className="code">{retryResult.plan_path}</p>
              <div className="run-actions">
                <button type="button" className="btn primary" onClick={() => onRunPlan(retryResult.plan_path!)}>
                  Open in Sort
                </button>
              </div>
            </>
          ) : (
            <p>
              Nothing to retry: none of the {retryResult.failed} failed files qualifies — they changed, went to the trash or
              their copies could not be removed.
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
          <p className="notice-title">The report's files could not be read</p>
          <pre className="notice-text">{error}</pre>
        </div>
      )}
      {detail && counts && (
        <>
          <div className="tabs history-tabs" role="tablist" aria-label="Files">
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
                {f.label}
                <span className="count">{counts[f.id].toLocaleString("en-US")}</span>
              </button>
            ))}
          </div>
          {items.length === 0 ? (
            <p className="empty small">No files.</p>
          ) : (
            <ul className="history-items" aria-label="Run files">
              {items.slice(0, shown).map((it) => (
                <ItemRow key={it.source} item={it} />
              ))}
            </ul>
          )}
          {items.length > shown && (
            <button type="button" className="btn quiet" onClick={() => setShown((n) => n + PAGE)}>
              Show more ({(items.length - shown).toLocaleString("en-US")})
            </button>
          )}
        </>
      )}

      {confirm === "undo" && detail && (
        <Confirm
          {...undoConfirmLines(entry, detail.undo)}
          action="Undo"
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            setConfirm(null);
            void history.startUndo(entry.path);
          }}
        />
      )}
      {confirm === "retry" && (
        <Confirm
          title="Retry the failed files?"
          lines={retryConfirmLines(entry)}
          action="Prepare retry"
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
  const u = history.undo!;
  const p = u.progress;
  const frac = p.total > 0 ? Math.min(1, p.done / p.total) : 0;
  return (
    <div className="run-progress" role="status" aria-label="Undo: progress">
      <div className="run-progress-head">
        <span>
          Undo: {p.done.toLocaleString("en-US")} of {p.total.toLocaleString("en-US")}
        </span>
      </div>
      <div className="run-bar" aria-hidden>
        <span style={{ width: `${frac * 100}%` }} />
      </div>
      <div className="scan-current">
        <span className="path" title={p.current ?? ""}>
          <bdi>{p.current ?? "Getting ready…"}</bdi>
        </span>
        <button type="button" className="btn" disabled={u.cancelling} onClick={history.cancelUndo}>
          {u.cancelling ? "Stopping…" : "Cancel"}
        </button>
      </div>
      {u.cancelling && <p className="muted small">No new files are started; the one in progress is finished.</p>}
    </div>
  );
}

function ItemRow({ item }: { item: ItemView }) {
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
            {item.undone ? "undone" : item.renamed ? "renamed" : STATUS_LABEL[item.status]}
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
              <Line key={o.actual!} label={o.renamed_from ? "renamed to" : "copy"}>
                <span className="code">{o.actual}</span>
                <Copy text={folderOf(o.actual!)} label="Folder" />
              </Line>
            ))}
          {item.trashed && (
            <Line label="in the trash">
              <span className="code">{item.trashed}</span>
            </Line>
          )}
          {warnings.map((w) => (
            <p key={w} className="warn-text small">
              {w}
            </p>
          ))}
          <Line label="source">
            <Copy text={item.source} />
            <Copy text={folderOf(item.source)} label="Folder" />
          </Line>
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
            Cancel
          </button>
          <button type="button" className="btn primary" onClick={onConfirm}>
            {action}
          </button>
        </div>
      </div>
    </div>
  );
}
