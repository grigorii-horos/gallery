import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PendingState } from "../tray/usePending";

const invoke = vi.fn<(cmd: string, args?: unknown) => Promise<unknown>>();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string, args?: unknown) => invoke(cmd, args) }));

const { SettingsScreen } = await import("../screens/SettingsScreen");
const { Nav } = await import("../components/Nav");
const { languageSetting } = await import("./index");

const view = {
  settings: { check_interval_min: 30, notify_threshold: 1, hide_to_tray: true, language: "system" },
  autostart: false,
};
const pending: PendingState = { snapshot: null, checking: false, error: null, checkNow: vi.fn(async () => {}) };

function Screen() {
  return (
    <>
      <Nav current="settings" onSelect={() => {}} />
      <SettingsScreen pending={pending} />
    </>
  );
}

describe("switching the language live", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (cmd, args) => {
      if (cmd === "tray_settings") return view;
      if (cmd === "list_ai_examples") return [];
      if (cmd === "set_locale") {
        const language = (args as { language: string }).language;
        return { setting: language, locale: language, system: "en" };
      }
      throw new Error(`unexpected ${cmd}`);
    });
  });

  it("lists system and the ten languages by their own names", async () => {
    render(<Screen />);
    const picker = await screen.findByLabelText("Language");
    const names = [...picker.querySelectorAll("option")].map((o) => o.textContent);
    expect(names).toEqual([
      "System (English)",
      "English",
      "Русский",
      "Українська",
      "Deutsch",
      "Français",
      "Español",
      "Português (Brasil)",
      "Italiano",
      "Polski",
      "简体中文",
    ]);
  });

  it("changes the UI texts at once and tells the backend", async () => {
    const user = userEvent.setup();
    render(<Screen />);
    const picker = await screen.findByLabelText("Language");
    expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument();
    await user.selectOptions(picker, "Русский");
    await waitFor(() => expect(screen.getByRole("button", { name: "Настройки" })).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Сохранить" })).toBeInTheDocument();
    expect(screen.getByLabelText("Язык")).toHaveValue("ru");
    expect(invoke).toHaveBeenCalledWith("set_locale", { language: "ru" });
    expect(languageSetting()).toBe("ru");
    // And back, then to a language with another script.
    await user.selectOptions(screen.getByLabelText("Язык"), "简体中文");
    await waitFor(() => expect(screen.getByRole("button", { name: "设置" })).toBeInTheDocument());
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("'System' follows the operating system and is remembered as such", async () => {
    const user = userEvent.setup();
    render(<Screen />);
    const picker = await screen.findByLabelText("Language");
    await user.selectOptions(picker, "Deutsch");
    await waitFor(() => expect(screen.getByRole("button", { name: "Einstellungen" })).toBeInTheDocument());
    await user.selectOptions(screen.getByLabelText("Sprache"), "system");
    // jsdom's navigator language is English.
    await waitFor(() => expect(screen.getByRole("button", { name: "Settings" })).toBeInTheDocument());
    expect(invoke).toHaveBeenLastCalledWith("set_locale", { language: "system" });
    expect(languageSetting()).toBe("system");
  });
});
