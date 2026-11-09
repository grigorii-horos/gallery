import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppStatus, ConfigSummary, PresetSet } from "./api";
import type { EditOutcome } from "./editor/model";

const status: AppStatus = {
  config_path: "/home/u/.config/gallery/gallery.toml",
  config_ok: true,
  config_error: null,
  config_warnings: [],
  tools: {
    ok: true,
    items: [{ name: "ffmpeg", required: true, path: "/usr/bin/ffmpeg", version: "ffmpeg 8" }],
    capabilities: { svtav1: true, opus: true, vidstab: false },
  },
  api_key_source: "none",
};

const summary: ConfigSummary = {
  path: status.config_path,
  valid: true,
  error: null,
  warnings: [],
  sources: [
    {
      id: "camera",
      path: "~/Captures/Camera",
      server_path: null,
      recursive: false,
      skip: [".stfolder"],
      ai_prefix: "Family archive.",
      ai_suffix: null,
    },
  ],
  targets: [
    {
      name: "Screenshots",
      kind: "folders",
      ai: false,
      filters: [{ sources: ["camera"], match: "Screenshot_*", exclude: null, ai: null, conditions: [] }],
      outputs: [{ dir: "~/Shots", server_dir: null, content: [], rename: [{ preset: "capture-date", mask: null }] }],
    },
  ],
  presets: { name: [], content: [], masks: [] },
  ai: { model: "m", effort: "low", jobs: 8 },
};

const builtins: PresetSet = {
  name: [{ name: "capture-date", kind: "name", description: "by capture date", mask: "*", mask_expanded: "*", params: {} }],
  content: [
    { name: "avif-high", kind: "photo", description: "AVIF", mask: "@photo-original", mask_expanded: "*.jpg", params: {} },
  ],
  masks: [{ name: "photo", source: "*.jpg", expanded: "*.jpg" }],
};

const outcome: EditOutcome = {
  version: "v1",
  changed: false,
  saved: false,
  error: null,
  warnings: [],
  renames: [],
  draft: {
    sources: [
      {
        id: "camera",
        path: "~/Captures/Camera",
        server_path: null,
        recursive: false,
        skip: [".stfolder"],
        ai_prefix: "Family archive.",
        ai_prefix_file: null,
        ai_suffix: null,
        ai_suffix_file: null,
      },
    ],
    targets: [
      {
        name: "Screenshots",
        kind: "folders",
        filters: [{ sources: ["camera"], match: ["Screenshot_*"], exclude: [], ai: null, expand: null, conditions: {} }],
        outputs: [{ dir: "~/Shots", server_dir: null, content: [], rename: [{ preset: "capture-date", mask: null }] }],
      },
    ],
    masks: [],
  },
};

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>(async (cmd) => {
  switch (cmd) {
    case "current_plan":
      return null;
    case "refresh_current":
    case "scan_status":
      return true;
    case "pending_status":
      return null;
    case "check_pending":
      throw new Error("no check in this test");
    case "app_status":
      return status;
    case "load_config":
      return summary;
    case "builtin_presets":
      return builtins;
    case "list_reports":
      return [];
    case "config_draft":
      return outcome;
    case "preset_limits":
      return [];
    default:
      throw new Error(`unexpected ${cmd}`);
  }
});

vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: async () => () => {} }));

const { default: App } = await import("./App");

describe("app shell", () => {
  beforeEach(() => {
    localStorage.clear();
    invoke.mockClear();
  });

  it("lists the screens in the nav, the main flow first", () => {
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "Sections" });
    const items = within(nav).getAllByRole("button").map((b) => b.textContent);
    expect(items).toEqual(["Sort", "Sources", "Targets", "Presets", "History", "Settings"]);
  });

  it("opens the sorting flow by default and switches screens", async () => {
    const user = userEvent.setup();
    history.replaceState(null, "", "#");
    render(<App />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Sort");

    const nav = screen.getByRole("navigation", { name: "Sections" });
    await user.click(within(nav).getByRole("button", { name: "Sources" }));
    expect(await screen.findByRole("button", { name: "camera" })).toBeInTheDocument();
    await user.click(within(nav).getByRole("button", { name: "Targets" }));
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Targets");
    expect(within(nav).getByRole("button", { name: "Targets" })).toHaveAttribute("aria-current", "page");
    await user.click(screen.getByRole("button", { name: /Screenshots/ }));
    expect(screen.getByText("capture-date")).toBeInTheDocument();

    await user.click(within(nav).getByRole("button", { name: "Presets" }));
    // In the list and in the inspector (the first item is selected).
    expect((await screen.findAllByText("avif-high")).length).toBeGreaterThan(0);

    await user.click(within(nav).getByRole("button", { name: "History" }));
    expect(await screen.findByText("No runs yet.")).toBeInTheDocument();
  });

  it("shows tools and AI key in the status bar", async () => {
    render(<App />);
    expect(await screen.findByText("Tools 1 of 1")).toBeInTheDocument();
    expect(screen.getByText("No AI key")).toBeInTheDocument();
    expect(screen.getByText("Config OK")).toBeInTheDocument();
  });
});

describe("screen in the URL hash", () => {
  it("opens the screen named in the hash and records navigation", async () => {
    const user = userEvent.setup();
    history.replaceState(null, "", "#presets");
    render(<App />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Presets");
    await user.click(screen.getByRole("button", { name: "Sort" }));
    expect(location.hash).toBe("#flow");
    history.replaceState(null, "", "#");
  });
});
