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
  warnings: [{ path: "targets[0].outputs[0].content", message: "Double lossy re-encoding: *.jpg" }],
  renames: [],
  draft: {
    sources: [
      {
        id: "camera",
        path: "~/Captures/Camera",
        server_path: null,
        recursive: false,
        skip: null,
        ai_prefix: null,
        ai_prefix_file: "prompts/camera.md",
        ai_suffix: null,
        ai_suffix_file: null,
      },
    ],
    targets: [
      {
        name: "Timeline",
        kind: "folders",
        filters: [{ sources: ["camera"], match: ["@photo"], exclude: [], ai: null, expand: null, conditions: {} }],
        outputs: [{ dir: "~/Timeline", server_dir: null, content: [{ preset: "avif-high", mask: null }], rename: [] }],
      },
    ],
    masks: ["mine"],
  },
};

const builtins: PresetSet = {
  name: [{ name: "capture-date", kind: "name", description: "", mask: "*", mask_expanded: "*", params: {} }],
  content: [
    { name: "avif-high", kind: "photo", description: "", mask: "@photo-original", mask_expanded: "", params: {} },
    { name: "meta-full", kind: "metadata", description: "", mask: "*", mask_expanded: "*", params: {} },
  ],
  masks: [{ name: "photo", source: "", expanded: "" }],
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

type Args = { ops: EditOp[]; sourceId?: string };
const invoke = vi.fn<(cmd: string, args: Args) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: Args) => invoke(cmd, args) }));

const { useConfigEditor } = await import("../editor/useConfigEditor");
const { TargetsScreen } = await import("./TargetsScreen");
const { SourcesScreen } = await import("./SourcesScreen");

function Targets() {
  const editor = useConfigEditor(data.configPath, () => {});
  return <TargetsScreen data={data} editor={editor} />;
}
function Sources() {
  const editor = useConfigEditor(data.configPath, () => {});
  return <SourcesScreen data={data} editor={editor} />;
}

const lastOps = () => {
  const calls = invoke.mock.calls.filter(([c]) => c === "config_draft");
  return calls[calls.length - 1][1].ops;
};

describe("editor screens", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") return { ...base, changed: args.ops.length > 0 };
      if (cmd === "config_preview_prompt") return { prompt: "PROMPT for " + args.sourceId, categories: ["Documents"] };
      throw new Error(cmd);
    });
  });

  it("edits a target's name and pipe, shows warnings inline", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    const name = await screen.findByRole("textbox", { name: "Target name" });
    expect(screen.getByText(/Double lossy re-encoding/)).toBeInTheDocument();
    // The step chip shows the preset's own mask.
    expect(screen.getByText("@photo-original")).toBeInTheDocument();

    await user.clear(name);
    await user.type(name, "Feed");
    await user.selectOptions(screen.getByRole("combobox", { name: "Add step: Content, output 1" }), "meta-full");
    await waitFor(() => {
      const ops = lastOps();
      expect(ops).toHaveLength(1);
      const op = ops[0];
      expect(op.op === "update_target" && op.target.name).toBe("Feed");
      expect(op.op === "update_target" && op.target.outputs[0].content.map((s) => s.preset)).toEqual([
        "avif-high",
        "meta-full",
      ]);
    });
    expect(screen.getByRole("status")).toHaveTextContent("Unsaved: 1 change");
  });

  it("adds a target at the end of the list", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "Add target: Targets" }));
    await waitFor(() => expect(lastOps()[0]).toMatchObject({ op: "add_target", kind: "folders", target: { name: "New target" } }));
  });

  it("gives a filter an AI description from the condition menu", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "+ condition" }));
    await user.click(screen.getByRole("menuitem", { name: "AI" }));
    await waitFor(() => {
      const op = lastOps()[0];
      expect(op.op === "update_target" && op.target.filters[0].ai).toBe("");
    });
  });

  it("adds mask chips with @ suggestions", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    const input = await screen.findByRole("combobox", { name: "Mask: except" });
    await user.type(input, "@mine{Enter}");
    await waitFor(() => {
      const op = lastOps()[0];
      expect(op.op === "update_target" && op.target.filters[0].exclude).toEqual(["@mine"]);
    });
    const options = [...document.querySelectorAll("datalist option")].map((o) => (o as HTMLOptionElement).value);
    expect(options).toContain("@photo");
  });

  it("edits a source and previews its prompt", async () => {
    const user = userEvent.setup();
    render(<Sources />);
    const form = await screen.findByRole("complementary", { name: "Selected source" });
    await user.click(within(form).getByRole("checkbox", { name: "Scan subfolders too" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "update_source", id: "camera", source: { ...base.draft.sources[0], recursive: true } }]));
    await user.click(within(form).getByRole("button", { name: "Show the final prompt" }));
    expect(await within(form).findByText("PROMPT for camera")).toBeInTheDocument();
  });
});
