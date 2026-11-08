import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { MaskChips, PipeEditor, type PresetChoice } from "./fields";
import { maskItemKind } from "./model";

describe("mask exclusions", () => {
  it("classifies items", () => {
    expect(maskItemKind("!*.av1.mp4")).toBe("not");
    expect(maskItemKind("!@raw")).toBe("not");
    expect(maskItemKind("@raw")).toBe("ref");
    expect(maskItemKind("*.jpg")).toBe("pattern");
  });

  it("renders a `!` item as an exclusion chip", () => {
    render(<MaskChips label="m" value={["*.mp4", "@raw", "!*.av1.mp4"]} suggestions={[]} onChange={() => {}} />);
    const chip = (t: string) => screen.getByText(t).closest(".chip")!;
    expect(chip("!*.av1.mp4").classList.contains("not")).toBe(true);
    expect(chip("!*.av1.mp4").getAttribute("title")).toMatch(/exclusion|exception/);
    expect(chip("@raw").classList.contains("ref")).toBe(true);
    expect(chip("*.mp4").classList.contains("not")).toBe(false);
  });

  it("typing `!pattern` adds an exclusion item as written", async () => {
    const onChange = vi.fn();
    render(<MaskChips label="m" value={["*.mp4"]} suggestions={[]} onChange={onChange} />);
    await userEvent.type(screen.getByLabelText("m"), "!*.av1.mp4{Enter}");
    expect(onChange).toHaveBeenCalledWith(["*.mp4", "!*.av1.mp4"]);
  });

  it("a field that only turns files away refuses `!` items and says where the exception goes", async () => {
    const onChange = vi.fn();
    render(<MaskChips label="x" value={["*.raw"]} suggestions={[]} allowNot={false} exceptionField="Matches" onChange={onChange} />);
    await userEvent.type(screen.getByLabelText("x"), "!*.jpg{Enter}");
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("alert").textContent).toMatch(/“!”.*“Matches”/);
  });

  it("an existing `!` chip in such a field is flagged, with no exclusion tooltip", () => {
    render(<MaskChips label="x" value={["!*.jpg"]} suggestions={[]} allowNot={false} onChange={() => {}} />);
    const chip = screen.getByText("!*.jpg").closest(".chip")!;
    expect(chip.getAttribute("title")).not.toMatch(/a name with this mask does not match/);
    expect(chip.getAttribute("title")).toMatch(/not allowed/);
    expect(chip.classList.contains("bad")).toBe(true);
  });

  it("the placeholder offers `!` only where it is allowed", () => {
    const { rerender } = render(<MaskChips label="x" value={[]} suggestions={[]} onChange={() => {}} />);
    expect(screen.getByLabelText("x").getAttribute("placeholder")).toMatch(/!/);
    rerender(<MaskChips label="x" value={[]} suggestions={[]} allowNot={false} onChange={() => {}} />);
    expect(screen.getByLabelText("x").getAttribute("placeholder")).not.toMatch(/!/);
  });
});

describe("pipe editor with bundles", () => {
  const presets: PresetChoice[] = [
    { name: "avif-high", mask: "@photo-original", description: "AVIF", own: false },
    { name: "compress-all", mask: "*", description: "set: avif-high + png-avif", own: false, bundle: true },
  ];

  it("offers bundles in their own group and adds one as a step", async () => {
    const onChange = vi.fn();
    render(<PipeEditor steps={[]} onChange={onChange} presets={presets} maskSuggestions={[]} label="c" empty="empty" />);
    const pick = screen.getByRole("combobox", { name: /Add step/ });
    expect(pick.querySelector("optgroup[label='Sets']")?.textContent).toContain("compress-all");
    await userEvent.selectOptions(pick, "compress-all");
    expect(onChange).toHaveBeenCalledWith([{ preset: "compress-all", mask: null }]);
  });

  it("shows the expansion on a bundle chip and offers no mask override for it", async () => {
    render(<PipeEditor steps={[{ preset: "compress-all", mask: null }]} onChange={() => {}} presets={presets} maskSuggestions={[]} label="c" empty="empty" />);
    expect(screen.getByText("set: avif-high + png-avif")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /compress-all/ }));
    expect(screen.queryByRole("checkbox")).toBeNull();
    expect(screen.getByText(/the masks are set on its steps/)).toBeInTheDocument();
  });
});
