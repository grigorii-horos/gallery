import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState, type ComponentType } from "react";
import { describe, expect, it, vi } from "vitest";
import { AiCondition } from "./AiCondition";
import { DateCondition } from "./DateCondition";
import { DeviceCondition } from "./DeviceCondition";
import { DuplicateCondition } from "./DuplicateCondition";
import { EventCondition } from "./EventCondition";
import { GeoCondition } from "./GeoCondition";
import { NearCondition } from "./NearCondition";
import { PathCondition } from "./PathCondition";
import { PropsCondition } from "./PropsCondition";
import { SeriesCondition } from "./SeriesCondition";
import type { CondContext, CondFormProps } from "./shared";

const ctx: CondContext = { sources: ["camera", "olga"], places: ["home", "dacha"], suggestions: ["@photo"] };

/** Renders a form on `start` and keeps what it reports, as the filter editor does. */
function Harness({ Form, start, seen }: { Form: ComponentType<CondFormProps>; start: unknown; seen: (v: unknown) => void }) {
  const [v, setV] = useState(start);
  return (
    <Form
      value={v}
      ctx={ctx}
      onChange={(n) => {
        setV(n);
        seen(n);
      }}
    />
  );
}
function setup(Form: ComponentType<CondFormProps>, start: unknown) {
  const seen = vi.fn();
  render(<Harness Form={Form} start={start} seen={seen} />);
  return { seen, last: () => seen.mock.calls.at(-1)![0], user: userEvent.setup() };
}

describe("date", () => {
  it("edits range and not, and drops empty options", async () => {
    const { last, user } = setup(DateCondition, {});
    await user.type(screen.getByLabelText("Range"), "2026-01-01..");
    expect(last()).toEqual({ range: "2026-01-01.." });
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toEqual({ range: "2026-01-01..", not: true });
    await user.clear(screen.getByLabelText("Range"));
    expect(last()).toEqual({ not: true });
  });

  it("picks the date used and months", async () => {
    const { last, user } = setup(DateCondition, {});
    await user.selectOptions(screen.getByLabelText("Date used"), "file_mtime");
    expect(last()).toEqual({ source: "file_mtime" });
    await user.click(screen.getByRole("button", { name: "Aug" }));
    await user.click(screen.getByRole("button", { name: "Jun" }));
    expect(last()).toEqual({ source: "file_mtime", months: [6, 8] });
    await user.selectOptions(screen.getByLabelText("Date used"), "");
    expect(last()).toEqual({ months: [6, 8] });
  });
});

describe("geo", () => {
  it("reads and writes named places, one or a list", async () => {
    const { last, user } = setup(GeoCondition, "@home");
    expect(screen.getByRole("button", { name: "home" })).toHaveAttribute("aria-pressed", "true");
    await user.click(screen.getByRole("button", { name: "dacha" }));
    expect(last()).toEqual(["@home", "@dacha"]);
    expect(screen.getByLabelText("Not (invert)")).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "home" }));
    expect(last()).toBe("@dacha");
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toBe("!@dacha");
  });

  it("switches to a circle and to the GPS tag", async () => {
    const { last, user } = setup(GeoCondition, "@home");
    await user.selectOptions(screen.getByLabelText("Place by"), "circle");
    expect(last()).toEqual({ lat: 0, lon: 0, km: 1 });
    await user.clear(screen.getByLabelText("Radius, km"));
    await user.type(screen.getByLabelText("Radius, km"), "3");
    expect(last()).toEqual({ lat: 0, lon: 0, km: 3 });
    await user.selectOptions(screen.getByLabelText("Place by"), "gps");
    await user.selectOptions(screen.getByLabelText("GPS tag", { selector: "select:not([aria-label='Place by'])" }), "false");
    expect(last()).toEqual({ has_gps: false });
  });
});

describe("geo circle", () => {
  it("keeps the circle form when its numbers are cleared, and accepts negative coordinates", async () => {
    const { last, user } = setup(GeoCondition, { lat: 1, lon: 2, km: 3 });
    expect(screen.getByLabelText("Latitude")).not.toHaveAttribute("min");
    expect(screen.getByLabelText("Radius, km")).toHaveAttribute("min", "0");
    for (const l of ["Latitude", "Longitude", "Radius, km"]) await user.clear(screen.getByLabelText(l));
    expect(last()).toEqual({});
    // Still the circle form, not the GPS one.
    expect(screen.getByLabelText("Place by")).toHaveValue("circle");
    await user.type(screen.getByLabelText("Latitude"), "-33.9");
    expect(last()).toEqual({ lat: -33.9 });
  });
});

