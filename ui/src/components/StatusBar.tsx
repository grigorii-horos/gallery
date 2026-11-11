import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { AppStatus } from "../api";
import type { MessageKey } from "../i18n";

const KEY_TEXT: Record<AppStatus["api_key_source"], MessageKey> = {
  env: "status_key_env",
  file: "status_key_file",
  none: "status_key_none",
};

/// Bottom bar: config health, tools, AI key, config path.
export function StatusBar({
  status,
  loading,
}: {
  status: AppStatus | null;
  loading: boolean;
}) {
  const { t } = useTranslation();
  const [toolsOpen, setToolsOpen] = useState(false);
  if (!status) {
    return (
      <footer className="statusbar">
        <span className="st muted">
          {loading ? t("status_checking") : t("status_unavailable")}
        </span>
      </footer>
    );
  }
  const required = status.tools.items.filter((x) => x.required);
  const found = required.filter((x) => x.path).length;
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
            ? t("status_config_warnings", { count: warnings })
            : t("status_config_ok")
          : t("status_config_error")}
      </span>
      <span className="st-wrap">
        <button
          type="button"
          className={`st st-btn ${status.tools.ok ? (caps?.svtav1 ? "ok" : "warn") : "bad"}`}
          aria-expanded={toolsOpen}
          onClick={() => setToolsOpen((o) => !o)}
        >
          <i className="dot" />
          {t("status_tools", { found, total: required.length })}
        </button>
        {toolsOpen && (
          <div
            className="popover"
            role="dialog"
            aria-label={t("status_tools_aria")}
          >
            <table className="grid compact">
              <tbody>
                {status.tools.items.map((tool) => (
                  <tr key={tool.name}>
                    <th scope="row">{tool.name}</th>
                    <td
                      className={
                        tool.path ? "" : tool.required ? "bad-text" : "muted"
                      }
                    >
                      {tool.path
                        ? (tool.version ?? tool.path)
                        : tool.required
                          ? t("status_tool_required")
                          : t("status_tool_png")}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {caps && (
              <ul className="caps">
                <Cap ok={caps.svtav1} text={t("status_cap_av1")} />
                <Cap ok={caps.opus} text={t("status_cap_opus")} />
                <Cap ok={caps.vidstab} text={t("status_cap_vidstab")} />
              </ul>
            )}
          </div>
        )}
      </span>
      {status.ai_backend === "claude-code" ? (
        <span
          className={`st ${status.claude_path ? "ok" : "warn"}`}
          title={status.claude_path ?? undefined}
        >
          <i className="dot" />
          {t(
            status.claude_path
              ? "status_ai_claude_code"
              : "status_ai_no_claude",
          )}
        </span>
      ) : (
        <span
          className={`st ${status.api_key_source === "none" ? "warn" : "ok"}`}
        >
          <i className="dot" />
          {t(KEY_TEXT[status.api_key_source])}
        </span>
      )}
      <span className="st path grow" title={status.config_path}>
        <bdi>{status.config_path}</bdi>
      </span>
    </footer>
  );
}

function Cap({ ok, text }: { ok: boolean; text: string }) {
  const { t } = useTranslation();
  return (
    <li className={ok ? "ok" : "bad"}>
      <i className="dot" />
      {text}: {ok ? t("yes") : t("no")}
    </li>
  );
}
