import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppData } from "../store";
import type { EditOp, EditOutcome, FilterDraft, TargetDraft } from "../editor/model";

const filter = (over: Partial<FilterDraft> = {}): FilterDraft => ({ sources: ["camera"], match: null, exclude: [], ai: null, expand: null, conditions: {}, ...over });
const target = (name: string, kind: TargetDraft["kind"], filters: FilterDraft[]): TargetDraft => ({
  name,
  kind,
  filters,
  outputs: kind === "folders" ? [{ dir: "~/Out", server_dir: null, content: [], rename: [] }] : [],
});

const base: EditOutcome = {
  version: "v1",
  changed: false,
  saved: false,
  error: null,
  warnings: [],
  renames: [],
  draft: {
    sources: [
      { id: "camera", path: "~/Cam", server_path: null, recursive: false, skip: null, ai_prefix: null, ai_prefix_file: null, ai_suffix: null, ai_suffix_file: null },
      { id: "olga", path: "~/Olga", server_path: null, recursive: false, skip: null, ai_prefix: null, ai_prefix_file: null, ai_suffix: null, ai_suffix_file: null },
    ],
    places: [{ name: "home", lat: 47.0245, lon: 28.8323, km: 2 }],
    targets: [
      target("Duplicates", "trash", [filter({ conditions: { duplicate: { of: ["sources"] } } })]),
      target("Night", "keep", [filter({ conditions: { date: { time: "22:00..05:00" } } })]),
      target("Docs", "folders", [filter({ ai: "Documents" })]),
    ],
    masks: [],
  },
};

const data: AppData = {
  configPath: "/c/gallery.toml",
  status: null,
  summary: null,
  loadError: null,
  builtins: { name: [], content: [], masks: [] },
  loading: false,
  setConfigPath: () => {},
  reload: () => {},
};

type Args = { ops: EditOp[] };
const invoke = vi.fn<(cmd: string, args: Args) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args: Args) => invoke(cmd, args) }));

const { useConfigEditor } = await import("../editor/useConfigEditor");
const { TargetsScreen } = await import("./TargetsScreen");
const { applyLanguageSetting } = await import("../i18n");

function Targets() {
  const editor = useConfigEditor(data.configPath, () => {});
  return <TargetsScreen data={data} editor={editor} />;
}

const lastOps = () => {
  const calls = invoke.mock.calls.filter(([c]) => c === "config_draft");
  return calls[calls.length - 1][1].ops;
};
const updated = () => {
  const op = lastOps().at(-1)!;
  if (op.op !== "update_target") throw new Error(`got ${op.op}`);
  return op.target;
};

describe("ordered target list", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") return { ...base, changed: args.ops.length > 0 };
      throw new Error(cmd);
    });
  });

  it("is one list: kind and AI badges, no stage headers", async () => {
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    const items = screen.getAllByRole("listitem").filter((li) => li.querySelector(".target"));
    expect(items).toHaveLength(3);
    expect(within(items[0]).getByText("Trash")).toBeInTheDocument();
    expect(within(items[1]).getByText("Keep")).toBeInTheDocument();
    expect(within(items[2]).getByText("AI")).toBeInTheDocument();
    expect(within(items[2]).queryByText("Trash")).toBeNull();
    expect(screen.queryByText(/Masks before AI|Masks after AI/)).toBeNull();
  });

  it("moves a target with Down", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "Down" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "move_target", from: 0, to: 1 }]));
  });

  it("moves a target with Up from the second place", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByText("Night"));
    await user.click(screen.getByRole("button", { name: "Up" }));
    await waitFor(() => expect(lastOps()).toEqual([{ op: "move_target", from: 1, to: 0 }]));
  });

  it("moves a target by dragging it", async () => {
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    const items = screen.getAllByRole("listitem").filter((li) => li.querySelector(".target"));
    const rect = { top: 0, height: 10, bottom: 10, left: 0, right: 10, width: 10, x: 0, y: 0, toJSON: () => ({}) };
    items[2].getBoundingClientRect = () => rect;
    const dt = { setData: () => {}, effectAllowed: "" };
    fireEvent.dragStart(items[0], { dataTransfer: dt });
    fireEvent.dragOver(items[2], { dataTransfer: dt, clientY: 9 });
    fireEvent.drop(items[2], { dataTransfer: dt });
    await waitFor(() => expect(lastOps()).toEqual([{ op: "move_target", from: 0, to: 2 }]));
  });

  it("changes the kind and hides the outputs of a trash target", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    expect(screen.queryByText("Outputs")).toBeNull();
    await user.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "keep");
    await waitFor(() => expect(updated().kind).toBe("keep"));
  });
});

