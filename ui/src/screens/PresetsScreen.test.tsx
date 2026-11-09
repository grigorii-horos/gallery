import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PresetSet } from "../api";
import type { AppData } from "../store";
import type { EditOp, EditOutcome } from "../editor/model";

const base: EditOutcome = {
  version: "v1",
  changed: false,
  saved: false,
  error: null,
  warnings: [],
  renames: [],
  draft: {
    sources: [],
    targets: [],
    masks: ["phone"],
    name_presets: [
      { name: "short", mask: null, template: "{date:YYYYMMDD}_{stem}.{ext}", date: "capture", skip_if: null, normalize_ext: null },
    ],
    content_presets: [
      { name: "tiny", kind: "photo", mask: null, format: "avif", quality: 40, speed: 8 },
      { name: "pack", kind: "bundle", mask: null, steps: [{ preset: "avif-high", mask: null }, { preset: "png-avif", mask: ["*.png"] }] },
    ],
    mask_defs: [{ name: "phone", items: ["@photo", "@video"] }],
  },
  usage: { name: {}, content: { tiny: ["target “Timeline”, output 1: content"] }, masks: {} },
  own: null,
};

const builtins: PresetSet = {
  name: [{ name: "capture-date", kind: "name", description: "by capture date", mask: "*", mask_expanded: "*", params: { template: "{date}" } }],
  content: [
    { name: "avif-high", kind: "photo", description: "AVIF q72", mask: "@photo-original", mask_expanded: "*.jpg", params: { quality: 72 } },
    { name: "png-avif", kind: "photo", description: "AVIF from PNG", mask: "@png", mask_expanded: "*.png", params: { quality: 60 } },
    {
      name: "compress-all",
      kind: "bundle",
      description: "set: avif-high + png-avif",
      mask: "*",
      mask_expanded: "*",
      params: { kind: "bundle", steps: ["avif-high", "png-avif"] },
    },
  ],
  masks: [{ name: "photo", source: "*.jpg", expanded: "*.jpg" }],
};

const data: AppData = {
  configPath: "/c/gallery.toml",
  status: null,
  summary: null,
  loadError: null,
  builtins,
  loading: false,
  setConfigPath: () => {},
  reload: () => {},
};

type Args = { ops: EditOp[]; sample?: { preset: unknown; names: string[] }; test?: { names: string[] } };
const invoke = vi.fn<(cmd: string, args: Args) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: Args) => invoke(cmd, args) }));

const { useConfigEditor } = await import("../editor/useConfigEditor");
const { PresetsScreen } = await import("./PresetsScreen");

function Presets() {
  const editor = useConfigEditor(data.configPath, () => {});
  return <PresetsScreen data={data} editor={editor} />;
}

const lastOps = () => {
  const calls = invoke.mock.calls.filter(([c]) => c === "config_draft");
  return calls[calls.length - 1][1].ops;
};

