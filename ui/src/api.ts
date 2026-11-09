// Typed wrappers over the Tauri commands of crates/gallery-gui.
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";

export type ApiKeySource = "env" | "file" | "none";

export interface ToolInfo {
  name: string;
  required: boolean;
  path: string | null;
  version: string | null;
}

export interface AppStatus {
  config_path: string;
  config_ok: boolean;
  config_error: string | null;
  config_warnings: string[];
  tools: {
    ok: boolean;
    items: ToolInfo[];
    capabilities: { svtav1: boolean; opus: boolean; vidstab: boolean } | null;
  };
  api_key_source: ApiKeySource;
}

export interface SourceSummary {
  id: string;
  path: string;
  server_path: string | null;
  recursive: boolean;
  skip: string[];
  ai_prefix: string | null;
  ai_suffix: string | null;
}

export interface FilterSummary {
  sources: string[];
  match: string | null;
  exclude: string | null;
  /** AI category description. */
  ai: string | null;
  /** Keys of its other conditions (`date`, `near`, `expand`, ...). */
  conditions: string[];
}

export interface StepSummary {
  preset: string;
  mask: string | null;
}

export interface OutputSummary {
  dir: string;
  server_dir: string | null;
  content: StepSummary[];
  rename: StepSummary[];
}

/** What a target does with the files it takes. */
export type TargetKind = "folders" | "trash" | "keep";

export interface TargetSummary {
  name: string;
  kind: TargetKind;
  /** Some filter asks the AI. */
  ai: boolean;
  filters: FilterSummary[];
  outputs: OutputSummary[];
}

export interface PresetInfo {
  name: string;
  kind: string;
  description: string;
  mask: string;
  mask_expanded: string;
  params: Record<string, unknown>;
}

export interface MaskInfo {
  name: string;
  source: string;
  expanded: string;
}

export interface PresetSet {
  name: PresetInfo[];
  content: PresetInfo[];
  masks: MaskInfo[];
}

export interface ConfigSummary {
  path: string;
  valid: boolean;
  error: string | null;
  warnings: string[];
  sources: SourceSummary[];
  /** In order: the first target with a matching filter takes a file. */
  targets: TargetSummary[];
  presets: PresetSet;
  ai: { model: string; effort: string; jobs: number };
}

const arg = (path: string | null) => ({ path: path ?? null });

export const api = {
  appStatus: (path: string | null) => invoke<AppStatus>("app_status", arg(path)),
  loadConfig: (path: string | null) => invoke<ConfigSummary>("load_config", arg(path)),
  builtinPresets: () => invoke<PresetSet>("builtin_presets"),
};

/// Pick a TOML config with the native dialog; null if cancelled.
export async function pickConfig(current: string | null): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    title: t("dialog_open_config"),
    multiple: false,
    directory: false,
    defaultPath: current ?? undefined,
    filters: [{ name: t("dialog_config_filter"), extensions: ["toml"] }],
  });
  return typeof picked === "string" ? picked : null;
}

/// Error text of a rejected command (Tauri rejects with the string the command returned).
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

// ---------- plan screen ----------

export type ScanStage = "scan" | "exif" | "ai" | "plan" | "apply" | "undo";

export interface ScanProgress {
  stage: ScanStage;
  done: number;
  /** 0 while not known (the scan stage). */
  total: number;
  current: string | null;
}

export interface AiCost {
  input: number;
  output: number;
  usd: number | null;
}

export interface ScanSummary {
  files: number;
  targets: Record<string, number>;
  review: number;
  unmatched: number;
  unchanged: number;
  /** Files keep targets take (also counted in `targets`). */
  kept: number;
  warnings: number;
  notes: string[];
  overrides_applied: number;
  ai: AiCost | null;
}

export interface ScanDone {
  plan_path: string;
  summary: ScanSummary;
}

export interface ScanError {
  message: string;
  cancelled: boolean;
}

/** The running scan, as `scan_status` tells it: nothing to choose, the scan is always the explicit AI one. */
export type ScanRequest = Record<string, never>;

export type Via =
  | { kind: "mask" }
  | { kind: "ai"; reason: string; low?: boolean }
  | { kind: "manual" };

