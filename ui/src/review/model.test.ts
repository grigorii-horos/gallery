import { describe, expect, it } from "vitest";
import type { ReviewItem } from "../api";
import {
  groupEvents,
  groupSeries,
  seriesReviews,
  groupState,
  initialSelection,
  decisionState,
  failureText,
  isNameError,
  nextFocus,
  pushRecent,
  seriesSpan,
  shortcutTargets,
  toggle,
  toggleGroup,
} from "./model";

const item = (source: string, captured: string | null, root = "/phone", reason = "AI unsure"): ReviewItem => ({
  source,
  size: 1,
  mtime_ns: 0,
  suggested: "Photos",
  reason,
  captured,
  source_root: root,
});

const paths = (s: { items: ReviewItem[] }[]) => s.map((g) => g.items.map((i) => i.source));

describe("series", () => {
  it("splits by a gap over 60 s, counting from the previous file", () => {
    const s = groupSeries([
      item("/phone/a", "2026-04-12T10:00:00"),
      item("/phone/b", "2026-04-12T10:01:00"), // exactly 60 s: same series
      item("/phone/c", "2026-04-12T10:01:55"), // 55 s after b: still the same
      item("/phone/d", "2026-04-12T10:03:00"), // 65 s: new series
    ]);
    expect(paths(s)).toEqual([["/phone/a", "/phone/b", "/phone/c"], ["/phone/d"]]);
    expect(seriesSpan(s[0])).toEqual({ start: "2026-04-12T10:00:00", seconds: 115 });
  });

  it("sorts by time within a source and never mixes sources", () => {
    const s = groupSeries([
      item("/phone/b", "2026-04-12T10:00:30"),
      item("/cam/x", "2026-04-12T10:00:10", "/cam"),
      item("/phone/a", "2026-04-12T10:00:00"),
      item("/cam/y", "2026-04-12T10:00:20", "/cam"),
    ]);
    expect(paths(s)).toEqual([
      ["/phone/a", "/phone/b"],
      ["/cam/x", "/cam/y"],
    ]);
    expect(s.map((g) => g.source)).toEqual(["/phone", "/cam"]);
  });

  it("keeps files without a capture time alone, after the dated ones", () => {
    const s = groupSeries([
      item("/phone/n2", null),
      item("/phone/a", "2026-04-12T10:00:00"),
      item("/phone/n1", null),
      item("/phone/b", "2026-04-12T10:00:05"),
    ]);
    expect(paths(s)).toEqual([["/phone/a", "/phone/b"], ["/phone/n1"], ["/phone/n2"]]);
    expect(seriesSpan(s[1])).toBeNull();
  });

  it("puts name errors in their own series and old plans in one source", () => {
    const old = { ...item("/x/o1", "2026-04-12T10:00:00"), source_root: undefined };
    const s = groupSeries([
      item("/phone/a", "2026-04-12T10:00:00"),
      { ...item("/phone/e", "2026-04-12T10:00:01", "/phone", "template “Date”: no extension"), name_error: true },
      item("/phone/b", "2026-04-12T10:00:02"),
      old,
      { ...item("/y/o2", "2026-04-12T10:00:30"), source_root: undefined },
    ]);
    expect(isNameError(s[0].items[0])).toBe(false);
    expect(isNameError({ ...item("/p/x", null), name_error: true })).toBe(true);
    // The wording of the reason does not decide.
    expect(isNameError(item("/p/y", null, "/p", "name preset 'x': y"))).toBe(false);
    expect(s.map((g) => [g.kind, g.items.map((i) => i.source)])).toEqual([
      ["ai", ["/phone/a", "/phone/b"]],
      ["ai", ["/x/o1", "/y/o2"]],
      ["name", ["/phone/e"]],
    ]);
    expect(new Set(s.map((g) => g.id)).size).toBe(3);
  });
});

