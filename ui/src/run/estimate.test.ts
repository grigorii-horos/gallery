import { describe, expect, it } from "vitest";
import type { Estimate, TargetEstimate } from "../api";
import { estimateView, formatDuration, rangeText, signedSize } from "./estimate";

const target = (name: string, files: number, saving: [number, number, number], secs: [number, number, number]): TargetEstimate => ({
  name,
  files,
  input_bytes: 0,
  output_bytes: { low: 0, mid: 0, high: 0 },
  saving_bytes: { low: saving[0], mid: saving[1], high: saving[2] },
  seconds: { low: secs[0], mid: secs[1], high: secs[2] },
  trash: files,
  renamed: 0,
  kept: 0,
});

const MB = 1024 * 1024;

describe("estimate presentation", () => {
  it("formats durations like the core does", () => {
    expect(formatDuration(0.4)).toBe("0 s");
    expect(formatDuration(45)).toBe("45 s");
    expect(formatDuration(700)).toBe("12 min");
    expect(formatDuration(7500)).toBe("2 h 05 min");
  });

  it("writes a range, and a single number when low and high agree", () => {
    expect(rangeText({ low: 480, mid: 720, high: 1140 }, formatDuration)).toBe("≈ 12 min (8 min – 19 min)");
    expect(rangeText({ low: 5, mid: 5, high: 5 }, (n) => `${n} s`)).toBe("≈ 5 s");
  });

  it("shows a negative saving with a minus", () => {
    expect(signedSize(-2 * MB)).toBe("−2.0 MB");
  });

  it("summarizes the total, and per target only when there are several", () => {
    const a = target("Photos", 10, [MB, 2 * MB, 3 * MB], [60, 120, 240]);
    const b = target("Videos", 2, [MB, 2 * MB, 3 * MB], [600, 900, 1800]);
    const total = target("", 12, [2 * MB, 4 * MB, 6 * MB], [660, 1020, 2040]);
    const e: Estimate = {
      targets: [a, b],
      total,
      jobs: 4,
      source: { kind: "history", samples: 31, guessed_files: 0 },
      source_label: "from past runs (31 files)",
    };
    const v = estimateView(e);
    expect(v.files).toBe(12);
    expect(v.saving).toBe("≈ 4.0 MB (2.0 MB – 6.0 MB)");
    expect(v.time).toBe("≈ 17 min (11 min – 34 min)");
    expect(v.source).toBe("from past runs (31 files)");
    expect(v.rows.map((r) => r.name)).toEqual(["Photos", "Videos"]);
    expect(estimateView({ ...e, targets: [a] }).rows).toEqual([]);
  });
});