describe("condition menu and forms", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") return { ...base, changed: args.ops.length > 0 };
      throw new Error(cmd);
    });
  });

  it("lists the ten conditions and disables the ones the filter has", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "+ condition" }));
    const items = screen.getAllByRole("menuitem");
    expect(items.map((i) => i.textContent)).toEqual(["Date", "Place", "Event", "Near", "Device", "Properties", "Duplicate", "Series", "Path", "AI"]);
    expect(screen.getByRole("menuitem", { name: "Duplicate" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Date" })).toBeEnabled();
  });

  it("moves focus into the menu and closes it with Escape, back to its button", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    const open = screen.getByRole("button", { name: "+ condition" });
    await user.click(open);
    // The first choice that is not disabled (Duplicate is taken).
    expect(screen.getByRole("menuitem", { name: "Date" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(open).toHaveFocus();
  });

  it("adds a condition from the menu", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "+ condition" }));
    await user.click(screen.getByRole("menuitem", { name: "Near" }));
    await waitFor(() => expect(updated().filters[0].conditions).toEqual({ duplicate: { of: ["sources"] }, near: { sources: [], time: "2h" } }));
    expect(screen.getByRole("group", { name: "Neighbours among" })).toBeInTheDocument();
  });

  it("edits sub-options and the not toggle, and removes the condition", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByRole("button", { name: "+ condition" }));
    await user.click(screen.getByRole("menuitem", { name: "Near" }));
    const near = screen.getByRole("group", { name: "Near" });
    await user.click(within(near).getByRole("button", { name: "olga" }));
    await user.clear(within(near).getByLabelText("Within, km"));
    await user.type(within(near).getByLabelText("Within, km"), "1.5");
    await user.click(within(near).getByLabelText("Not (invert)"));
    await waitFor(() => expect(updated().filters[0].conditions.near).toEqual({ sources: ["olga"], time: "2h", km: 1.5, not: true }));
    await user.click(within(near).getByRole("button", { name: "Remove Near" }));
    await waitFor(() => expect(updated().filters[0].conditions).toEqual({ duplicate: { of: ["sources"] } }));
  });

  it("edits the date condition's time of day", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByText("Night"));
    const date = screen.getByRole("group", { name: "Date" });
    expect(within(date).getByLabelText("Time of day")).toHaveValue("22:00..05:00");
    await user.click(within(date).getByRole("button", { name: /^Mon/ }));
    await waitFor(() => expect(updated().filters[0].conditions.date).toEqual({ time: "22:00..05:00", weekdays: ["mon"] }));
  });

  it("disables expand with a hint when the filter has AI, and sets it otherwise", async () => {
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.click(screen.getByText("Docs"));
    expect(screen.getByLabelText("Expand to the whole event")).toBeDisabled();
    expect(screen.getByText("Not available together with an AI description.")).toBeInTheDocument();
    await user.click(screen.getByText("Night"));
    await user.click(screen.getByLabelText("Expand to the whole event"));
    await waitFor(() => expect(updated().filters[0].expand).toBe("event"));
  });

  it("renders the labels in Russian", async () => {
    await applyLanguageSetting("ru");
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: /Название|название/ });
    expect(document.querySelector(".badge.kind-trash")).toHaveTextContent("Корзина");
    await user.click(screen.getByRole("button", { name: "+ условие" }));
    expect(screen.getByRole("menuitem", { name: "Устройство" })).toBeInTheDocument();
  });
});

describe("kind switch and filter removal", () => {
  const withDraft = (targets: TargetDraft[]) => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "config_draft") return { ...base, draft: { ...base.draft, targets }, changed: args.ops.length > 0 };
      throw new Error(cmd);
    });
  };

  it("asks before switching a folders target with outputs to trash, then drops the outputs", async () => {
    withDraft([target("Docs", "folders", [filter()])]);
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    expect(screen.getByText("Outputs")).toBeInTheDocument();
    await user.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "trash");
    expect(screen.getByText("This kind has no outputs: switching removes this target's outputs.")).toBeInTheDocument();
    expect(invoke.mock.calls.filter(([c]) => c === "config_draft").every(([, a]) => a.ops.length === 0)).toBe(true);
    await user.click(screen.getByRole("button", { name: "Remove outputs and switch" }));
    await waitFor(() => expect(updated()).toMatchObject({ kind: "trash", outputs: [] }));
  });

  it("keeps the folders kind and outputs when the switch is cancelled", async () => {
    withDraft([target("Docs", "folders", [filter()])]);
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "keep");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByText("This kind has no outputs: switching removes this target's outputs.")).toBeNull();
    expect(screen.getByRole("combobox", { name: "Kind" })).toHaveValue("folders");
    expect(screen.getByText("Outputs")).toBeInTheDocument();
    expect(invoke.mock.calls.filter(([c]) => c === "config_draft").every(([, a]) => a.ops.length === 0)).toBe(true);
  });

  it("switches a folders target without outputs at once", async () => {
    withDraft([{ ...target("Empty", "folders", [filter()]), outputs: [] }]);
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    await user.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "keep");
    await waitFor(() => expect(updated()).toMatchObject({ kind: "keep", outputs: [] }));
  });

  it("shows and saves the second filter's own codec list after the first is removed", async () => {
    withDraft([
      target("Videos", "folders", [
        filter({ conditions: { props: { video_codec: ["hevc"] } } }),
        filter({ conditions: { props: { video_codec: ["h264", "av1"] } } }),
      ]),
    ]);
    const user = userEvent.setup();
    render(<Targets />);
    await screen.findByRole("textbox", { name: "Target name" });
    const codecs = () => screen.getAllByRole("textbox", { name: "Video codec" });
    expect(codecs().map((c) => (c as HTMLInputElement).value)).toEqual(["hevc", "h264, av1"]);
    await user.click(screen.getAllByRole("button", { name: "Remove filter" })[0]);
    await waitFor(() => expect(updated().filters).toHaveLength(1));
    expect(codecs().map((c) => (c as HTMLInputElement).value)).toEqual(["h264, av1"]);
    await user.type(codecs()[0], ", vp9");
    await waitFor(() => expect(updated().filters[0].conditions.props).toEqual({ video_codec: ["h264", "av1", "vp9"] }));
  });
});
