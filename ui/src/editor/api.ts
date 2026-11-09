// Tauri commands of the config editor (crates/gallery-gui/src/config_edit).
import { invoke } from "@tauri-apps/api/core";
import { t } from "../i18n";
import type { EditFailure, EditOp, EditOutcome, MaskTestResult, NamePresetDraft, PromptPreview } from "./model";
import type { Limit, NameResult } from "./presets";

export const editorApi = {
  /// `version`: the file version the operations are made for (null: any).
  draft: (path: string | null, version: string | null, ops: EditOp[]) =>
    invoke<EditOutcome>("config_draft", { path, version, ops }),
  save: (path: string | null, version: string, ops: EditOp[]) =>
    invoke<EditOutcome>("config_edit", { path, version, ops }),
  /// Move manual decisions from a renamed target's old name to the new one.
  /// `renames`: the whole [from, to] mapping, applied at once; refused if the config is no longer `version`.
  retargetOverrides: (path: string | null, version: string, renames: [string, string][]) =>
    invoke<number>("config_retarget_overrides", { path, version, renames }),
  previewPrompt: (path: string | null, ops: EditOp[], sourceId: string) =>
    invoke<PromptPreview>("config_preview_prompt", { path, ops, sourceId }),
  maskTest: (path: string | null, ops: EditOp[], test: { match: string[] | null; exclude: string[]; names: string[] }) =>
    invoke<MaskTestResult[]>("config_mask_test", { path, ops, test }),
  /// What a name preset (a name, or the form's draft) makes of sample names with a fake date.
  previewName: (
    path: string | null,
    ops: EditOp[],
    sample: { preset: string | NamePresetDraft; names: string[]; date: string | null; source: string | null },
  ) => invoke<NameResult[]>("config_preview_name", { path, ops, sample }),
  presetLimits: () => invoke<Limit[]>("preset_limits"),
};

/// A rejected editor command as a failure (the commands reject with one;
/// anything else is an internal error).
export function asFailure(e: unknown): EditFailure {
  if (e && typeof e === "object" && "kind" in e && "message" in e) return e as EditFailure;
  return { kind: "io", message: typeof e === "string" ? e : e instanceof Error ? e.message : String(e), op: null };
}

/// Pick a folder with the native dialog; null if cancelled.
export async function pickFolder(current: string | null, title: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ title, directory: true, multiple: false, defaultPath: current ?? undefined });
  return typeof picked === "string" ? picked : null;
}

const MEDIA_EXTENSIONS = [
  "jpg", "jpeg", "png", "heic", "heif", "avif", "webp", "tif", "tiff", "dng", "cr2", "cr3", "nef", "arw", "orf", "raf", "rw2",
  "mp4", "mov", "m4v", "3gp", "mkv", "avi", "webm",
];

/// Pick a photo or video (to take its GPS); null if cancelled.
export async function pickMediaFile(title: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    title,
    directory: false,
    multiple: false,
    filters: [{ name: t("dialog_media_filter"), extensions: MEDIA_EXTENSIONS }],
  });
  return typeof picked === "string" ? picked : null;
}

/// GPS of a photo or video; rejects with a message when it has no fix.
export const gpsOf = (path: string) => invoke<{ lat: number; lon: number }>("gps_of", { path });

/// Pick a text file (an AI prompt part); null if cancelled.
export async function pickTextFile(current: string | null, title: string): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    title,
    directory: false,
    multiple: false,
    defaultPath: current ?? undefined,
    filters: [{ name: t("dialog_text_filter"), extensions: ["md", "txt"] }],
  });
  return typeof picked === "string" ? picked : null;
}
