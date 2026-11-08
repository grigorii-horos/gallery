import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { baseName, gridWindow } from "./model";
import { Thumb } from "./Thumb";
import type { ThumbQueue } from "./thumbQueue";

const CELL = 132;
const ROW = 158;
const GAP = 8;

interface Props {
  files: string[];
  selected: string | null;
  onSelect: (path: string) => void;
  queue: ThumbQueue;
  /** Small mark in a cell's corner (manual decision, low confidence…). */
  mark?: (path: string) => { text: string; title: string } | null;
  label: string;
}

/// A virtual grid: only the rows in view (plus a little) are in the DOM, and
/// only their thumbnails are requested, so thousands of files stay smooth.
export function ThumbGrid({ files, selected, onSelect, queue, mark, label }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 800, h: 600 });
  const [top, setTop] = useState(0);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => setSize({ w: el.clientWidth - 2 * GAP || 800, h: el.clientHeight || 600 });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // A new group starts at the top.
  useEffect(() => {
    ref.current?.scrollTo?.({ top: 0 });
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setTop(0);
  }, [files]);

  const win = gridWindow(files.length, size.w, size.h, top, CELL, ROW, GAP);

  // Arrow keys move the selection; the grid scrolls it into view.
  const onKey = (e: KeyboardEvent) => {
    if (!selected) return;
    const i = files.indexOf(selected);
    const step = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: win.columns, ArrowUp: -win.columns }[e.key];
    if (i < 0 || step === undefined) return;
    const j = Math.min(files.length - 1, Math.max(0, i + step));
    e.preventDefault();
    onSelect(files[j]);
    const el = ref.current;
    if (el) {
      const y = Math.floor(j / win.columns) * (ROW + GAP);
      if (y < el.scrollTop) el.scrollTop = y;
      else if (y + ROW > el.scrollTop + el.clientHeight) el.scrollTop = y + ROW - el.clientHeight + GAP;
    }
  };

  return (
    <div
      ref={ref}
      className="thumbgrid"
      role="listbox"
      aria-label={label}
      tabIndex={0}
      onKeyDown={onKey}
      onScroll={(e) => setTop(e.currentTarget.scrollTop)}
    >
      <div style={{ height: win.totalHeight + 2 * GAP, position: "relative" }}>
        <div
          className="thumbgrid-rows"
          style={{
            transform: `translateY(${win.offsetY + GAP}px)`,
            // Cells stretch to fill the row; CELL is their minimum width.
            gridTemplateColumns: `repeat(${win.columns}, minmax(0, 1fr))`,
            gap: GAP,
            padding: `0 ${GAP}px`,
          }}
        >
          {files.slice(win.start, win.end).map((path) => {
            const m = mark?.(path);
            return (
              <button
                key={path}
                type="button"
                role="option"
                aria-selected={path === selected}
                className="cell"
                style={{ height: ROW }}
                title={path}
                onClick={() => onSelect(path)}
              >
                <span className="cell-thumb">
                  <Thumb path={path} queue={queue} />
                  {m && (
                    <span className="cell-mark" title={m.title}>
                      {m.text}
                    </span>
                  )}
                </span>
                <span className="cell-name">{baseName(path)}</span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