describe("selection", () => {
  it("toggles one file without touching the set it came from", () => {
    const a = new Set(["/a"]);
    const b = toggle(a, "/b");
    expect([...b].sort()).toEqual(["/a", "/b"]);
    expect([...toggle(b, "/a")]).toEqual(["/b"]);
    expect([...a]).toEqual(["/a"]);
  });

  it("group checkbox: none or some → all, all → none; other groups kept", () => {
    const g = ["/a", "/b"];
    expect(groupState(new Set(), g)).toBe("none");
    expect(groupState(new Set(["/a"]), g)).toBe("some");
    expect(groupState(new Set(g), g)).toBe("all");
    expect(groupState(new Set(), [])).toBe("none");
    const some = new Set(["/a", "/other"]);
    expect([...toggleGroup(some, g)].sort()).toEqual(["/a", "/b", "/other"]);
    expect([...toggleGroup(new Set(["/a", "/b", "/other"]), g)]).toEqual(["/other"]);
  });

  it("starts with every undecided file checked", () => {
    const s = groupSeries([item("/phone/a", null), item("/phone/b", null)]);
    expect([...initialSelection(s, new Set(["/phone/b"]))]).toEqual(["/phone/a"]);
  });

  it("moves focus along files and between series", () => {
    const s = groupSeries([
      item("/phone/a", "2026-04-12T10:00:00"),
      item("/phone/b", "2026-04-12T10:00:01"),
      item("/phone/c", "2026-04-12T11:00:00"),
    ]);
    expect(nextFocus(s, null, "ArrowRight")).toBe("/phone/a");
    expect(nextFocus(s, "/phone/a", "ArrowRight")).toBe("/phone/b");
    expect(nextFocus(s, "/phone/b", "ArrowRight")).toBe("/phone/c");
    expect(nextFocus(s, "/phone/c", "ArrowRight")).toBe("/phone/c");
    expect(nextFocus(s, "/phone/a", "ArrowLeft")).toBe("/phone/a");
    expect(nextFocus(s, "/phone/b", "ArrowDown")).toBe("/phone/c");
    expect(nextFocus(s, "/phone/c", "ArrowUp")).toBe("/phone/a");
    expect(nextFocus(s, "/phone/a", "ArrowUp")).toBe("/phone/a");
    expect(nextFocus([], null, "ArrowDown")).toBeNull();
  });
});

describe("shortcut targets", () => {
  const targets = ["Photos", "Screenshots", "Videos", "Documents"];

  it("recent first, then the rest in config order, capped", () => {
    expect(shortcutTargets(["Videos", "Deleted", "Photos"], targets)).toEqual([
      "Videos",
      "Photos",
      "Screenshots",
      "Documents",
    ]);
    expect(shortcutTargets([], targets, 2)).toEqual(["Photos", "Screenshots"]);
  });

  it("remembers the last used target first, without duplicates", () => {
    expect(pushRecent(["Photos", "Videos"], "Videos")).toEqual(["Videos", "Photos"]);
    expect(pushRecent(["a", "b", "c"], "d", 3)).toEqual(["d", "a", "b"]);
  });
});

describe("decisionState", () => {
  const it0 = { ...item("/p/a", null), size: 5, mtime_ns: 10 };
  const keep = { kind: "keep" } as const;
  it("is resolved only for the file version the plan has", () => {
    expect(decisionState(it0, undefined)).toBe("none");
    expect(decisionState(it0, { path: "/p/a", action: keep, size: 5, mtime_ns: 10 })).toBe("resolved");
    expect(decisionState(it0, { path: "/p/a", action: keep, size: 6, mtime_ns: 10 })).toBe("stale");
    expect(decisionState(it0, { path: "/p/a", action: keep, size: 5, mtime_ns: 11 })).toBe("stale");
  });
});

describe("failureText", () => {
  const r = (n: number, error: string | null) => ({ source_path: `/p/${n}`, key: error ? null : "k", size: 1, mtime_ns: 0, error });
  it("lists the count and the first two errors", () => {
    expect(failureText([r(1, null), r(2, "bad 2")])).toBe("Not accepted 1 of 2: bad 2");
    expect(failureText([r(1, "e1"), r(2, "e2"), r(3, null)])).toBe("Not accepted 2 of 3: e1; e2");
    expect(failureText([r(1, "e1"), r(2, "e2"), r(3, "e3"), r(4, "e4")])).toBe("Not accepted 4 of 4: e1; e2 and 2 more");
    expect(failureText([r(1, null)])).toBeNull();
  });
});

describe("events and series reviews", () => {
  it("clusters by a 4 h gap across sources; undated files form a last event", () => {
    const ev = groupEvents([
      item("/phone/a", "2026-04-12T10:00:00", "/phone"),
      item("/cam/b", "2026-04-12T13:59:00", "/cam"), // 3 h 59 min: same event
      item("/phone/c", "2026-04-12T18:00:00"), // 4 h 1 min after b: new event
      item("/phone/d", null),
    ]);
    expect(paths(ev)).toEqual([["/phone/a", "/cam/b"], ["/phone/c"], ["/phone/d"]]);
    expect(ev[2].start).toBeNull();
  });

  it("groups review items by their series, members in the series' order", () => {
    const s = { members: ["/p/1", "/p/2", "/p/3"], best: "/p/2" };
    const g = seriesReviews([
      { ...item("/p/3", null), series: s },
      item("/p/other", null),
      { ...item("/p/1", null), series: s },
      { ...item("/p/2", null), series: s },
    ]);
    expect(g).toHaveLength(1);
    expect(g[0].best).toBe("/p/2");
    expect(g[0].items.map((i) => i.source)).toEqual(["/p/1", "/p/2", "/p/3"]);
  });
});