describe("event", () => {
  it("edits its options", async () => {
    const { last, user } = setup(EventCondition, {});
    await user.click(screen.getByRole("button", { name: "olga" }));
    await user.type(screen.getByLabelText("Pause that starts a new event"), "6h");
    await user.type(screen.getByLabelText("At least this many files"), "10");
    await user.selectOptions(screen.getByLabelText("Away from place"), "home");
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toEqual({ sources: ["olga"], gap: "6h", min_files: 10, away_from: "@home", not: true });
  });
});

describe("near", () => {
  it("keeps sources in the table and edits time, km, min, require_gps", async () => {
    const { last, user } = setup(NearCondition, { sources: ["olga"], time: "2h" });
    await user.click(screen.getByRole("button", { name: "olga" }));
    expect(last()).toEqual({ sources: [], time: "2h" });
    await user.click(screen.getByRole("button", { name: "olga" }));
    await user.type(screen.getByLabelText("Within, km"), "1");
    await user.type(screen.getByLabelText("Neighbours needed"), "2");
    await user.click(screen.getByLabelText("Both files need GPS"));
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toEqual({ sources: ["olga"], time: "2h", km: 1, min: 2, require_gps: true, not: true });
  });
});

describe("device", () => {
  it("writes one mask as text, several as a list", async () => {
    const { last, user } = setup(DeviceCondition, {});
    await user.type(screen.getByLabelText("Make"), "Google{Enter}");
    expect(last()).toEqual({ make: "Google" });
    await user.type(screen.getByLabelText("Model"), "Pixel*{Enter}Nexus*{Enter}");
    expect(last()).toEqual({ make: "Google", model: ["Pixel*", "Nexus*"] });
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toMatchObject({ not: true });
  });
});

describe("props", () => {
  it("edits ranges, lists, orientation and audio", async () => {
    const { last, user } = setup(PropsCondition, {});
    await user.type(screen.getByLabelText("Size"), "..500KB");
    await user.type(screen.getByLabelText("Sharpness (0-100)"), "<40");
    await user.type(screen.getByLabelText("Video codec"), "hevc, h264");
    await user.selectOptions(screen.getByLabelText("Orientation"), "portrait");
    await user.selectOptions(screen.getByLabelText("Audio track"), "false");
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toEqual({ size: "..500KB", sharpness: "<40", video_codec: ["hevc", "h264"], orientation: "portrait", has_audio: false, not: true });
    await user.selectOptions(screen.getByLabelText("Audio track"), "");
    expect(last()).not.toHaveProperty("has_audio");
  });
});

describe("duplicate", () => {
  it("defaults to the scan, adds output folders", async () => {
    const { last, user } = setup(DuplicateCondition, {});
    expect(screen.getByRole("button", { name: "other files of the scan" })).toHaveAttribute("aria-pressed", "true");
    await user.click(screen.getByRole("button", { name: "output folders" }));
    expect(last()).toEqual({ of: ["sources", "outputs"] });
    await user.click(screen.getByLabelText("Not (invert)"));
    expect(last()).toEqual({ of: ["sources", "outputs"], not: true });
  });
});

describe("duplicate scope", () => {
  it("lets the last scope be switched off instead of snapping back", async () => {
    const { last, user } = setup(DuplicateCondition, {});
    await user.click(screen.getByRole("button", { name: "other files of the scan" }));
    expect(last()).toEqual({ of: [] });
    expect(screen.getByRole("button", { name: "other files of the scan" })).toHaveAttribute("aria-pressed", "false");
  });
});

describe("series", () => {
  it("edits gap, similarity and the best-shot rule", async () => {
    const { last, user } = setup(SeriesCondition, {});
    await user.type(screen.getByLabelText("Shots closer than"), "8s");
    await user.click(screen.getByLabelText("Must look alike"));
    await user.selectOptions(screen.getByLabelText("Best shot by"), "size");
    expect(last()).toEqual({ gap: "8s", similar: false, by: "size" });
    await user.click(screen.getByLabelText("Must look alike"));
    await user.selectOptions(screen.getByLabelText("Best shot by"), "");
    expect(last()).toEqual({ gap: "8s" });
  });
});

describe("path", () => {
  it("writes one glob as text, several as a list", async () => {
    const { last, user } = setup(PathCondition, []);
    await user.type(screen.getByLabelText("Path patterns"), "WhatsApp/**{Enter}");
    expect(last()).toBe("WhatsApp/**");
    await user.type(screen.getByLabelText("Path patterns"), "!**/Sent/**{Enter}");
    expect(last()).toEqual(["WhatsApp/**", "!**/Sent/**"]);
  });
});

describe("ai", () => {
  it("edits the description", async () => {
    const onChange = vi.fn();
    render(<AiCondition value="" onChange={onChange} />);
    await userEvent.type(screen.getByLabelText("Description for the AI"), "x");
    expect(onChange).toHaveBeenCalledWith("x");
  });
});
