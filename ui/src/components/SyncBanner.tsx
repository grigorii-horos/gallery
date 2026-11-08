import type { SyncPauseState } from "../sync/useSyncPause";
import { pausedText } from "../run/model";

/// Shown while Gallery holds Syncthing folders paused: after a run the user
/// looks at the result, then resumes.
export function SyncBanner({ sync }: { sync: SyncPauseState }) {
  if (!sync.paused) {
    // No pause to show, but a state file that cannot be read must not look like "nothing paused".
    if (!sync.error) return null;
    return (
      <div className="notice bad sync-banner" role="alert">
        <p className="bad-text">{sync.error}</p>
      </div>
    );
  }
  return (
    <div className="notice sync-banner" role="status">
      <p>
        <strong>{pausedText(sync.paused)}</strong>
        <span className="muted"> — check the result, then resume</span>
      </p>
      <button type="button" className="btn primary" disabled={sync.resuming} onClick={() => void sync.resume()}>
        {sync.resuming ? "Resuming…" : "Resume sync"}
      </button>
      {sync.error && <p className="bad-text">{sync.error}</p>}
    </div>
  );
}