export interface TraceStep {
  index: number;
  preset: string;
  matched: boolean;
  after: string;
  /** Absent in plans written before it was recorded. */
  pipe?: "content" | "name";
  /** The bundle this content step was expanded from. */
  bundle?: string;
}

export type OutputOp = "write" | "rename" | "keep";

export interface PlannedOutput {
  dest: string;
  op?: OutputOp;
  steps: string[];
  trace: TraceStep[];
}

export interface PlanItem {
  source: string;
  source_root: string;
  size: number;
  mtime_ns: number;
  target: string;
  via: Via;
  /** What the target does; absent (folders) in version-1 plans. A `trash` item has no outputs. */
  kind?: "folders" | "trash" | "keep";
  outputs: PlannedOutput[];
  /** Capture time, `YYYY-MM-DDTHH:MM:SS` local, on `trash` items; absent when unknown or in older plans. */
  captured?: string | null;
}

export interface ReviewItem {
  source: string;
  size: number;
  mtime_ns: number;
  suggested: string;
  reason: string;
  /** Capture time, `YYYY-MM-DDTHH:MM:SS` local; absent when unknown or in older plans. */
  captured?: string | null;
  /** Root of the source the file came from; absent in older plans. */
  source_root?: string | null;
  /** Held back because its name could not be built, not by the AI; absent in older plans. */
  name_error?: boolean;
  /** Held because a `series` condition matched: the whole series and the suggested best shot. */
  series?: { members: string[]; best: string } | null;
}

export interface OverrideResult {
  source_path: string;
  /** The path the decision is stored under; null when refused. */
  key: string | null;
  /** File size and mtime the decision was stamped with; null when refused. */
  size: number | null;
  mtime_ns: number | null;
  error: string | null;
}

export type PlanWarning =
  | { kind: "series_split"; a: string; b: string }
  | { kind: "double_encode"; source: string; dest: string }
  | { kind: "missing_server_path"; what: string };

export interface Plan {
  version: number;
  created: string;
  side: "local" | "server";
  tz?: string | null;
  items: PlanItem[];
  review: ReviewItem[];
  unmatched: string[];
  warnings: PlanWarning[];
  unchanged?: string[];
  /** Files `keep` targets take: left untouched by a run. */
  kept?: { source: string; target: string }[];
}

export type OverrideAction = { kind: "target"; name: string } | { kind: "keep" } | { kind: "series_best" };

/** A review file as the plan saw it: a decision is refused if the file changed since. */
export interface ReviewMember {
  source_path: string;
  size: number;
  mtime_ns: number;
}

/** A stored decision with the file state it was made for; the core ignores it once the file differs. */
export interface OverrideView {
  path: string;
  action: OverrideAction;
  size: number;
  mtime_ns: number;
}

export interface PlanView {
  path: string;
  plan: Plan;
  overrides: OverrideView[];
}

/** The one current plan of the config: always built from the present files, config and decisions. */
export interface CurrentView extends PlanView {
  /** RFC 3339 time of the build; the UI never shows it as "plan from…". */
  built_at: string;
  /** Files that wait for a fresh AI answer (left out of the plan). */
  waiting_ai: number;
  notes: string[];
  /** Identity of this plan: a run names the plan the user confirmed. */
  plan_id: string;
}

// ---------- "Why this file?" ----------

export type ExplainScope =
  | { kind: "scanned"; source: string }
  | { kind: "missing" }
  | { kind: "not_file" }
  | { kind: "no_source" }
  | { kind: "hidden"; source: string; part: string }
  | { kind: "skip"; source: string; part: string; glob: string }
  | { kind: "not_recursive"; source: string };

export interface ItemCheck {
  item: string;
  negated: boolean;
  hit: boolean;
  /** A reference expanded; empty for a plain pattern. */
  expanded: string;
  children: ItemCheck[];
  /** Russian phrase from the core: why the item did or did not take the name. */
  reason: string;
}

export interface MaskCheck {
  source: string;
  expanded: string;
  matched: boolean;
  items: ItemCheck[];
}

export interface FilterCheck {
  index: number;
  sources: string[];
  source_ok: boolean;
  matches: MaskCheck | null;
  exclude: MaskCheck | null;
  fit: "match" | "excluded" | "no";
  /** The filter asks the AI (its masks are the AI's prefilter). */
  ai: boolean;
}

