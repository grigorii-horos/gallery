import { useEffect, useState } from "react";
import type { ThumbQueue, ThumbResult } from "./thumbQueue";

function ext(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
}

/// A thumbnail asked from the queue while mounted; the extension stands in
/// while it loads or when the file has none (audio, other files).
export function Thumb({ path, queue, fit = "cover" }: { path: string; queue: ThumbQueue; fit?: "cover" | "contain" }) {
  const [state, setState] = useState<{ path: string; r: ThumbResult | undefined }>(() => ({
    path,
    r: queue.cached(path),
  }));
  const r = state.path === path ? state.r : queue.cached(path);

  useEffect(() => queue.request(path, (res) => setState({ path, r: res })), [path, queue]);

  if (r && "url" in r) {
    return <img className={`thumb-img ${fit}`} src={r.url} alt="" draggable={false} decoding="async" />;
  }
  return (
    <span className={`thumb-ph${r ? " failed" : ""}`} aria-hidden>
      {ext(path) || "?"}
    </span>
  );
}
