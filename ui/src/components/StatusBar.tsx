import { useState } from "react";
import type { AppStatus } from "../api";

const KEY_TEXT: Record<AppStatus["api_key_source"], string> = {
  env: "AI key from the environment",
  file: "AI key from a file",
  none: "No AI key",
};

/// Bottom bar: config health, tools, AI key, config path.
export function StatusBar({ status, loading }: { status: AppStatus | null; loading: boolean }) {
  const [toolsOpen, setToolsOpen] = useState(false);
  if (!status) {
    return (
      <footer className="statusbar">
        <span className="st muted">{loading ? "Checking config and tools…" : "Status unavailable"}</span>
      </footer>
    );
  }
  const required = status.tools.items.filter((t) => t.required);
  const found = required.filter((t) => t.path).length;
  const warnings = status.config_warnings.length;
  const caps = status.tools.capabilities;

  return (
    <footer className="statusbar">
      <span
        className={`st ${status.config_ok ? (warnings ? "warn" : "ok") : "bad"}`}
        title={status.config_error ?? status.config_warnings.join("\n")}
      >
        <i className="dot" />
        {status.config_ok
          ? warnings
            ? `Config OK, warnings: ${warnings}`
            : "Config OK"
          : "Config has an error"}
      </span>
      <span className="st-wrap">
        <button
          type="button"
          className={`st st-btn ${status.tools.ok ? (caps?.svtav1 ? "ok" : "warn") : "bad"}`}
          aria-expanded={toolsOpen}
          onClick={() => setToolsOpen((o) => !o)}
        >
          <i className="dot" />
          {`Tools ${found} of ${required.length}`}
        </button>
        {toolsOpen && (
          <div className="popover" role="dialog" aria-label="Tools">
            <table className="grid compact">
              <tbody>
                {status.tools.items.map((t) => (
                  <tr key={t.name}>
                    <th scope="row">{t.name}</th>
                    <td className={t.path ? "" : t.required ? "bad-text" : "muted"}>
                      {t.path
                        ? (t.version ?? t.path)
                        : t.required
                          ? "not found, required"
                          : "not found, needed for PNG"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {caps && (
              <ul className="caps">
                <Cap ok={caps.svtav1} text="AV1 video (libsvtav1)" />
                <Cap ok={caps.opus} text="Opus audio (libopus)" />
                <Cap ok={caps.vidstab} text="Stabilization (vidstab)" />
              </ul>
            )}
          </div>
        )}
      </span>
      <span className={`st ${status.api_key_source === "none" ? "warn" : "ok"}`}>
        <i className="dot" />
        {KEY_TEXT[status.api_key_source]}
      </span>
      <span className="st path grow" title={status.config_path}>
        <bdi>{status.config_path}</bdi>
      </span>
    </footer>
  );
}

function Cap({ ok, text }: { ok: boolean; text: string }) {
  return (
    <li className={ok ? "ok" : "bad"}>
      <i className="dot" />
      {text}: {ok ? "yes" : "no"}
    </li>
  );
}
