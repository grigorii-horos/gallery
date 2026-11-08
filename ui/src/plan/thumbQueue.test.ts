import { describe, expect, it, vi } from "vitest";
import { ThumbQueue } from "./thumbQueue";

function deferredLoader() {
  const calls: { path: string; resolve: (u: string) => void; reject: (e: unknown) => void }[] = [];
  const load = vi.fn(
    (path: string) =>
      new Promise<string>((resolve, reject) => {
        calls.push({ path, resolve, reject });
      }),
  );
  return { load, calls };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("thumbnail queue", () => {
  it("keeps at most `limit` loads in flight, newest request first", async () => {
    const { load, calls } = deferredLoader();
    const q = new ThumbQueue(load, 2);
    const got: string[] = [];
    for (const p of ["a", "b", "c", "d"]) q.request(p, (r) => "url" in r && got.push(r.url));
    expect(calls.map((c) => c.path)).toEqual(["a", "b"]);
    calls[0].resolve("A");
    await flush();
    // "d" was asked last, so it goes before "c".
    expect(calls.map((c) => c.path)).toEqual(["a", "b", "d"]);
    expect(got).toEqual(["A"]);
  });

  it("drops a withdrawn request before it starts and caches results", async () => {
    const { load, calls } = deferredLoader();
    const q = new ThumbQueue(load, 1);
    q.request("a", () => {});
    const cancelB = q.request("b", () => {});
    const seen: string[] = [];
    q.request("c", (r) => seen.push("url" in r ? r.url : r.error));
    cancelB();
    calls[0].resolve("A");
    await flush();
    expect(calls.map((c) => c.path)).toEqual(["a", "c"]);
    calls[1].reject("no thumbnail");
    await flush();
    expect(seen).toEqual(["no thumbnail"]);
    // Cached: answered at once, no new load.
    const again = vi.fn();
    q.request("a", again);
    expect(again).toHaveBeenCalledWith({ url: "A" });
    expect(q.cached("c")).toEqual({ error: "no thumbnail" });
    q.clearErrors();
    expect(q.cached("c")).toBeUndefined();
    expect(q.cached("a")).toEqual({ url: "A" });
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("shares one load between cells asking for the same file", async () => {
    const { load, calls } = deferredLoader();
    const q = new ThumbQueue(load, 4);
    const one = vi.fn();
    const two = vi.fn();
    q.request("a", one);
    q.request("a", two);
    expect(load).toHaveBeenCalledTimes(1);
    calls[0].resolve("A");
    await flush();
    expect(one).toHaveBeenCalledWith({ url: "A" });
    expect(two).toHaveBeenCalledWith({ url: "A" });
  });
});
