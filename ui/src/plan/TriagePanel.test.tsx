import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ThumbQueue } from "./thumbQueue";
import type { ChangePreview, SuggestedAction, TriageView } from "./triage";

const op = { op: "update_target", stage: "after_ai", index: 0, target: { name: "pics" } };
const addAction = (target: string): SuggestedAction => ({
  kind: "add_to_target",
  label: `Add *.webm to the match of target “${target}”`,
  target,
  dir: null,
  ops: [{ ...op, index: target === "pics" ? 0 : 1 } as never],
});
const view: TriageView = {
  version: "v1",
  total: 1971,
  groups: [
    {
      source: "screenshots",
      dir: "Screenshots",
      ext: "webm",
      count: 1500,
      size: 3 * 1024 * 1024,
      samples: ["/m/Screenshots/a.webm", "/m/Screenshots/b.webm"],
      reason: "Extension .webm is not in the match [*.jpg] of target pics",
      actions: [
        addAction("pics"),
        addAction("clips"),
        { kind: "skip_folder", label: "skip", target: null, dir: null, ops: [{ op: "update_source" }] as never },
        { kind: "skip_source", label: "skip all", target: null, dir: null, ops: [{ op: "update_source" }] as never },
        {
          kind: "create_target",
          label: "Create target…",
          target: "screenshots .webm",
          dir: "/m/Screenshots",
          ops: [{ op: "add_target" }] as never,
        },
      ],
    },
    {
      source: "screenshots",
      dir: "",
      ext: "",
      count: 3,
      size: 10,
      samples: ["/m/README"],
      reason: "File without an extension",
      actions: [],
    },
  ],
};
const preview = (over: Partial<ChangePreview> = {}): ChangePreview => ({
  version: "v1",
  changed: true,
  diff: [
    { kind: "ctx", text: "[[targets.after_ai.filters]]" },
    { kind: "del", text: 'match = "*.jpg"' },
    { kind: "add", text: 'match = ["*.jpg", "*.webm"]' },
  ],
  error: null,
  warnings: [],
  ...over,
});

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const { TriagePanel } = await import("./TriagePanel");
const queue = { cached: () => undefined, request: () => () => {}, clearErrors: () => {} } as unknown as ThumbQueue;

function setup(onSaved = vi.fn()) {
  render(<TriagePanel configPath={null} version="v1" queue={queue} onConfigSaved={onSaved} />);
  return onSaved;
}

describe("triage panel", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd) => {
      if (cmd === "triage_view") return view;
      if (cmd === "config_preview_change") return preview();
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("lists groups with count, size and reason, and expands to samples and actions", async () => {
    const user = userEvent.setup();
    setup();
    const row = (await screen.findByText(/is not in the match/)).closest("tr")!;
    expect(row).toHaveTextContent("screenshots/Screenshots");
    expect(row).toHaveTextContent(".webm");
    expect(row.textContent).toMatch(/1,500/);
    expect(row).toHaveTextContent("3.0 MB");
    await user.click(screen.getByRole("button", { name: "screenshots/Screenshots .webm" }));
    const samples = screen.getByRole("list", { name: "Example files" });
    expect(within(samples).getByText("a.webm")).toBeInTheDocument();
    expect(within(samples).getByText(/and 1,498 more/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add to target…" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Always ignore in this folder" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create target…" })).toBeInTheDocument();
    expect(invoke).toHaveBeenCalledWith("triage_view", { configPath: null });
  });

  it("offers no edits for files without extension", async () => {
    const user = userEvent.setup();
    setup();
    await user.click(await screen.findByRole("button", { name: /screenshots no extension/ }));
    expect(screen.getByText(/edit the config by hand/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Create target…" })).toBeNull();
  });

  it("previews the exact change of the chosen target, then saves with the file version", async () => {
    const user = userEvent.setup();
    const onSaved = setup();
    await user.click(await screen.findByRole("button", { name: "screenshots/Screenshots .webm" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Target" }), "clips");
    await user.click(screen.getByRole("button", { name: "Add to target…" }));
    const diff = await screen.findByLabelText("TOML change");
    expect(diff).toHaveTextContent('- match = "*.jpg"');
    expect(diff).toHaveTextContent('+ match = ["*.jpg", "*.webm"]');
    expect(invoke).toHaveBeenCalledWith("config_preview_change", {
      path: null,
      version: "v1",
      ops: [{ ...op, index: 1 }],
    });
    // Nothing is saved before the click.
    expect(invoke).not.toHaveBeenCalledWith("config_edit", expect.anything());
    invoke.mockImplementation(async (cmd) => {
      if (cmd === "config_edit") return { saved: true, error: null, version: "v2" };
      if (cmd === "triage_view") return { ...view, version: "v2" };
      throw new Error(cmd);
    });
    await user.click(screen.getByRole("button", { name: "Save to config" }));
    expect(invoke).toHaveBeenCalledWith("config_edit", { path: null, version: "v1", ops: [{ ...op, index: 1 }] });
    expect(await screen.findByText(/Saved to the config/)).toBeInTheDocument();
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("does not let an invalid change be saved", async () => {
    const user = userEvent.setup();
    invoke.mockImplementation(async (cmd) =>
      cmd === "triage_view" ? view : preview({ error: { message: "target name is empty", path: null } }),
    );
    setup();
    await user.click(await screen.findByRole("button", { name: "screenshots/Screenshots .webm" }));
    await user.click(screen.getByRole("button", { name: "Always ignore in this folder" }));
    expect(await screen.findByText("target name is empty")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save to config" })).toBeDisabled();
  });

  it("refuses to preview over a config that changed and offers to refresh", async () => {
    const user = userEvent.setup();
    invoke.mockImplementation(async (cmd) => {
      if (cmd === "triage_view") return view;
      throw { kind: "changed", message: "the file changed after the editor read it", op: null };
    });
    setup();
    await user.click(await screen.findByRole("button", { name: "screenshots/Screenshots .webm" }));
    await user.click(screen.getByRole("button", { name: "Always ignore in the whole source" }));
    expect(await screen.findByText(/file changed/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save to config" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Refresh triage" })).toBeInTheDocument();
  });

  it("create target: typed name and folder go to Rust, which returns the operation", async () => {
    const user = userEvent.setup();
    setup();
    await user.click(await screen.findByRole("button", { name: "screenshots/Screenshots .webm" }));
    await user.click(screen.getByRole("button", { name: "Create target…" }));
    const name = await screen.findByRole("textbox", { name: "Target name" });
    expect(name).toHaveValue("screenshots .webm");
    expect(screen.getByRole("textbox", { name: "Target folder" })).toHaveValue("/m/Screenshots");
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "triage_create_ops")
        return { kind: "create_target", label: "x", target: (args as { name: string }).name, dir: "/m/Screenshots", ops: [{ op: "add_target" }] };
      return preview();
    });
    await user.type(name, "!");
    expect(invoke).toHaveBeenCalledWith("triage_create_ops", {
      configPath: null,
      version: "v1",
      source: "screenshots",
      relDir: "Screenshots",
      ext: "webm",
      name: "screenshots .webm!",
      dir: "/m/Screenshots",
    });
  });
});
