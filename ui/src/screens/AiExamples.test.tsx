import { render, screen, within, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { SourceExamples } from "../api";

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));

const { AiExamples } = await import("./AiExamples");

const item = (id: string, file: string, description: string, choice: string | null = "Receipts") => ({
  id,
  file,
  description,
  ai_suggested: "Photos",
  user_choice: choice,
  at: "t",
});

let data: SourceExamples[];
describe("AI examples in settings", () => {
  beforeEach(() => {
    invoke.mockReset();
    data = [
      {
        source_id: "cam",
        count: 2,
        items: [item("b", "b.jpg", "chat screenshot", null), item("a", "a.jpg", "receipt from Lidl")],
      },
      { source_id: "phone", count: 1, items: [item("c", "c.jpg", "meme")] },
    ];
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "list_ai_examples") return data;
      if (cmd === "remove_ai_example") {
        const id = (args as { id: string }).id;
        data = data
          .map((g) => {
            const items = g.items.filter((i) => i.id !== id);
            return { ...g, items, count: items.length };
          })
          .filter((g) => g.count > 0);
        return true;
      }
      if (cmd === "clear_ai_examples") {
        const s = (args as { sourceId: string | null }).sourceId;
        data = s === null ? [] : data.filter((g) => g.source_id !== s);
        return 1;
      }
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("shows the count per source and each example as a line and says they only affect new requests", async () => {
    render(<AiExamples />);
    const cam = (await screen.findByText("cam")).closest(".ai-examples-source") as HTMLElement;
    expect(within(cam).getByText(/— 2/)).toBeInTheDocument();
    expect(within(cam).getByText(/chat screenshot — AI chose Photos, correct: leave in place/)).toBeInTheDocument();
    expect(within(cam).getByText(/receipt from Lidl — AI chose Photos, correct: Receipts/)).toBeInTheDocument();
    expect(screen.getByText(/Examples affect new AI requests/)).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("estimate_ai_scan", expect.anything());
  });

  it("deletes one example, clears a source and clears everything", async () => {
    const user = userEvent.setup();
    render(<AiExamples />);
    await user.click(await screen.findByRole("button", { name: "Delete example a.jpg: receipt from Lidl" }));
    expect(invoke).toHaveBeenCalledWith("remove_ai_example", { id: "a" });
    await waitFor(() => expect(screen.queryByText(/receipt from Lidl/)).not.toBeInTheDocument());

    await user.click(await screen.findByRole("button", { name: "Clear “phone”" }));
    expect(invoke).toHaveBeenCalledWith("clear_ai_examples", { sourceId: "phone" });
    await waitFor(() => expect(screen.queryByText("phone")).not.toBeInTheDocument());

    await user.click(await screen.findByRole("button", { name: "Clear" }));
    expect(invoke).toHaveBeenCalledWith("clear_ai_examples", { sourceId: null });
    expect(await screen.findByText("No corrections yet.")).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });
});
