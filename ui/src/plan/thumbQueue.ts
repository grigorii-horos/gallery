// Lazy thumbnail loading for the virtual grid: at most `limit` requests in
// flight, the most recently asked first (what just scrolled into view), a
// request whose cell left the view is dropped before it starts. Results
// (URLs and failures) are cached for the session.

export type ThumbResult = { url: string } | { error: string };
type Listener = (r: ThumbResult) => void;

export class ThumbQueue {
  private cache = new Map<string, ThumbResult>();
  private waiting = new Map<string, Set<Listener>>();
  /** Paths not started yet, oldest first; taken from the end. */
  private pending: string[] = [];
  private inFlight = new Set<string>();

  constructor(
    private load: (path: string) => Promise<string>,
    private limit = 6,
  ) {}

  cached(path: string): ThumbResult | undefined {
    return this.cache.get(path);
  }

  /** Ask for a thumbnail; returns a function that withdraws the request. */
  request(path: string, cb: Listener): () => void {
    const hit = this.cache.get(path);
    if (hit) {
      cb(hit);
      return () => {};
    }
    let set = this.waiting.get(path);
    if (!set) {
      set = new Set();
      this.waiting.set(path, set);
    }
    set.add(cb);
    if (this.inFlight.has(path)) return this.withdraw(path, cb);
    // Move to the front of the line (the end of `pending`).
    const at = this.pending.indexOf(path);
    if (at >= 0) this.pending.splice(at, 1);
    this.pending.push(path);
    this.pump();
    return this.withdraw(path, cb);
  }

  private withdraw(path: string, cb: Listener): () => void {
    return () => {
      const s = this.waiting.get(path);
      if (!s) return;
      s.delete(cb);
      if (s.size === 0 && !this.inFlight.has(path)) {
        this.waiting.delete(path);
        const i = this.pending.indexOf(path);
        if (i >= 0) this.pending.splice(i, 1);
      }
    };
  }

  /** Forget failures, e.g. after a rescan. */
  clearErrors() {
    for (const [k, v] of this.cache) if ("error" in v) this.cache.delete(k);
  }

  private pump() {
    while (this.inFlight.size < this.limit && this.pending.length > 0) {
      const path = this.pending.pop()!;
      this.inFlight.add(path);
      this.load(path)
        .then(
          (url): ThumbResult => ({ url }),
          (e: unknown): ThumbResult => ({ error: String(e) }),
        )
        .then((r) => {
          this.cache.set(path, r);
          const s = this.waiting.get(path);
          this.waiting.delete(path);
          s?.forEach((cb) => cb(r));
          this.inFlight.delete(path);
          this.pump();
        });
    }
  }
}