/** One target of the ordered list, in list order. */
export interface TargetCheck {
  kind: "folders" | "trash" | "keep";
  /** Some filter of the target asks the AI. */
  ai: boolean;
  name: string;
  filters: FilterCheck[];
  candidate: boolean;
  taken: boolean;
  /** Why no filter takes the file (first miss); absent when one does. */
  miss?: { condition: string; detail: string } | null;
}

export interface ExplainDecision {
  kind: "target" | "review" | "excluded" | "kept" | "unmatched";
  target: string | null;
  via: { kind: "mask" } | { kind: "ai"; reason: string; low?: boolean } | { kind: "manual" } | null;
  detail: string | null;
}

export type ExplainAi =
  | { kind: "no_candidates" }
  | { kind: "not_reached"; candidates: string[] }
  | { kind: "not_asked"; candidates: string[] }
  | { kind: "cached"; category: string | null; reason: string; low: boolean };

export interface StepCheck {
  index: number;
  preset: string;
  mask: string;
  mask_expanded: string;
  matched: boolean;
  /** Content steps: the bundle this step was expanded from. */
  bundle?: string | null;
  before: string;
  after: string;
  skipped: string | null;
}

export interface OutputCheck {
  content: StepCheck[];
  rename: StepCheck[];
  dest: string;
  op: OutputOp;
  /** Kept as it is: the content pipe did not make the file smaller before. */
  incompressible?: boolean;
}

export interface Explanation {
  path: string;
  scope: ExplainScope;
  targets: TargetCheck[];
  /** Names of targets that do not list the file's source. */
  other_targets: string[];
  ai: ExplainAi;
  decision: ExplainDecision | null;
  outcome: "process" | "unchanged" | "trash" | "kept" | "review" | "unmatched" | "not_scanned";
  outputs: OutputCheck[];
  verdict: string;
}

/** Low / median / high of a figure. */
export interface Range {
  low: number;
  mid: number;
  high: number;
}

export interface TargetEstimate {
  /** Empty for the plan's total. */
  name: string;
  files: number;
  input_bytes: number;
  /** Bytes that exist afterwards (new files, plus sources that stay). */
  output_bytes: Range;
  /** Input minus output; negative when a run adds more than it frees. */
  saving_bytes: Range;
  seconds: Range;
  trash: number;
  renamed: number;
  kept: number;
}

/** What a run of a plan would save and take (ranges, learned from past reports). */
export interface Estimate {
  targets: TargetEstimate[];
  total: TargetEstimate;
  jobs: number;
  source: { kind: "history"; samples: number; guessed_files: number } | { kind: "defaults" };
  /** "from past runs (N files)" or "rough estimate". */
  source_label: string;
}

/** What a scan with AI would cost before it starts. */
export interface AiEstimate {
  candidates: number;
  cached: number;
  to_send: number;
  model: string;
  /** USD; null for a model without a known price. */
  usd: Range | null;
  source_label: string;
  /** "≈ $X (Y files, Z from cache)". */
  label: string;
  /** More than $1: ask before starting. */
  confirm: boolean;
}

export const planApi = {
  /** Read-only: files an AI scan would send, from the cache, and the price. */
  estimateAiScan: (path: string | null) => invoke<AiEstimate>("estimate_ai_scan", { path: path ?? null }),
  /** Read-only: the core's account of what a scan does with the file at `path`. */
  explainFile: (configPath: string | null, path: string) =>
    invoke<Explanation>("explain_file", { configPath: configPath ?? null, path }),
  startScan: (path: string | null) => invoke<void>("start_scan", { path: path ?? null }),
  cancelScan: () => invoke<boolean>("cancel_scan"),
  /** The stored current plan, null before the first build. Never builds. */
  currentPlan: (path: string | null) => invoke<CurrentView | null>("current_plan", { path: path ?? null }),
  /** Bring the current plan up to date in the background (no AI); false: another operation holds the slot. */
  refreshCurrent: (path: string | null) => invoke<boolean>("refresh_current", { path: path ?? null }),
  thumb: (path: string) => invoke<string>("thumb", { path }),
  /** The running scan's request, `null` when none runs. */
  scanStatus: () => invoke<ScanRequest | null>("scan_status"),
  /** Checked against the config at `path`; returns the key the decision is stored under and its stamp. */
  setOverride: (path: string | null, sourcePath: string, action: OverrideAction) =>
    invoke<OverrideResult>("set_override", { path: path ?? null, sourcePath, action }),
  /** Several decisions in one write; one result per item, in order. */
  setOverrides: (path: string | null, items: { source_path: string; action: OverrideAction }[]) =>
    invoke<OverrideResult[]>("set_overrides", { path: path ?? null, items }),
  /** Series review: `best` is marked, the other members go to `target`; one result per member. */
  confirmSeries: (path: string | null, members: ReviewMember[], best: string, target: string) =>
    invoke<OverrideResult[]>("confirm_series", { path: path ?? null, members, best, target }),
  /** One decision for every listed file of an event; one result per member. */
  decideEvent: (path: string | null, members: ReviewMember[], action: OverrideAction) =>
    invoke<OverrideResult[]>("decide_event", { path: path ?? null, members, action }),
  removeOverride: (sourcePath: string) => invoke<boolean>("remove_override", { sourcePath }),
};

