import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Estimate } from "../api";

const estimate: Estimate = {
  targets: [],
  total: {
    name: "",
    files: 1200,
    input_bytes: 0,
    output_bytes: { low: 0, mid: 0, high: 0 },
    saving_bytes: { low: 1024 ** 3, mid: 2 * 1024 ** 3, high: 3 * 1024 ** 3 },
    seconds: { low: 480, mid: 720, high: 1140 },
    trash: 1200,
    renamed: 0,
    kept: 0,
  },
  jobs: 4,
  source: { kind: "defaults" },
  source_label: "rough estimate",
};

let failure: string | null = null;
const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async () => {
  if (failure) throw new Error(failure);
  return estimate;
});
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));

const { EstimateBlock } = await import("./EstimateBlock");

describe("EstimateBlock", () => {
  beforeEach(() => {
    invoke.mockClear();
  });

  it("shows files, saving and the time range with their source", async () => {
    failure = null;
    render(<EstimateBlock planPath="/p/a.plan.json" />);
    expect(screen.getByText("Estimating…")).toBeInTheDocument();
    const box = await screen.findByRole("region", { name: "Estimate" });
    expect(within(box).getByText(/^1,200$/)).toBeInTheDocument();
    expect(within(box).getByText("≈ 2.0 GB (1.0 GB – 3.0 GB)")).toBeInTheDocument();
    expect(within(box).getByText("≈ 12 min (8 min – 19 min)")).toBeInTheDocument();
    expect(within(box).getByText(/Estimate: rough estimate/)).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("estimate_plan", { planPath: "/p/a.plan.json", pilot: null });
  });

  it("says quietly when the estimate fails", async () => {
    failure = "plan unreadable";
    render(<EstimateBlock planPath="/p/a.plan.json" />);
    expect(await screen.findByText("Estimate unavailable: plan unreadable")).toBeInTheDocument();
  });
});
