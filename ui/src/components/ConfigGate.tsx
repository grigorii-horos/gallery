import type { ReactNode } from "react";
import type { ConfigSummary } from "../api";
import type { AppData } from "../store";
import { useTranslation } from "react-i18next";

/// Renders `children` with the loaded config, or says why there is none.
/// Validation errors and warnings are shown above the content.
export function ConfigGate({
  data,
  children,
}: {
  data: AppData;
  children: (summary: ConfigSummary) => ReactNode;
}) {
  const { t } = useTranslation();
  if (!data.summary) {
    if (data.loading) return <p className="empty">{t("gate_reading")}</p>;
    return (
      <div className="notice bad">
        <p className="notice-title">{t("gate_unreadable")}</p>
        <pre className="notice-text">{data.loadError}</pre>
        <p>{t("gate_fix_hint")}</p>
      </div>
    );
  }
  const s = data.summary;
  return (
    <>
      {!s.valid && (
        <div className="notice bad">
          <p className="notice-title">{t("gate_invalid")}</p>
          <pre className="notice-text">{s.error}</pre>
        </div>
      )}
      {s.warnings.length > 0 && (
        <div className="notice warn">
          {s.warnings.map((w) => (
            <p key={w}>{w}</p>
          ))}
        </div>
      )}
      {children(s)}
    </>
  );
}

/// A mask or path value, set in the code face.
export function Code({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <code className="code" title={title}>
      {children}
    </code>
  );
}