// ---------- run screen ----------

/** What a run of a plan would do, counted from the plan alone. */
export interface ApplyPreview {
  plan_path: string;
  /** Identity of the plan file these counts are of: a run confirmed from them names it. */
  plan_id: string;
  created: string;
  side: "local" | "server" | null;
  files: number;
  /** Sources that go to the trash once their outputs are in place. */
  trash: number;
  /** Sources renamed in place (or moved inside their source root): never trashed. */
  renamed: number;
  /** Sources an output keeps where they are. */
  kept: number;
  /** Files of trash targets: no copies, they go to the trash after every placement of the run. */
  to_trash: number;
  /** Files keep targets take: outside the run, untouched. */
  kept_targets: number;
  /** New files written. */
  writes: number;
  review: number;
  unmatched: number;
  unchanged: number;
  /** A run of this plan already finished (a report, not cancelled, is next to it). */
  applied: "clean" | "with_failures" | "undone" | null;
}

export interface ApplyRequest {
  plan_path: string;
  dry_run: boolean;
  jobs?: number | null;
  /** Place outputs that differ from their source instead of failing them. */
  allow_anomalies?: boolean;
  /** Pause the local Syncthing folders the plan touches until the user resumes them. */
  pause_sync?: boolean;
}

/** What pausing Syncthing for a plan would do (`sync_check`). */
export interface SyncCheck {
  reachable: boolean;
  reason: string | null;
  /** Labels of the folders the plan touches that are not paused yet. */
  folders: string[];
}

/** What Gallery paused and has not resumed (the state file). */
export interface PauseState {
  folders: { id: string; label: string }[];
  plan: string;
  report: string;
  paused_at: string;
}

export interface ResumeOutcome {
  resumed: string[];
  failed: string[];
}

export const syncApi = {
  /** `pilot`: files per target when only the pilot slice runs. */
  check: (planPath: string, pilot?: number) => invoke<SyncCheck>("sync_check", { planPath, pilot: pilot ?? null }),
  paused: () => invoke<PauseState | null>("sync_paused"),
  resume: () => invoke<ResumeOutcome>("sync_resume"),
};

export interface ApplyProgress {
  done: number;
  total: number;
  current: string | null;
  failed: number;
  skipped: number;
}

export interface ApplyStatus {
  request: ApplyRequest;
  progress: ApplyProgress;
}

/** The running apply and the last finished one (kept for a reloaded webview). */
export interface ApplyState {
  running: ApplyStatus | null;
  last_done: ApplyDone | null;
}

export interface ApplySummary {
  total: number;
  done: number;
  renamed: number;
  failed: number;
  stale: number;
  missing: number;
  not_started: number;
  cancelled: boolean;
  /** The run stopped itself: too many outputs differed from their source. */
  stopped_by_anomalies?: boolean;
  /** Files with an output that differed from its source. */
  anomalies?: number;
}

export interface ApplyDone {
  plan_path: string;
  dry_run: boolean;
  /** null for a dry run: it writes no report. */
  report_path: string | null;
  summary: ApplySummary;
  failures: { source: string; reason: string }[];
  warnings: string[];
}

export interface ApplyError {
  message: string;
}

export interface ExportRequest {
  dest_dir: string;
}

export interface ExportDone {
  plan_path: string;
  file_name: string;
  command: string;
  summary: ScanSummary;
}

