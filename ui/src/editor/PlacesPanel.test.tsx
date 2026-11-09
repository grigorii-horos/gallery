import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EditOp, EditOutcome } from "./model";

const base: EditOutcome = {
  version: "v1",
  changed: false,
  saved: false,
  error: null,
  warnings: [],
  renames: [],
  draft: { sources: [], places: [{ name: "home", lat: 47.0245, lon: 28.8323, km: 2 }], targets: [], masks: [] },
};

type Args = { ops: EditOp[] };
const invoke = vi.fn<(cmd: string, args: Args) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: Args) => invoke(cmd, args) }));

const { useConfigEditor } = await import("./useConfigEditor");
const { PlacesPanel, parseLatLon } = await import("./PlacesPanel");

function Panel() {
  const editor = useConfigEditor("/c/gallery.toml", () => {});
  return <PlacesPanel editor={editor} />;
}
const lastOps = () => {
  const calls = invoke.mock.calls.filter(([c]) => c === "config_draft");
  return calls[calls.length - 1][1].ops;
};

describe("parseLatLon", () => {
  it("reads a pasted pair", () => {
    expect(parseLatLon("47.0245, 28.8323")).toEqual({ lat: 47.0245, lon: 28.8323 });
    expect(parseLatLon("-33.9 151.2")).toEqual({ lat: -33.9, lon: 151.2 });
    expect(parseLatLon("47.0245")).toBeNull();
    expect(parseLatLon("home")).toBeNull();
  });
});

describe("places panel", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") return { ...base, changed: args.ops.length > 0 };
      throw new Error(cmd);
    });
  });

  it("lists the places of the config", async () => {
    render(<Panel />);
    const row = (await screen.findByText("home")).closest("li")!;
    expect(within(row).getByLabelText("Latitude")).toHaveValue("47.0245");
    expect(within(row).getByLabelText("Radius, km")).toHaveValue("2");
  });

  it("fills latitude and longitude from a pasted pair, then adds the place", async () => {
    const user = userEvent.setup();
    render(<Panel />);
    await screen.findByText("home");
    const add = screen.getByRole("button", { name: "Add place" });
    expect(add).toBeDisabled();
    const form = add.closest(".field-body") as HTMLElement;
    await user.type(within(form).getByLabelText("Name"), "dacha");
    await user.click(within(form).getByLabelText("Latitude"));
    await user.paste("46.98, 28.70");
    expect(within(form).getByLabelText("Latitude")).toHaveValue("46.98");
    expect(within(form).getByLabelText("Longitude")).toHaveValue("28.7");
    await user.click(add);
    await waitFor(() => expect(lastOps()).toEqual([{ op: "set_place", name: "dacha", place: { lat: 46.98, lon: 28.7, km: 1 } }]));
  });

  it("edits a place's radius", async () => {
    const user = userEvent.setup();
    render(<Panel />);
    const row = (await screen.findByText("home")).closest("li")!;
    const km = within(row).getByLabelText("Radius, km");
    await user.clear(km);
    await user.type(km, "5");
    await waitFor(() => expect(lastOps()).toEqual([{ op: "set_place", name: "home", place: { lat: 47.0245, lon: 28.8323, km: 5 } }]));
  });

  it("removes a place", async () => {
    const user = userEvent.setup();
    render(<Panel />);
    await screen.findByText("home");
    await user.click(screen.getByRole("button", { name: "Remove place home" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "remove_place", name: "home" }]));
  });
});
