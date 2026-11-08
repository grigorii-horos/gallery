import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { EffectPanel } from "./EffectPanel";
import type { Counts, EffectDiff } from "./effect";
import { targetLine } from "./effect";

const c = (o: Partial<Counts> = {}): Counts => ({
  compressed: 0,
  renamed: 0,
  unchanged: 0,
  excluded: 0,
  review: 0,
  unmatched: 0,
  waiting_ai: 0,
  ...o,
});

const diff: EffectDiff = {
  files: 5,
  before: c({ unchanged: 3, unmatched: 2 }),
  after: c({ compressed: 3, unmatched: 1, waiting_ai: 1 }),
  changed: 4,
  targets: [{ name: "pngs", before: c({ unchanged: 3 }), after: c({ compressed: 3 }), changed: 3 }],
  examples: [
    {
      outcome: "compressed",
      count: 7,
      files: [
        {
          path: "/m/20260716_211554.png",
          before: { outcome: "unchanged", target: "pngs", label: "pngs: unchanged" },
          after: { outcome: "compressed", target: "pngs", label: "pngs: png-avif → 20260716_211554.avif" },
        },
      ],
    },
  ],
  rescanned: false,
  scan_ms: 0,
  diff_ms: 12,
};

describe("effect panel", () => {
  it("shows totals, per-target deltas and expandable before to after examples", async () => {
    const user = userEvent.setup();
    render(<EffectPanel state={{ diff, loading: false, error: null }} />);
    const panel = screen.getByRole("region", { name: "Effect" });
    expect(panel).toHaveTextContent("4 of 5 files will change");
    expect(panel).toHaveTextContent("Unmatched: 2 → 1");
    expect(panel).toHaveTextContent("Waiting for AI: 1");
    expect(panel).toHaveTextContent("pngs takes 3 (was 3): 3 compressed");
    expect(within(panel).queryByText("20260716_211554.png")).toBeNull();
    await user.click(screen.getByRole("button", { name: /compressed: 7/ }));
    expect(panel).toHaveTextContent("20260716_211554.png: pngs: unchanged → pngs: png-avif → 20260716_211554.avif");
    expect(panel).toHaveTextContent("…and 6 more");
  });

  it("says when nothing changes, and shows loading and errors", () => {
    const none = { ...diff, changed: 0, examples: [] };
    const { rerender } = render(<EffectPanel state={{ diff: none, loading: true, error: null }} />);
    expect(screen.getByRole("region", { name: "Effect" })).toHaveTextContent("affect none of the");
    expect(screen.getByText("calculating…")).toBeInTheDocument();
    rerender(<EffectPanel state={{ diff: null, loading: false, error: "boom" }} />);
    expect(screen.getByText("boom")).toBeInTheDocument();
  });

  it("words a target line", () => {
    expect(targetLine({ name: "x", before: c({ unchanged: 1 }), after: c({ compressed: 2, renamed: 1 }), changed: 3 })).toBe(
      "takes 3 (was 1): 2 compressed, 1 renamed",
    );
  });
});
