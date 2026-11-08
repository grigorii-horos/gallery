import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, beforeEach } from "vitest";
import { applyLanguageSetting } from "./i18n";

// Every test starts in English, whatever an earlier one switched to.
beforeEach(async () => {
  localStorage.removeItem("gallery.language");
  await applyLanguageSetting("en");
});

afterEach(cleanup);