/** How the current plan splits for a pilot (nothing is written). */
export interface PilotSplit {
  /** Identity of the plan and of the chosen slice: the run names them. */
  plan_id: string;
  pilot_id: string;
  per_target: number;
  pilot_files: number;
  rest_files: number;
  /** What the pilot run would do, for its confirmation. */
  preview: ApplyPreview;
}

/** What a run of the current plan asks for. */
export interface CurrentRun {
  dry_run: boolean;
  jobs?: number | null;
  allow_anomalies?: boolean;
  pause_sync?: boolean;
  /** A pilot of this many files per target instead of the whole plan. */
  pilot?: number | null;
  /** The plan (and pilot slice) the user confirmed; another one is refused. */
  plan_id?: string | null;
  pilot_id?: string | null;
}

export interface PilotFacts {
  size: number | null;
  /** Local time, YYYY-MM-DD HH:MM:SS. */
  mtime: string | null;
  capture: string | null;
  /** null: not readable. */
  gps: boolean | null;
}

export interface PilotOutput {
  planned: string;
  actual: string | null;
  error: string | null;
  anomalies: string[];
  after: PilotFacts;
}

export interface PilotFile {
  source: string;
  target: string;
  status: "done" | "failed" | "stale" | "missing";
  error: string | null;
  before: PilotFacts;
  outputs: PilotOutput[];
}

export interface PilotResult {
  report_path: string;
  files: PilotFile[];
  anomalies: number;
  stopped_by_anomalies: boolean;
}

export const runApi = {
  /** Read-only: saving and time ranges of a run of the plan. */
  estimatePlan: (planPath: string, pilot?: number) =>
    invoke<Estimate>("estimate_plan", { planPath, pilot: pilot ?? null }),
  pilotPreview: (planPath: string, perTarget: number) =>
    invoke<PilotSplit>("pilot_preview", { planPath, perTarget }),
  /** Runs the current plan after a final freshness check (a stale plan is refused and rebuilt). */
  startApplyCurrent: (path: string | null, run: CurrentRun) =>
    invoke<void>("start_apply_current", { path: path ?? null, run }),
  pilotResult: (reportPath: string) => invoke<PilotResult>("pilot_result", { reportPath }),
  applyPreview: (planPath: string) => invoke<ApplyPreview>("apply_preview", { planPath }),
  /** A plan file (a retry plan from History); the current plan runs with `startApplyCurrent`. */
  startApply: (request: ApplyRequest) => invoke<void>("start_apply", { request }),
  cancelApply: () => invoke<boolean>("cancel_apply"),
  applyStatus: () => invoke<ApplyState>("apply_status"),
  exportServerPlan: (path: string | null, request: ExportRequest) =>
    invoke<ExportDone>("export_server_plan", { path: path ?? null, request }),
  cancelExport: () => invoke<boolean>("cancel_export"),
};

/// Pick the export folder with the native dialog; null if cancelled.
export async function pickFolder(current: string | null): Promise<string | null> {
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({
    title: t("server_pick_title"),
    multiple: false,
    directory: true,
    defaultPath: current ?? undefined,
  });
  return typeof picked === "string" ? picked : null;
}

// ---------- history ----------

export type Side = "local" | "server";

/** One report in the History list. */
export interface ReportEntry {
  path: string;
  file_name: string;
  /** The plan next to the report (the local copy for a server report). */
  plan_path: string;
  plan_exists: boolean;
  side: Side;
  run_id: string;
  started: string;
  /** Empty: the run never finished. */
  finished: string;
  /** An earlier run of the plan, kept under its run id. */
  archived: boolean;
  summary: ApplySummary;
  undone: boolean;
  partially_undone: boolean;
  undone_items: number;
  /** The report could not be read. */
  error: string | null;
}

export interface OutputView {
  planned: string;
  actual: string | null;
  error: string | null;
  warnings: string[];
  renamed_from: string | null;
}

export type ItemStatus = "done" | "failed" | "stale" | "missing" | "dry_run";

export interface ItemView {
  source: string;
  status: ItemStatus;
  trashed: string | null;
  renamed: boolean;
  undone: boolean;
  /** The plan's target that routed the file; null when the plan is gone. */
  target: string | null;
  /** What undoing just this file would do (items 0: nothing to undo). */
  undo: UndoPreview;
  error: string | null;
  outputs: OutputView[];
}

