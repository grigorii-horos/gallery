import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { baseName, dirName, suggestFiles } from "./model";

/// "Find file…": a path or a name; suggestions come from the plan's files
/// (also the unchanged and unmatched ones). An absolute path not in the plan
/// can be explained too.
export function FileSearch({ files, onPick }: { files: string[]; onPick: (path: string) => void }) {
  const { t } = useTranslation();
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);
  const hits = useMemo(() => suggestFiles(files, q), [files, q]);
  const typed = q.trim();
  const absolute = typed.startsWith("/") || typed.startsWith("~");

  const pick = (p: string) => {
    setOpen(false);
    setQ("");
    onPick(p);
  };
  const submit = () => {
    if (hits.length > 0 && !(absolute && files.includes(typed))) pick(absolute && hits.includes(typed) ? typed : hits[0]);
    else if (absolute) pick(typed);
  };

  return (
    <div className="file-search">
      <input
        type="search"
        placeholder={t("search_placeholder")}
        aria-label={t("search_aria")}
        value={q}
        onChange={(e) => {
          setQ(e.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onKeyDown={(e) => {
          if (e.key === "Enter") submit();
          if (e.key === "Escape") setOpen(false);
        }}
      />
      {open && (hits.length > 0 || absolute) && (
        <ul className="search-hits" role="listbox" aria-label={t("search_hits_aria")}>
          {hits.map((p) => (
            <li key={p} role="option" aria-selected={false}>
              <button type="button" onClick={() => pick(p)} title={p}>
                <span>{baseName(p)}</span>
                <span className="muted small">{dirName(p)}</span>
              </button>
            </li>
          ))}
          {absolute && !hits.includes(typed) && (
            <li role="option" aria-selected={false}>
              <button type="button" onClick={() => pick(typed)}>
                <span>{t("search_explain_path")}</span>
                <span className="muted small">{typed}</span>
              </button>
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