describe("presets screen", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") {
        const refused = args.ops.find((o) => o.op === "remove_content_preset");
        if (refused) throw { kind: "op", message: "Content preset “tiny” is in use: target “Timeline”", op: args.ops.indexOf(refused) };
        return { ...base, changed: args.ops.length > 0 };
      }
      if (cmd === "preset_limits") return [{ kind: "photo", variant: null, key: "quality", min: 0, max: 100 }];
      if (cmd === "config_preview_name")
        return args.sample!.names.map((n) => ({ name: n, result: `20261105_${n}`, status: "renamed" }));
      if (cmd === "config_mask_test")
        return args.test!.names.map((n) => ({ name: n, matched: n.endsWith(".jpg"), excluded: false, taken: n.endsWith(".jpg") }));
      throw new Error(cmd);
    });
  });

  it("edits an own content preset and checks the range at once", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    const form = await screen.findByRole("complementary", { name: "Selected preset" });
    expect(within(form).getByText("target “Timeline”, output 1: content")).toBeInTheDocument();
    const q = within(form).getByRole("spinbutton", { name: "Quality" });
    await user.clear(q);
    await user.type(q, "120");
    expect(within(form).getByText("from 0 to 100")).toBeInTheDocument();
    await waitFor(() => {
      const op = lastOps()[0];
      expect(op.op === "update_content_preset" && op.preset.quality).toBe(120);
    });
    expect(lastOps()).toHaveLength(1);
  });

  it("copies a built-in under a free name and refuses a clashing one", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("button", { name: /avif-high/ }));
    const name = screen.getByRole("textbox", { name: "Copy name" });
    expect(name).toHaveValue("my-avif-high");
    await user.clear(name);
    await user.type(name, "tiny");
    expect(screen.getByText("this name is already in the config")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy" })).toBeDisabled();
    await user.clear(name);
    await user.type(name, "my-avif");
    await user.click(screen.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "copy_builtin", kind: "content", from: "avif-high", to: "my-avif" }]));
  });

  it("shows why a used preset cannot be removed", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("button", { name: "Delete preset" }));
    // Above the lists and next to the button.
    expect(await screen.findAllByText(/is in use: target “Timeline”/)).toHaveLength(2);
  });

  it("renames with reference update", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("button", { name: "Rename…" }));
    const to = screen.getByRole("textbox", { name: "New name" });
    await user.clear(to);
    await user.type(to, "small");
    expect(screen.getByRole("checkbox", { name: /Update references to it \(1\)/ })).toBeChecked();
    await user.click(screen.getByRole("button", { name: "Rename" }));
    await waitFor(() =>
      expect(lastOps()).toEqual([{ op: "rename_preset", kind: "content", from: "tiny", to: "small", update_refs: true }]),
    );
  });

  it("previews a name preset and a mask on sample names", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("tab", { name: /Name/ }));
    expect(await screen.findByText("20261105_20260102_030405.jpg")).toBeInTheDocument();
    const call = invoke.mock.calls.find(([c]) => c === "config_preview_name")!;
    expect(call[1].sample!.preset).toMatchObject({ name: "short", template: "{date:YYYYMMDD}_{stem}.{ext}" });

    await user.click(screen.getByRole("tab", { name: /Masks/ }));
    const rows = await screen.findAllByLabelText("matches");
    expect(rows.length).toBeGreaterThan(0);
    expect(screen.getAllByLabelText("does not match").length).toBeGreaterThan(0);
  });

  it("keeps bundles out of “Content” and lists them on “Sets”", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    expect(await screen.findByRole("button", { name: /^tiny/ })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^pack/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /^compress-all/ })).toBeNull();
    await user.click(screen.getByRole("tab", { name: /Sets/ }));
    expect(await screen.findByRole("button", { name: /^pack/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^compress-all/ })).toBeInTheDocument();
  });

  it("shows a built-in bundle's expansion read-only and copies it as own", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("tab", { name: /Sets/ }));
    await user.click(await screen.findByRole("button", { name: /^compress-all/ }));
    const form = screen.getByRole("complementary", { name: "Selected preset" });
    const steps = within(form).getByRole("list");
    expect(within(steps).getAllByRole("listitem").map((li) => li.textContent)).toEqual([
      "avif-high @photo-original",
      "png-avif @png",
    ]);
    await user.click(within(form).getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "copy_builtin", kind: "content", from: "compress-all", to: "my-compress-all" }]));
  });

  it("edits an own bundle as an ordered chip list", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("tab", { name: /Sets/ }));
    const form = await screen.findByRole("complementary", { name: "Selected preset" });
    const chips = within(form).getAllByRole("listitem").map((li) => li.textContent);
    expect(chips).toEqual(["avif-high@photo-original", "png-avif*.png"]);
    // Move the second step earlier.
    await user.click(within(form).getByRole("button", { name: /png-avif/ }));
    await user.click(within(form).getByRole("button", { name: /Earlier/ }));
    await waitFor(() => {
      const op = lastOps()[0];
      expect(op.op === "update_content_preset" && op.name === "pack" && op.preset.kind === "bundle").toBe(true);
      expect(op.op === "update_content_preset" && op.preset.steps).toEqual([
        { preset: "png-avif", mask: ["*.png"] },
        { preset: "avif-high", mask: null },
      ]);
    });
    // A bundle has no mask of its own.
    expect(within(form).queryByText("Mask")).toBeNull();
  });

  it("adds a bundle with starter steps", async () => {
    const user = userEvent.setup();
    render(<Presets />);
    await user.click(await screen.findByRole("tab", { name: /Sets/ }));
    await user.click(await screen.findByRole("button", { name: "+ set" }));
    await waitFor(() => {
      const op = lastOps()[0];
      expect(op.op === "add_content_preset" && op.preset).toMatchObject({ kind: "bundle", name: "my-pack" });
    });
  });
});