/** What an undo would do with the items not undone yet. */
export interface UndoPreview {
  items: number;
  restore: number;
  rename_back: number;
  remove_outputs: number;
}

export interface ReportDetail {
  entry: ReportEntry;
  items: ItemView[];
  warnings: string[];
  undo: UndoPreview;
  /** Server reports: the command that undoes the run on the server. */
  server_command: string | null;
}

export interface UndoProgress {
  done: number;
  total: number;
  current: string | null;
}

export interface UndoStatus {
  report_path: string;
  progress: UndoProgress;
}

export interface UndoDone {
  report_path: string;
  restored: number;
  removed_outputs: number;
  problems: string[];
  cancelled: boolean;
  undone: boolean;
}

export interface RetryDone {
  report_path: string;
  /** The saved retry plan; null when no failed file could be retried. */
  plan_path: string | null;
  failed: number;
  files: number;
  left_out: number;
  preview: ApplyPreview | null;
  /** Prepared before and not run yet: given back as it is. */
  reused: boolean;
}

export const historyApi = {
  listReports: (exportDir: string | null) => invoke<ReportEntry[]>("list_reports", { exportDir }),
  loadReport: (path: string, exportDir: string | null) => invoke<ReportDetail>("load_report", { path, exportDir }),
  startUndo: (reportPath: string) => invoke<void>("start_undo", { reportPath }),
  undoFiles: (reportPath: string, sources: string[]) => invoke<void>("undo_files", { reportPath, sources }),
  undoTarget: (reportPath: string, target: string) => invoke<void>("undo_target", { reportPath, target }),
  cancelUndo: () => invoke<boolean>("cancel_undo"),
  undoStatus: () => invoke<UndoStatus | null>("undo_status"),
  startRetry: (reportPath: string) => invoke<RetryDone>("start_retry", { reportPath }),
};

/** One of the user's corrections the AI sees as an example. */
export interface AiExample {
  id: string;
  file: string;
  /** The AI's reason: the description of the example. */
  description: string;
  /** The AI's category; null when it had none. */
  ai_suggested: string | null;
  /** The user's target; null: "leave in place". */
  user_choice: string | null;
  at: string;
}

export interface SourceExamples {
  source_id: string;
  count: number;
  /** Newest first. */
  items: AiExample[];
}

export const examplesApi = {
  list: () => invoke<SourceExamples[]>("list_ai_examples"),
  remove: (id: string) => invoke<boolean>("remove_ai_example", { id }),
  /** All examples, or those of one source; how many were deleted. */
  clear: (sourceId: string | null) => invoke<number>("clear_ai_examples", { sourceId }),
};

/** Tray and notification settings (crates/gallery-gui `settings`). */
export interface TraySettings {
  /** `"system"` or a supported language code; changed through `localeApi`. */
  language: string;
  check_interval_min: number;
  notify_threshold: number;
  hide_to_tray: boolean;
}

export interface TrayView {
  settings: TraySettings;
  /** Whether the XDG autostart entry exists. */
  autostart: boolean;
}

/** The result of a background check of how much work waits. */
export interface PendingSnapshot {
  to_process: number;
  need_ai: number;
  total: number;
  by_target: Record<string, number>;
  checked_at: string;
  error: string | null;
}

/** The interface language as the backend holds it. */
export interface LocaleView {
  /** `"system"` or a supported code. */
  setting: string;
  /** The supported code in force. */
  locale: string;
  /** The supported code the system language maps to. */
  system: string;
}

export const localeApi = {
  get: () => invoke<LocaleView>("get_locale"),
  /** Switches the backend (messages, tray, notifications) and stores the choice. */
  set: (language: string) => invoke<LocaleView>("set_locale", { language }),
};

export const trayApi = {
  settings: () => invoke<TrayView>("tray_settings"),
  setSettings: (settings: TraySettings, autostart: boolean) =>
    invoke<TrayView>("set_tray_settings", { settings, autostart }),
  pendingStatus: () => invoke<PendingSnapshot | null>("pending_status"),
  /** Counts now for this config (the background check follows it). */
  checkPending: (path: string | null) => invoke<PendingSnapshot>("check_pending", { path: path ?? null }),
};
